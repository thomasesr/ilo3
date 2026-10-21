# iLO 3 I2C/SMBus engine — transfer protocol

Date: 2026-10-20. Static RE of `decompiled/os_main_base0.bin` (`ARM:LE:32:v5`, base 0),
`src\i2c*.c`. Extends the register table in `docs/05`. The Gromit-XE has **8 I2C/SMBus
engines** in the `0x01F10000` host-ROM/peripheral block, driven as a small DMA/mailbox
engine (fill buffer → set descriptor → kick → poll status), **not** register bit-bang.

Confidence: the three region bases, the 16-byte buffer, the reset sequence and the error
status bits are read directly from disassembly (**CONFIRMED**). The command-byte encoding
and the exact trigger are partly behind an INTEGRITY IODevice call (**BEST-GUESS** — flagged
in `i2c-gxe.c`).

## Firmware call chain

| Addr | Role |
|------|------|
| `0xe36e4` | `i2c_mwr` / ProcessRequest — CCB dispatcher; op code at `[r5,#1]`, 22-entry jump table @ `0xe375c` |
| `0xe23b0` | mid-level — bus mux + lock, builds the request, calls the engine |
| `0xe21c8` | low-level engine xfer — buffer copy + descriptor write + kick + status read |
| `0xe30cc` | engine reset/abort |
| `0x1873e34` | INTEGRITY IODevice kick (engine handle from `[0x32dc8][eng]`) — the trigger |

## Engine MMIO layout (physical; base `0x01F10100`)

Three sub-regions, indexed by engine `e` (0..7):

| Region | Address | Per-engine | Fields |
|--------|---------|-----------|--------|
| **engine regs** | `0x01F10100 + e*0x10` | 16 B | status `+0x00` (bit0 busy, bit4 active, err `0xE0` in `+0x01`), ctrl/err `+0x01`, data `+0x04`, cmd `+0x08` |
| **TX/RX buffer** | `0x01F10180 + e*0x10` (**+`0x200` for e=6,7**) | 16 B | transfer payload; byte0 = addressing byte |
| **descriptor** | `0x01F101E0 + e*4` | 4 B | status `+0`, cmd `+1`, len `+2`, (`+3` second length/param) |

Engines 0-5 buffers: `0x01F10180..0x01F101DF`; descriptors (all 8): `0x01F101E0..0x01F101FF`;
engines 6-7 buffers: `0x01F10380..` (the `+0x200` paging). DT `reg = <0x1f10100 0x300>` covers
all three. Per-engine IRQ **8-15** (`docs/12`).

## Transfer sequence (from `0xe21c8`)

1. Copy ≤ `0x10` TX bytes into the engine buffer (`strb` loop; dest = `BUF(e)+idx`).
2. Descriptor: `len` → `+0x02`; second param → `+0x03`; `cmd | 1` → `+0x01` (**bit0 = go**).
3. Kick via IODevice call `0x1873e34` on the engine handle. (For Linux we treat the
   descriptor `cmd` write as the trigger — the closest direct-register equivalent.)
4. Poll descriptor status `+0x00`: **`0x2` = done**; bit0 busy, bit2/bit3 = bus error.
5. On error: reset — `strb 0 → reg+0x01`, `strb 0x80 → reg+0x04`, `strb 0x33 → reg+0x04`
   (variant in `0xe30cc`: `0xB0`/`0x30` → `reg+0x08`). Then read RX bytes back from buffer.

## Resolved globals (RAM pointers in `0xe21c8`)

| Literal @ | Value | Meaning |
|-----------|-------|---------|
| `0xe2838` | `0x01F10180` | TX/RX buffer base |
| `0xe283c` | `0x01F101E0` | descriptor base |
| `0xe20f4` | `0x01F10104` | engine ctrl base (reg+0x04 of engine 0) |
| `0xe20fc` | `0x00032dc8` | engine handle[] (RAM) — IODevice objects for the kick |
| `0xe2848` | `0x00032da8` | bus-map (RAM) — logical bus → engine |

## For the OpenBMC port

`rebuild/meta-ilo3/recipes-kernel/linux/files/i2c-gxe.c` implements this as a Linux
`i2c_algorithm.master_xfer`: one `i2c_adapter` per engine, poll-based completion, 16-byte
cap per message, reset-on-error. **CONFIRM-ON-HARDWARE**: the command-byte direction encoding
and that the descriptor-cmd write actually triggers the transfer (vs. a separate kick the
firmware only reaches through its IODevice object). If transfers never complete, that is the
first thing to revisit — IRQ 8-15 are available to convert the poll loop to interrupt-driven.
