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

> **Partly SUPERSEDED — see "RE update (2026-10-21)" below.** The clock-select strap is
> `0xD1000000` (bits[1:0]), not `0x801F00A4` (which is only the MEMCFG `>>1` source), and
> the PLL (`+0x88`) is reprogrammed only on the clock-mode==0 branch — not on the operative
> table-walk path. The table/addresses below are still valid; the *interpretation* is corrected.

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

## RE update (2026-10-21): clock-select + the two boot paths — PLL gating RESOLVED

Deeper disasm (capstone, `decompiled/bootrom_7f0000.bin`) of the clock/DDR caller
resolves the open clock question and corrects two earlier points.

### Clock-select strap = `0xD1000000` (NOT `0x801F00A4`)

The parent fn (prologue `0x7f11e8`) sets `r5 = 0xD1000000`, `r6 = 0xC0000000`, then:
`r4 = *0xD1000000 & 3` — this is the **clock-select mode**. (`0x801F00A4` is used
*only* for the final MEMCFG, `>> 1` → `0xC0000064`; it is not the clock selector.)
`0xD1000000` is the "Wallace"/companion block, read via an **indexed port**: write
an index byte to `+0x24`, read the strap word from `+0x20`.

### Two DDR/clock paths, gated by ASIC rev (`0xC00000A0` bits[23:16])

`fp = *0xC00000A0 & 0xFF0000; if fp > 0x40000 → 0x7f1950` (rev > 4), else falls into
the `0x7f1240` path (rev ≤ 4). **The two paths use different DDR timing values:**

- **rev > 4 (operative; docs/30 above):** `0x7f1950` → if `(*0xD1000000 & 3) == 0`
  jump to the strap→PLL decode (`0x7f14f4`); else `bl 0x7f198c` = the **table-walk**
  at `0x7f1a0c`, which does **NO clock/PLL reprogram** — DDR trains on the
  strap-default clock, then `0xC0000064 = *0x801F00A4 >> 1`. This is the path the
  values above (and `spl/src/ddr.rs`) implement.
- **rev ≤ 4:** `0x7f1240` loads DDR timings from pc-relative tables `0x7f11b8`/`0x7f11d0`
  indexed by `r4` (values `0x339`/`0x341b2800`/`0x00007a47`… — **different** from the
  `0x7f1a0c` set), and on `r4 == 3` runs the same `0x7f14f4` strap decode.

### PLL (`0x80200000+0x88`) is programmed ONLY on clock-mode == 0

The only write to the PLL control (`0x7f1578: str r0,[0x80200000,#0x88]`) is inside
the `0x7f14f4` strap-decode block, reached only when `(*0xD1000000 & 3) == 0` (rev>4)
or `r4 == 3` (rev≤4). That block also computes the DDR timing regs **live** from strap
bitfields rather than using a static table. **Because the real FW-1.94 DDR values live
in the static `0x7f1a0c` table, the fitted board takes the table-walk path → clock mode
is non-zero → the PLL is not reprogrammed.** So a first-stage port needs no PLL
programming; it inherits the strap-default reset clock. (`spl/src/ddr.rs::clock_mode()`
checks `*0xD1000000 & 3` at boot and warns if it is 0.)

### Confirmed `0x7f1a0c` table (exact ROM order)

Phase 1: `0x30=0x1520, 0x34=0x5, 0x38=0, 0x3c=0, 0x28=0x552C9180, 0x2c=0x50C34F,
0x40=0xAA9658E3, 0x48=0x7D, 0x24=0x22, 0x18=0xFDF0FFF0, 0x20=0x3` (start) · term `0`.
Phase 2: `0x20=0x2, 0x80200040=0x400` · term `0`. Then MEMCFG `0xC0000064 = strap>>1`.
(The `0x80200040 = 0xC400` seen at `0x7f1388` is the **rev≤4** path, not this one.)

### Remaining HW-confirms
- ASIC rev of the fitted GXE (selects rev>4 vs rev≤4 timing set). docs/30 assumes rev>4.
- `0xD1000000 & 3 != 0` on the board (selects table-walk, no-PLL path). SPL self-checks.

## DRAM + SRAM parts identified (2026-10-21, from the board)

- **DRAM = ISSI IS43/46LD16640A = 1 Gbit LPDDR2-S4** (×16, 64M×16 = 128 MB/device). This
  **CORRECTS the "DDR3" label in this doc's title/text** — the fitted part is **LPDDR2-S4**
  (JEDEC JESD209-2), not DDR3. The firmware's "DDR3 table init" string is a misnomer (HP's generic
  DDR init path). The RE'd table (`0x7f1a0c`) + fn `0x7f198c` stay authoritative for bring-up, but
  are the **LPDDR2-S4 init sequence** (power-stable delay → mode-register writes / calibration →
  MEMCFG), so decode the per-register fields against **JESD209-2**, not the DDR3 spec. Total size
  1 Gbit/128 MB (MEMCFG-strap-derived, docs/13) — the "32 MB DT placeholder" is just a floor.
