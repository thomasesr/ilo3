# iLO 3 SMIF SPI boot-flash — full transfer protocol

Date: 2026-10-20. Static RE of the type-2 **kernel** `gxekflsh` driver
(`decompiled/kernel_type2.bin`, `ARM:LE:32:v5`). Completes `docs/15`: the register
file, opcodes, data FIFO, and the two completion waits are now fully decoded, enough
to write a Linux `spi-mem` controller (`rebuild/.../files/spi-gxe-smif.c`).

## Code map (file offsets in `kernel_type2.bin`)

| Addr | Function |
|------|----------|
| `0x252d0` | `wait_ready` — poll GO reg (`0xC0000204`) **bit1** until clear (controller busy) |
| `0x252f0` | `spi_is_busy` / RDSR — opcode `0x05`, read FIFO `0xC0000240` bit0 = flash WIP |
| `0x25360` | `write_enable` — unlock + opcode `0x06` (WREN) |
| `0x253d0` | `program` — page program, opcode `0x02`, 64-byte FIFO chunks ×4 per 256-byte page |
| `0x2559c` | `erase` — opcode `0x39` preamble then `0xD8` (64 KB block erase), addr += 0x10000 |

Literal pool @ `0x2564c`/`0x25678` holds the register bases `0xC0000202`/`0xC0000200`;
the `gxekflsh` string sits between them @ `0x25650`.

## Register file — base `0xC0000200` (ALL CONFIRMED from disassembly)

| Off | Width | Role | Evidence |
|-----|-------|------|----------|
| `+0x00` | u32 | **CTRL** — `ldr;bic #0x3800;str` clears bits [13:11] before each op (mode/clk select) | prog/erase entry |
| `+0x02` | u8 | **MODE** — `3` = data transfer (program/erase/read-with-data), `0` = no-data cmd | `strb 3`/`strb 0` |
| `+0x04` | u8 | **GO** (write = launch) / **BUSY** (read bit1) | `strb` go + `ldrb;tst #2` |
| `+0x05` | u8 | **OPCODE** — SPI command byte; **unlock `0x55`,`0xAA`** written here first (control ops) | `strb 0x55/0xAA/5/6/2/0xD8` |
| `+0x06` | u8 | **COUNT** — data byte count (≤ `0x40`) | `strb 0x40`/`1`/`0` |
| `+0x08` | u32 | **ADDR** — 24-bit flash address (`and ...#0xff0000`/`#0xffffff`) | `str addr` |
| `+0x40` | 64 B | **DATA FIFO** — program source / read dest / RDSR result | `memcpy 0x40`, `ldrb +0x3e` |

## Per-command sequences (exact, from fw)

**RDSR** (`0x252f0`): `MODE=0; COUNT=1; OPCODE=0x05; GO=1;` wait-ready; read `FIFO[0]` bit0 = WIP.
Preceded by unlock `0x55,0xAA → OPCODE`.

**WREN** (`0x25360`): unlock; `MODE=0; COUNT=0; OPCODE=0x06; GO=9;` (`+0x04` preset `8` then `9`).

**Page program** (`0x253d0`): per 64-byte chunk — `memcpy src→FIFO(0x40); ADDR=addr; MODE=3;
COUNT=0x40; OPCODE=0x02; GO=0xD;` wait-ready. 4 chunks = one 256-byte page, then WIP-poll.
No unlock (the preceding WREN armed it).

**Block erase** (`0x2559c`): WREN; `MODE=3; ADDR=addr; OPCODE=0x39; GO=9;` wait-ready; WREN;
`OPCODE=0xD8; GO=9;` wait-ready; `addr += 0x10000`; WIP-poll. (`0x39` = vendor preamble/unprotect
before the `0xD8` 64 KB erase.)

## Two completion waits

1. **Controller-ready**: poll GO/BUSY `0xC0000204` **bit1** == 0 (`0x252d0`). After every command phase.
2. **Flash-WIP**: RDSR (`0x05`) → FIFO bit0 == 0. After erase/program (internal flash op).

## GO-register encoding (the one soft spot)

Observed launch values: **RDSR=1, WREN=9, erase=9, program=0xD** (bit0 = start always). The exact
meaning of bits 3:1 (phase/direction) is not fully pinned → the Linux driver keys GO off the op
class (`GXE_SMIF_GO_*`) and flags it **CONFIRM-ON-HARDWARE**. The bulk-**read** path (opcode
`0x03`/`0x0B`) was not isolated in the fw (reads may use a separate DMA/window); the driver reads
via the same opcode+addr+count+FIFO model, which the register file supports.

## For the OpenBMC port

`spi-gxe-smif.c` = a `spi-mem` controller (`hpe,gxe-smif`, `reg = <0xc0000200 0x80>`): the
spi-nor/mtd core drives WREN/RDSR/erase/program/read as individual `exec_op`s; the driver issues
each through the register file above, 64-byte FIFO windows, polling GO bit1 then (core-driven) WIP.
Partition the flash per `docs/01`/`docs/03` (boot ROM, main image, kernel, certs/sig) via DT
`fixed-partitions` for `mtd`/`fw_env`. The `0x01F10000` "SMIF" (host BIOS, `romchf`) is a
**separate** controller (`hpe,gxe-host-smif`) — not this one.
