# iLO 3 DDR3 init — boot-ROM memory-controller bring-up

Date: 2026-10-20. Static RE of the boot ROM (`decompiled/bootrom_7f0000.bin`, raw
`ARM:LE:32:v5` @ base `0x7f0000`). The DDR3 controller init function is at **`0x7f198c`**,
driving a (register, value) table at **`0x7f1a0c`**. All CONFIRMED from disassembly — this is
the sequence a U-Boot/first-stage port must reproduce to bring up DRAM on Gromit-XE.

## Memory controller register block — base `0xC0000000`

| Reg | Init value | Role (inferred) |
|-----|-----------|-----------------|
| `0xC0000018` | `0xFDF0FFF0` | address/row-col mask or DDR config |
| `0xC0000020` | `0x3` → (ready) → `0x2` | **MEMCTL / status** — write `0x3` to start; **read bit7 (0x80) = DDR ready/calibrated**; write `0x2` after calibration |
| `0xC0000024` | `0x22` | timing |
| `0xC0000028` | `0x552C9180` | timing (tRAS/tRC group) |
| `0xC000002C` | `0x50C34F` | timing |
| `0xC0000030` | `0x1520` | timing / mode |
| `0xC0000034` | `0x5` | mode |
| `0xC0000038` | `0x0` | — |
| `0xC000003C` | `0x0` | — |
| `0xC0000040` | `0xAA9658E3` | timing |
| `0xC0000048` | `0x7D` | refresh interval |
| `0xC0000064` | strap `>>1` | **MEMCFG** — final config, from the strap at `0x801F00A4` |
| `0xC0000098` | (read) | 64-bit timer counter — used for the power-stable delay |
| `0x80200040` | `0x400` | companion block, post-init write |

(These are the exact FW 1.94 values for the fitted DDR3; a port can reuse them for the same
RAM part, or recompute from the DDR3 datasheet + the actual device.)

## Init sequence (`0x7f198c`) — CONFIRMED

1. **Warm-boot check**: read `0xC0000020`; if **bit7 (0x80) set**, DRAM is already up → return
   "warm" (2), skip init. Else cold init (return 1). (Matches boot tags `SCBWRM`/`SCBCLD`.)
2. **Power-stable delay**: busy-wait on the timer `0xC0000098` until the counter ≥ `0x1F4` (500).
3. **Table phase 1 (`DDRTBL`/`DDR3 table init`)**: walk the table at `0x7f1a0c`, writing each
   `(reg, val)` pair (`str val,[reg]`) until a `reg == 0` terminator. Writes the timing/mode
   registers above and kicks the controller with `0xC0000020 = 0x3`.
4. **Calibration wait (`DDRCAL`/`DDRCND`)**: poll `0xC0000020` until **bit7 (0x80) set** = DDR
   PHY calibration / DLL lock complete.
5. **Table phase 2**: continue the table past the terminator — post-calibration writes
   (`0xC0000020 = 0x2`, `0x80200040 = 0x400`).
6. **Final MEMCFG (`DDREND`)**: read the strap register `0x801F00A4`, `>> 1`, write to
   `0xC0000064`. DRAM is now usable.

Table encoding: flat `u32` array of `[reg_addr, value, reg_addr, value, …, 0]`, applied by
`ldr r3,[tbl],#4; cmp r3,#0; ldrne val,[tbl],#4; strne val,[r3]` — a `0` reg address ends a
phase.

## Clocking / straps (boot ROM, docs/03)

`FRQUSB`/`FRQLPC`/`FRQMAC` (clock-frequency straps), `HSTRAP`/`SSTRAP` (hardware/software strap
pins), `0x801F00A4` (the MEMCFG source strap). The DDR clock + the `0xC0000064` MEMCFG come from
these straps — read, don't hardcode, on a different board.

## Clock / strap config (boot ROM)

DDR timing + the whole clock tree derive from board **straps**. RE of the clock setup
(`0x7f1540`-`0x7f1b2c`):

| Reg | Role | Evidence |
|-----|------|----------|
| **`0x801F00A4`** | primary **strap / clock-select** register — read the board straps here: bits[1:0] = clock mode, [3:2], [19:16], and [27:22] = clock-select field; also re-written as a mirror after decode | `tst #3` @ `0x7f1af8`, writes @ `0x7f16f4` |
| **`0x80200000` + `0x88`** | **clock / PLL control** — programmed from the decoded strap bit-fields | `str [r2,#0x88]` @ `0x7f1574`/`0x7f188c` |
| `0x80200000` + `0x40` | post-init write `0x400` | `0x7f192c` |
| `0x80200000` + `0x58` | clock/reset | `0x7f3c2c` |

Clock-select decode (`0x7f1af8`): if `0x801F00A4` bits[1:0] ≠ 0, the clock comes from bits[27:22]
directly; else from a config field (1→`0x04000000`, 2→`0x08000000`, 3→`0x10000000`). The result
feeds the **DDR/clock coupling**: `0xC0000064` MEMCFG = `min(configured, clock >> 1)` — i.e. the
DDR data rate is capped at half the core clock. The derived outputs are reported as
`FRQUSB`/`FRQLPC`/`FRQMAC` (USB/LPC/MAC clock frequencies).

The `0x80200000` block is the Gromit-XE **clock/PLL/reset controller** (distinct from IC1 at
`0x80F00000`, docs/12) — add it to the physical map (docs/13).

## For the OpenBMC / U-Boot port

- This is the **DRAM bring-up** a Gromit-XE U-Boot SPL (or the `gxp`-style U-Boot fork) must do
  before relocating. Port it as a `lowlevel_init` / `dram_init`: the warm-boot bit7 check, the
  timer delay, the two table phases with the bit7 calibration poll, and the strap-derived
  `0xC0000064` MEMCFG.
- The register block `0xC0000000` is the same ASIC window the drivers use (timer `+0x98`, etc.).
- Keep the FW 1.94 table values as the known-good reference for the G7's DDR3; re-derive only if
  targeting different RAM.

## Open

- Exact per-register timing-field decode (which bits in `0xC0000028`/`2C`/`40` = tRCD/tRP/tRAS/
  CAS) — needs the Gromit-XE memory-controller datasheet; the *values* above are sufficient to
  boot the same hardware.
- DRAM size: the `0xC0000064` MEMCFG (strap-derived) encodes geometry; the 32 MB figure in the DT
  is a placeholder — confirm from the strap/MEMCFG on hardware.
