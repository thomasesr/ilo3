#!/usr/bin/env python3
"""iLO 3 CHIF debug console (host side) — talk to the Rust firmware over PCI.

No JTAG/UART on the board, so the firmware's debug channel rides CHIF (pure PCI).
The host boots AFTER iLO, so this tool PULLS (DRAIN) the buffered early-boot log —
nothing is missed. Pairs with firmware src/dbgchif.rs + src/dbglog.rs.

Two modes:
  observe   mmap the CHIF BAR and hexdump the control header + CCBs — passive, no
            protocol; use this FIRST to confirm iLO is doing CHIF at all (and to
            reverse the real CCB/doorbell convention against drivers/misc/hpilo.c).
  console   interactive request/response:
              ping | drain | id | md <addr> <len> | mw <addr> <u32> | mb <addr> <u8>
              flash <file> [base] | verify <file> [base] | reboot
            `flash` reflashes iLO's SPI from the host (no resolder): erase + stream +
            verify + optional `reboot`. The SPL/boot region (>=0x7f0000) is write-
            protected in firmware, so a bad reflash of the payload is itself re-
            flashable from the host; only a nuked SPL needs the SPI programmer.
  recover   RAM-boot a raw main image (--image ilo3-fw.bin) via the SPL recovery
            window (spl/src/recovery.rs) — recovers a main too broken to reach its
            own CHIF debug task, with NO doorbell dependency. Volatile; persist the
            fix afterwards with `console` + `flash`. Reset iLO while this waits.

Run as root. Point --bar at the iLO CHIF function's BAR2 resource, e.g.:
  /sys/bus/pci/devices/0000:02:00.2/resource2   (1 MB window, docs/06)
You may need to unbind hpilo first so the region is free:
  echo 0000:02:00.2 > /sys/bus/pci/drivers/hpilo/unbind

CONFIRM-ON-HW: the CCB message-offset convention + the host→iLO doorbell are the
two params to pin from hpilo.c + observe mode (see the constants below). Until
then `console` mode is a starting point, `observe` works now.
"""
import argparse, mmap, os, struct, sys, time

# --- CHIF layout (mirror of firmware/src/chif.rs + dbgchif.rs) ---
WINDOW = 0x100000          # 1 MB BAR2
DATA_REGION = 0x4000       # control header lives here
MAGIC = b"CHIF"            # 0x18 message type
NR_CCB = 32
CCB_SIZE = 0x80
CCB_OFFSET = 0x04          # u32 message offset into the window
CCB_OWN = 0x10             # u32, bit31 = ownership
OWN_BIT = 1 << 31
DBG_CH = 0xDB90            # debug channel id (matches dbgchif.rs)

# Convention for THIS debug channel (firmware + tool must agree; CONFIRM-ON-HW):
REQ_CCB = 1                # host writes requests here
REQ_MSG_OFF = 0x8000 + REQ_CCB * 0x4000   # its message region in the window

CMD = {"ping": 1, "drain": 2, "md": 3, "mw": 4, "mb": 5,
       "id": 0x10, "ferase": 0x11, "fwrite": 0x12, "fread": 0x13, "reboot": 0x14}

SPL_BASE = 0x7f0000        # write-protected in firmware (recovery boot)
CHUNK = 4096               # bytes per fwrite / fread (well under the 16 KB region)

# --- SPL recovery mailbox (spl/src/recovery.rs), at window + 0x8000 ---
REC = 0x8000
REC_MAGIC, REC_REQ, REC_CMD, REC_ADDR = REC+0x00, REC+0x04, REC+0x08, REC+0x0C
REC_LEN, REC_RESP, REC_STATUS, REC_DATA = REC+0x10, REC+0x14, REC+0x18, REC+0x40
RCVR = 0x52564352          # "RCVR" little-endian
RC_WRITE, RC_BOOT, RC_PING = 1, 2, 3
RC_CHUNK = 0x3000          # mailbox data area (recovery.rs MAX_CHUNK)
RAM_LOAD = 0x20000000      # DRAM base: where main's _start runs (= flash load addr)


def open_bar(path):
    f = os.open(path, os.O_RDWR | os.O_SYNC)
    size = os.fstat(f).st_size or WINDOW
    return mmap.mmap(f, size, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE)


def hexdump(mm, base, length):
    for off in range(0, length, 16):
        row = mm[base + off: base + off + 16]
        hexs = " ".join(f"{b:02x}" for b in row)
        asc = "".join(chr(b) if 32 <= b < 127 else "." for b in row)
        print(f"{base + off:06x}: {hexs:<48} {asc}")