- **Pre-DDR scratch = ISSI IS62WV2568BLL async SRAM** at `0x20000000` — this is why the boot
  ROM / SPL can use stack `0x20004000` before DRAM is trained (docs/13, docs/31 open item resolved).

## Cross-check vs LPDDR2-S4 (JESD209-2) — 2026-10-21

The fitted part is 1 Gbit LPDDR2-S4 (ISSI IS43/46LD16640A, docs/13). Cross-checking the RE'd
`0x7f1a0c` table against JESD209-2. The registers are Gromit-XE **controller** registers (packed
fields; the full layout needs the controller datasheet), but several decode cleanly and the whole
sequence maps onto the LPDDR2 power-up flow. `spl/src/ddr.rs` byte-matches this table (re-verified).

### Mode-register decodes (strong — match JESD209-2 exactly)

- **`0xC0000024 = 0x22` ≈ MR1 (Device Feature 1)**: `0b0010_0010` → BL[2:0]=`010`=**BL4**,
  BT[3]=0 (sequential), nWR[6:4]=`010`=**nWR4**. (docs/30 label "timing" → refine to MR1.)
- **`0xC0000034 = 0x5` ≈ MR2 (Device Feature 2)**: `0b0101` → **RL7 / WL4**, i.e. the LPDDR2
  read/write-latency set for the **400–466 MHz** clock bin (LPDDR2-800/933). (label "mode" → MR2.)
- RL7/WL4 pins the operating point: DDR clock ≈ 400–466 MHz (800–933 MT/s), consistent with the
  `0xC0000064` MEMCFG = `min(cfg, clock>>1)` coupling (docs/30 clock section).

These are likely the MR values the controller issues via MRW during the start kick (LPDDR2 needs
MRW to MR1/MR2/MR3 + ZQ-cal MR10 at init). HYPOTHESIS: the controller reg may re-encode rather than
pass the raw MR byte — CONFIRM-ON-HW — but the bit-exact JESD209-2 match is compelling.

### Sequence vs JESD209-2 §3.3.1 power-up — structurally matches

1. **Vdd-stable delay** (timer ≥ `0x1F4`=500 ticks) → JESD209-2 `tINIT1..3` require **≥ 200 µs**
   after Vdd stable before CKE/RESET. ⚠ **SAFETY CHECK**: 500 ticks must be ≥ 200 µs at the real
   counter rate. At the TIMER_HZ placeholder (1 MHz) = 500 µs ✓; re-confirm once the rate is known.
2. **Controller config** (the timing words + MR1/MR2) loaded, then `0xC0000020 = 0x3` **start** →
   the controller runs the LPDDR2 init (RESET via MR63, ZQ Init Cal via MR10 `tZQINIT ≥ 1 µs`, MRW
   MR1/2/3). This is why there is **no explicit MR10/MRW in the table** — the controller auto-issues
   them on start.
3. **Poll `0xC0000020` bit7 = ready** → this is **ZQ-cal + PHY training complete**, NOT "DLL lock":
   **LPDDR2 is DLL-less**. (Correct the docs/03/ddr.rs "DLL lock" wording.)
4. Post-cal `0xC0000020 = 0x2`, then `0x80200040 = 0x400`, then MEMCFG = strap>>1.

### Timing words + refresh (bounded, not fully decoded)

- `0xC0000028 = 0x552C9180`, `0xC000002C = 0x50C34F`, `0xC0000040 = 0xAA9658E3` = packed
  tRCD/tRP/tRAS/tRC/tRFCab/tFAW etc. for 1 Gbit LPDDR2-S4. Exact field split needs the controller
  datasheet (standing Open), but at RL7/WL4 the required timings (tRCD/tRP ≈ 18 ns, tRAS ≈ 42 ns,
  tRFCab ≈ 130 ns for 1 Gbit, tFAW) are all in-range and representable in these 32-bit words.
- `0xC0000048 = 0x7D` (125) = refresh interval count. LPDDR2-S4 **tREFIab = 3.9 µs** (≤ 85 °C;
  1.95 µs above 85 °C). Verify `125 × tCK_ctrl ≤ 3.9 µs` at the real controller clock (CONFIRM-ON-HW).
- `0xC0000018 = 0xFDF0FFF0` = row/col/bank address mask for a 1 Gbit ×16 device (8 banks, 14 row,
  10 col); exact mapping needs the controller datasheet.

### Verdict

The table is a **valid LPDDR2-S4 init for a 1 Gbit ×16 device at RL7/WL4 (~400–466 MHz)** — the
MR1/MR2 decodes are bit-exact against JESD209-2, and the sequence matches the power-up flow. Since
the values are RE'd from the shipping ROM that boots this exact RAM, they are correct by
construction; this cross-check validates the *port* (`ddr.rs` matches) + the *understanding* (it's
LPDDR2, DLL-less, ~400–466 MHz). Remaining HW-confirms: the ≥200 µs power delay vs the real counter
rate, the tREFI math, and the packed timing/mask field layout (controller datasheet).
