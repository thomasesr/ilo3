#!/usr/bin/env python3
"""HP iLO LZSS decompressor (iLO3/4/5 ROM lineage).

LZSS, 0x1000-byte sliding window. Control byte, bits MSB-first:
  1 -> literal: copy next raw byte
  0 -> back-ref (2 bytes b0 b1): length=(b0>>4)+3,
       distance = wpos - (b1 | ((b0 & 0xf) << 8)) - 1

Two implementations are provided and cross-checked:
  * decompress_stream  - continuous single pass over the whole blob
  * decompress_chunked - faithful port of ilo4_toolbox decompress_all():
        streams in 0x10000 chunks, stops each chunk while >limit bytes
        remain so a token group is never split across a chunk boundary,
        carries the unconsumed tail + the persistent window into the next
        chunk, and finishes the final <=limit bytes with limit=0.
For a single complete stream both yield identical bytes; the chunked form
is the reference and is what main() uses.

The image carries its own geometry: ImgHeader @0x440 (magic 'iLO3'),
compression_type 0x1000 = LZSS, raw_size @0x30, decompressed_size @0x2c,
payload begins after the 0x440-byte header (0x40 fields + 0x200 sig +
0x200 pad) at 0x880.

Usage:
  ilo_lzss.py <infile> <outfile>            # auto: parse header, decompress image
  ilo_lzss.py <infile> <outfile> <start> <end>   # explicit span (hex ok)
  ilo_lzss.py --verify <infile>             # parse + decompress + cross-check impls
"""
import sys, struct

WINDOW = 0x1000


def decompress_stream(data: bytes) -> bytes:
    window = bytearray(WINDOW)
    wpos = 0
    out = bytearray()
    i, n = 0, len(data)
    while i < n:
        ctrl = data[i]; i += 1
        for bit in range(8):
            if i >= n:
                break
            if (ctrl >> (7 - bit)) & 1:
                c = data[i]; i += 1
                out.append(c); window[wpos] = c; wpos = (wpos + 1) & 0xfff
            else:
                if i + 1 >= n:
                    break
                b0, b1 = data[i], data[i + 1]; i += 2
                length = (b0 >> 4) + 3
                ptr = wpos - (b1 + ((b0 & 0xf) << 8)) - 1
                for k in range(length):
                    v = window[(ptr + k) & 0xfff]
                    out.append(v); window[wpos] = v; wpos = (wpos + 1) & 0xfff
    return bytes(out)


class _Ctx:
    __slots__ = ("window", "wpos")

    def __init__(self):
        self.window = bytearray(WINDOW)
        self.wpos = 0


def _decompress_pass(ctx, data: bytes, out: bytearray, limit: int) -> int:
    """Port of ilo4_toolbox decompress(): consume `data` while len>limit,
    appending to `out`; return the count of unconsumed trailing bytes."""
    i, n = 0, len(data)
    while (n - i) > limit:
        ctrl = data[i]; i += 1
        for bit in range(8):
            if limit == 0 and i >= n:
                break
            if (ctrl >> (7 - bit)) & 1:
                c = data[i]; i += 1
                out.append(c); ctx.window[ctx.wpos] = c
                ctx.wpos = (ctx.wpos + 1) & 0xfff
            else:
                b0 = data[i]; b1 = data[i + 1]; i += 2
                length = (b0 >> 4) + 3
                ptr = ctx.wpos - (b1 + ((b0 & 0xf) << 8)) - 1
                for k in range(length):
                    v = ctx.window[(ptr + k) & 0xfff]
                    out.append(v); ctx.window[ctx.wpos] = v
                    ctx.wpos = (ctx.wpos + 1) & 0xfff
    return n - i


def decompress_chunked(data: bytes, chunks: int = 0x10000, limit: int = 16) -> bytes:
    ctx = _Ctx()
    out = bytearray()
    data = memoryview(data)
    while len(data) > 0:
        if len(data) < chunks:
            ret = _decompress_pass(ctx, bytes(data[:chunks]), out, limit)
            data = data[-ret:] if ret else data[:0]
            ret = _decompress_pass(ctx, bytes(data[:chunks]), out, 0)
            data = data[-ret:] if ret else data[:0]
        else:
            ret = _decompress_pass(ctx, bytes(data[:chunks]), out, limit)
            data = data[chunks - ret:]
    return bytes(out)


def parse_image(raw: bytes, hdr_off: int = 0x440):
    """Return dict with payload span + sizes for the ImgHeader at hdr_off."""
    magic = raw[hdr_off:hdr_off + 4]
    compr = struct.unpack_from("<H", raw, hdr_off + 0x22)[0]
    dec_size = struct.unpack_from("<I", raw, hdr_off + 0x2c)[0]
    raw_size = struct.unpack_from("<I", raw, hdr_off + 0x30)[0]
    payload = hdr_off + 0x440            # 0x40 fields + 0x200 sig + 0x200 pad
    return {"magic": magic, "compression_type": compr, "decompressed_size": dec_size,
            "raw_size": raw_size, "start": payload, "end": payload + raw_size}


def main():
    a = sys.argv[1:]
    if not a:
        sys.exit(__doc__)
    if a[0] == "--verify":
        raw = open(a[1], "rb").read()
        img = parse_image(raw)
        print("ImgHeader:", {k: (hex(v) if isinstance(v, int) else v) for k, v in img.items()})
        blob = raw[img["start"]:img["end"]]
        s = decompress_stream(blob)
        c = decompress_chunked(blob)
        print(f"stream  : {len(s)} bytes")
        print(f"chunked : {len(c)} bytes")
        print(f"identical: {s == c}")
        print(f"matches header decompressed_size: {len(c) == img['decompressed_size']}  "
              f"(hdr=0x{img['decompressed_size']:x} got=0x{len(c):x})")
        mark = b"Gromit-XE verification"
        print(f"marker '{mark.decode()}' present: {mark in c}  @ "
              f"0x{c.find(mark):x}")
        return
    inf, outf = a[0], a[1]
    raw = open(inf, "rb").read()
    if len(a) >= 4:
        blob = raw[int(a[2], 0):int(a[3], 0)]
    else:
        img = parse_image(raw)
        blob = raw[img["start"]:img["end"]]
    res = decompress_chunked(blob)
    open(outf, "wb").write(res)
    print(f"in {len(blob)} -> out {len(res)} bytes (x{len(res) / len(blob):.2f})")


if __name__ == "__main__":
    main()
