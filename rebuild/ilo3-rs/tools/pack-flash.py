#!/usr/bin/env python3
"""Assemble a flashable 8 MB Gromit-XE image: main firmware payload + SPL.

Layout (our own; the SPL reads the header at flash base, docs: spl/src/flash.rs):
  0x000000  payload header (16 B) + main firmware .bin
              +0x00 u32 magic "GXSP"  +0x04 u32 length
              +0x08 u32 load=0x20000000  +0x0C u32 entry=0x20000000
  0x7f0000  SPL .bin (XIP boot ROM replacement; reset target 0xFFFF0000)
  fill      0xFF to 8 MB
"""
import struct, sys

FLASH_SIZE = 0x800000      # 8 MB
SPL_OFFSET = 0x7f0000      # = 0xFFFF0000 (high vectors / reset)
LOAD = ENTRY = 0x20000000  # main firmware runs from DDR here
MAGIC = b"GXSP"

def main(main_bin, spl_bin, out):
    fw = open(main_bin, "rb").read()
    spl = open(spl_bin, "rb").read()
    hdr = MAGIC + struct.pack("<III", len(fw), LOAD, ENTRY)
    payload = hdr + fw

    if len(payload) > SPL_OFFSET:
        sys.exit(f"payload {len(payload)} B overruns SPL region @ {SPL_OFFSET:#x}")
    if len(spl) > FLASH_SIZE - SPL_OFFSET:
        sys.exit(f"SPL {len(spl)} B overruns 64 KB flash region")

    img = bytearray(b"\xff" * FLASH_SIZE)
    img[0:len(payload)] = payload
    img[SPL_OFFSET:SPL_OFFSET + len(spl)] = spl
    open(out, "wb").write(img)
    print(f"wrote {out}: {FLASH_SIZE} B")
    print(f"  payload @ 0x000000  hdr 16 B + fw {len(fw)} B = {len(payload)} B")
    print(f"  spl     @ {SPL_OFFSET:#08x}  {len(spl)} B  (reset 0xFFFF0000)")

if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("usage: pack-flash.py <main.bin> <spl.bin> <out.bin>")
    main(*sys.argv[1:])