def observe(mm):
    print("== control header @ 0x4000 ==")
    hexdump(mm, DATA_REGION, 32)
    print("== CCBs (offset, ownership) ==")
    for i in range(NR_CCB):
        b = i * CCB_SIZE
        off = struct.unpack_from("<I", mm, b + CCB_OFFSET)[0]
        own = struct.unpack_from("<I", mm, b + CCB_OWN)[0]
        if off or own:
            print(f"  ccb[{i:2}] off={off:#08x} own={own:#010x}"
                  f"{'  (produced)' if own & OWN_BIT else ''}")


def put_u16(mm, off, v):
    struct.pack_into("<H", mm, off, v)


def put_u32(mm, off, v):
    struct.pack_into("<I", mm, off, v)


def ring_doorbell(mm):
    # CONFIRM-ON-HW: the host→iLO doorbell. docs/28: iLO reads 0x01F03004 bit2.
    # The host sets it via a register in the device BAR / config space — pin this
    # from hpilo.c (hpilo_db_outbound). Placeholder: no-op (fill in once known).
    pass


def request(mm, cmd, addr=0, arg=0, data=b"", timeout=5.0):
    # payload = [cmd u8][addr u32][arg u32][data...] ; length field = payload len
    msg = REQ_MSG_OFF
    plen = 9 + len(data)
    put_u16(mm, msg + 0x00, plen)       # per-message payload length
    put_u16(mm, msg + 0x06, DBG_CH)     # channel id
    mm[msg + 0x08] = cmd
    put_u32(mm, msg + 0x09, addr)
    put_u32(mm, msg + 0x0d, arg)
    if data:
        mm[msg + 0x11: msg + 0x11 + len(data)] = data
    # global control header: magic + 14-byte sum-to-zero checksum
    mm[DATA_REGION:DATA_REGION + 4] = MAGIC
    chk = (-sum(mm[DATA_REGION:DATA_REGION + 13])) & 0xff
    mm[DATA_REGION + 13] = chk
    put_u32(mm, REQ_CCB * CCB_SIZE + CCB_OFFSET, msg)
    put_u32(mm, REQ_CCB * CCB_SIZE + CCB_OWN, OWN_BIT)  # hand to iLO
    ring_doorbell(mm)
    # wait for iLO to produce the response on the same CCB
    t0 = time.time()
    while time.time() - t0 < timeout:
        own = struct.unpack_from("<I", mm, REQ_CCB * CCB_SIZE + CCB_OWN)[0]
        if own & OWN_BIT:  # iLO re-set ownership = response ready
            roff = struct.unpack_from("<I", mm, REQ_CCB * CCB_SIZE + CCB_OFFSET)[0]
            status = mm[roff + 0x08]
            n = struct.unpack_from("<H", mm, roff + 0x09)[0]
            data = bytes(mm[roff + 0x0b: roff + 0x0b + n])
            put_u32(mm, REQ_CCB * CCB_SIZE + CCB_OWN, 0)  # release
            return status, data
        time.sleep(0.005)
    return None, b""


def _clip_to_payload(base, size):
    # firmware write-protects the SPL/boot region; clip a full-flash image to the
    # payload so `flash flash.bin` writes only [base, SPL_BASE).
    if base < SPL_BASE and base + size > SPL_BASE:
        print(f"note: clipping to payload region [{base:#x}, {SPL_BASE:#x}) "
              f"(SPL is write-protected; use the SPI programmer for it)")
        return SPL_BASE - base
    return size


def flash_image(mm, path, base=0):
    data = open(path, "rb").read()
    size = _clip_to_payload(base, len(data))
    data = data[:size]
    print(f"flash {path}: {size} B @ {base:#x}")
    st, _ = request(mm, CMD["ferase"], base, size, timeout=60.0)
    if st != 0:
        print(f"  erase FAILED status={st}"); return False
    print("  erased")
    for off in range(0, size, CHUNK):
        chunk = data[off:off + CHUNK]
        st, _ = request(mm, CMD["fwrite"], base + off, len(chunk), chunk)
        if st != 0:
            print(f"  write FAILED @ {base+off:#x} status={st}"); return False
        print(f"\r  wrote {off+len(chunk)}/{size}", end="", flush=True)
    print()
    return verify_image(mm, data, base)


def verify_image(mm, data, base=0):
    if isinstance(data, str):
        data = open(data, "rb").read()
        data = data[:_clip_to_payload(base, len(data))]
    for off in range(0, len(data), CHUNK):
        want = data[off:off + CHUNK]
        st, got = request(mm, CMD["fread"], base + off, len(want))
        if st != 0 or got[:len(want)] != want:
            print(f"  verify MISMATCH @ {base+off:#x} status={st}"); return False
        print(f"\r  verified {off+len(want)}/{len(data)}", end="", flush=True)
    print("\n  OK")
    return True


