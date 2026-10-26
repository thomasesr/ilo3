# ilo3-chif — host CHIF tool (debug console + SPI reflash over PCI)

Compiled host-side counterpart of the clean-room Rust firmware's CHIF debug
service (`rebuild/ilo3-rs/firmware/src/dbgchif.rs`). The iLO board has no exposed
JTAG/UART, so the firmware's bring-up channel rides **CHIF — pure PCI**. This tool
is the host end: it mmaps the Gromit-XE CHIF shared-memory BAR and speaks the same
framing as `firmware/src/chif.rs`.

It is the compiled replacement for `../ilo3-rs/tools/ilo3-chif-console.py`: the
XCP-ng 8.3 dom0 has no guaranteed `python3`, and a **static musl** binary runs on
it regardless of the (CentOS 7.5 / glibc 2.17) base.

## Build

```sh
# For the XCP-ng host (static, glibc-independent — copy this one to dom0):
rustup target add x86_64-unknown-linux-musl   # once
cargo build --release --target x86_64-unknown-linux-musl
#   -> target/x86_64-unknown-linux-musl/release/ilo3-chif  (~400 KB, static-PIE)

# Local smoke build:
cargo build --release
```

This crate deliberately sits **outside** `rebuild/ilo3-rs/`: that tree's
`.cargo/config.toml` pins the `armv5te-ilo3` bare-metal target + `build-std`,
which would break this host (std, x86_64) build.

## Run (on the XCP-ng host, as root)

Free the CHIF function from the stock host driver first, then point the tool at
its BAR2 resource (`.2` = `103c:3307`, 1 MB window — `docs/06`):

```sh
lspci -nn | grep 103c:3307                    # find the b:d.f, e.g. 02:00.2
echo 0000:02:00.2 > /sys/bus/pci/drivers/hpilo/unbind   # if hpilo holds it
ilo3-chif observe                             # auto-finds 103c:3307 .2
```

`--bar <resourceN>` or `--pci <bdf>` override the auto-discovery.

### Modes

| command | what |
|---|---|
| `observe` | passive hexdump of the window + CCB table — **works with no protocol**; use first to confirm iLO is doing CHIF |
| `console` | interactive `ping / drain / id / md / mw / mb / flash / verify / reboot` |
| `ping` `drain` `id` `reboot` | one-shot |
| `md <addr> <len>` / `mw <addr> <u32>` / `mb <addr> <u8>` | raw peek/poke (hex args) |
| `flash <file> [base]` | **firmware reflash** — erase + stream + verify iLO's SPI payload from the host, no resolder |
| `verify <file> [base]` | read-back compare only |
| `recover --image <main.bin>` | SPL RAM-boot a raw main image when the main is too broken to reach its own CHIF task (no doorbell; reset iLO while it waits) |

### Reflash (the firmware reflash tool)

```sh
ilo3-chif flash ilo3_194_payload.bin          # erase+write+verify @ 0x0
ilo3-chif flash flash.bin --base 0x0          # full image is auto-clipped to the payload
ilo3-chif console                             # then: reboot
```

The SPL/boot region (`>= 0x7f0000`) is **write-protected in firmware** — the
always-good recovery path. A bad payload reflash is itself re-flashable from here;
only a nuked SPL needs the SPI programmer. A full-image file is auto-clipped to
`[base, 0x7f0000)`.

## Doorbell (CONFIRM-ON-HW)

The BMC learns a request is waiting by reading `0x01F03004` bit2 (`chif.rs`
`rx_pending`). The host raises that from a register in a small `.2` BAR; the exact
BAR/offset/value is not yet pinned (oracle: `drivers/misc/hpilo.c`
`hpilo_db_outbound`). Supply it once known:

```sh
ilo3-chif console --db-off 0x<reg> [--db-bar <resource1>] [--db-val 0x<val>]
```

Without it, `observe` still works, and a firmware build that polls CCB ownership
directly still answers.

## Ownership handshake

Request/response share one CCB; **bit31 of the ring-control word** is the baton:
host sets it (BMC owns → process the request), the BMC writes the response into
the same message region and **clears** it (host owns again → response ready). The
host waits for the **clear**. (This corrects the earlier reference polarity in the
python tool, which waited for a re-*set*; `firmware/src/dbgchif.rs` nets the bit
*cleared* after `rx_release`.)
```
+0x00 u16 len   +0x06 u16 channel(0xDB90)   +0x08.. payload
request  payload: [cmd u8][addr u32][arg u32][data..]
response payload: [status u8][len u16][data..]
```
