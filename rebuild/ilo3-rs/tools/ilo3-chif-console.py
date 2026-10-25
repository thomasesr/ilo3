#!/usr/bin/env python3
"""iLO 3 CHIF debug console (host side) — talk to the Rust firmware over PCI.

No JTAG/UART on the board, so the firmware's debug channel rides CHIF (pure PCI).
The host boots AFTER iLO, so this tool PULLS (DRAIN) the buffered early-boot log —
nothing is missed. Pairs with firmware src/dbgchif.rs + src/dbglog.rs.

Two modes:
  observe   mmap the CHIF BAR and hexdump the control header + CCBs — passive, no
            protocol; use this FIRST to confirm iLO is doing CHIF at all (and to
            reverse the real CCB/doorbell convention against drivers/misc/hpilo.c).
  console   interactive request/response: ping | drain | md <addr> <len> |
            mw <addr> <u32> | mb <addr> <u8>.

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

CMD = {"ping": 1, "drain": 2, "md": 3, "mw": 4, "mb": 5}


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


def request(mm, cmd, addr=0, arg=0, timeout=2.0):
    # build [cmd u8][addr u32][arg u32] in the request CCB's message region
    msg = REQ_MSG_OFF
    put_u16(mm, msg + 0x00, 9)          # per-message length
    put_u16(mm, msg + 0x06, DBG_CH)     # channel id
    mm[msg + 0x08] = cmd
    put_u32(mm, msg + 0x09, addr)
    put_u32(mm, msg + 0x0d, arg)
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


def console(mm):
    print("iLO3 CHIF debug console. commands: ping | drain | md <a> <n> | mw <a> <v> | mb <a> <v> | quit")
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
        if c not in CMD:
            print("?"); continue
        addr = int(line[1], 16) if len(line) > 1 else 0
        arg = int(line[2], 16) if len(line) > 2 else 0
        st, data = request(mm, CMD[c], addr, arg)
        if st is None:
            print("timeout (doorbell/convention not confirmed? try 'observe')")
        elif c in ("drain",):
            sys.stdout.write(data.decode("utf-8", "replace"))
        elif c == "md":
            hexbytes(data, addr)
        else:
            print(f"status={st} data={data!r}")


def hexbytes(data, base):
    for off in range(0, len(data), 16):
        row = data[off:off + 16]
        print(f"{base + off:08x}: " + " ".join(f"{b:02x}" for b in row))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mode", choices=["observe", "console"])
    ap.add_argument("--bar", required=True, help="path to the CHIF BAR2 resource")
    a = ap.parse_args()
    mm = open_bar(a.bar)
    (observe if a.mode == "observe" else console)(mm)


if __name__ == "__main__":
    main()