def console(mm):
    print("iLO3 CHIF debug console. commands:")
    print("  ping | drain | id | md <a> <n> | mw <a> <v> | mb <a> <v>")
    print("  flash <file> [base_hex] | verify <file> [base_hex] | reboot | quit")
    while True:
        try:
            line = input("chif> ").split()
        except EOFError:
            break
        if not line:
            continue
        c = line[0]
        if c == "quit":
            break
        # multi-step / special commands
        if c == "flash" and len(line) > 1:
            flash_image(mm, line[1], int(line[2], 16) if len(line) > 2 else 0)
            continue
        if c == "verify" and len(line) > 1:
            verify_image(mm, line[1], int(line[2], 16) if len(line) > 2 else 0)
            continue
        if c == "reboot":
            request(mm, CMD["reboot"], timeout=1.0)  # no response: iLO jumps
            print("reboot sent (iLO re-runs flash boot; reconnect after boot)")
            continue
        if c not in CMD:
            print("?"); continue
        addr = int(line[1], 16) if len(line) > 1 else 0
        arg = int(line[2], 16) if len(line) > 2 else 0
        st, data = request(mm, CMD[c], addr, arg)
        if st is None:
            print("timeout (doorbell/convention not confirmed? try 'observe')")
        elif c == "drain":
            sys.stdout.write(data.decode("utf-8", "replace"))
        elif c in ("md", "fread"):
            hexbytes(data, addr)
        elif c == "id":
            print("JEDEC id: " + " ".join(f"{b:02x}" for b in data))
        else:
            print(f"status={st} data={data!r}")


def hexbytes(data, base):
    for off in range(0, len(data), 16):
        row = data[off:off + 16]
        print(f"{base + off:08x}: " + " ".join(f"{b:02x}" for b in row))


def rec_req(mm, cmd, addr=0, data=b"", wait=True, timeout=3.0):
    # one recovery-mailbox request; (req_seq != resp_seq) carries it to the SPL
    rec_req.seq = getattr(rec_req, "seq", 0) + 1
    seq = rec_req.seq
    if data:
        mm[REC_DATA: REC_DATA + len(data)] = data
    put_u32(mm, REC_CMD, cmd)
    put_u32(mm, REC_ADDR, addr)
    put_u32(mm, REC_LEN, len(data))
    put_u32(mm, REC_REQ, seq)
    if not wait:
        return 0
    t0 = time.time()
    while time.time() - t0 < timeout:
        if struct.unpack_from("<I", mm, REC_RESP)[0] == seq:
            return struct.unpack_from("<I", mm, REC_STATUS)[0]
        time.sleep(0.002)
    return None


def recover(mm, image_path, connect_timeout=90.0):
    # Stream the RAW main firmware (ilo3-fw.bin, _start at 0x20000000 -- NOT
    # flash.bin) into DRAM and RAM-boot it. Recovers a main too broken to reach
    # its own CHIF debug task, without the SPI programmer. See spl/src/recovery.rs.
    data = open(image_path, "rb").read()
    print(f"recover: {len(data)} B -> DRAM {RAM_LOAD:#x} (volatile RAM-boot)")
    put_u32(mm, REC_RESP, 0)
    put_u32(mm, REC_REQ, 0)
    rec_req.seq = 0
    put_u32(mm, REC_MAGIC, RCVR)
    print("waiting for the SPL recovery window — reset/power-cycle iLO now...")
    t0 = time.time()
    while time.time() - t0 < connect_timeout:
        put_u32(mm, REC_MAGIC, RCVR)   # keep raised across iLO window re-mapping
        if rec_req(mm, RC_PING, timeout=0.5) == 0:
            break
    else:
        print("no SPL window (PCI enumeration / window timing — see docs/48)")
        return False
    print("connected to SPL recovery")
    for off in range(0, len(data), RC_CHUNK):
        chunk = data[off:off + RC_CHUNK]
        st = rec_req(mm, RC_WRITE, RAM_LOAD + off, chunk)
        if st != 0:
            print(f"\n  write FAILED @ {RAM_LOAD+off:#x} status={st}")
            return False
        print(f"\r  streamed {off+len(chunk)}/{len(data)}", end="", flush=True)
    print()
    rec_req(mm, RC_BOOT, RAM_LOAD, wait=False)
    print(f"boot sent -> iLO RAM-booting {RAM_LOAD:#x} "
          "(volatile; persist with main `flash` once it's healthy)")
    return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["observe", "console", "recover"])
    ap.add_argument("--bar", required=True, help="path to the CHIF BAR2 resource")
    ap.add_argument("--image", help="raw main firmware .bin for `recover`")
    a = ap.parse_args()
    mm = open_bar(a.bar)
    if a.mode == "observe":
        observe(mm)
    elif a.mode == "recover":
        if not a.image:
            ap.error("recover needs --image <ilo3-fw.bin>")
        recover(mm, a.image)
    else:
        console(mm)


if __name__ == "__main__":
    main()
