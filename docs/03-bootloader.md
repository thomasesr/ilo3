# iLO 3 boot ROM — raw ARM926 code at flash `0x7f0000`

Date: 2026-10-20. Tool: Ghidra 12.1.4 headless, `ARM:LE:32:v5`, BinaryLoader base `0x7f0000`.
Source: `firmware/staging/ilo3_194.bin`. Carved copy: `decompiled/bootrom_7f0000.bin` (64 KB).

## The key correction: code is raw, resources are compressed

**UPDATE (see docs/04):** the `0x440` image IS the main OS code — a 12 MB INTEGRITY ARM926
monolith whose instructions are byte-phase-1 shifted, so it first *looked* code-free. The boot
ROM below is a *separate* first-stage loader. Corrected table:

| Flash offset | `iLO3` hdr | Build     | Content (LZSS → decompressed)                |
|--------------|-----------|-----------|----------------------------------------------|
| `0x000440`   | yes       | v1.94.2   | ~12 MB **main INTEGRITY OS** (code+data+web); base 0, see docs/04 |
| `0x6f0000`   | yes       | v0.8.42   | **INTEGRITY kernel + BSP** (~795 KB phase-1 ARM; MMU/FCSE, MAC `umac.c`) — see docs/10 |
| `0x770000`   | yes       | v0.8.42   | identical copy of `0x6f0000` (2nd bank / kernel)   |
| **`0x7f0000`**| no        | **boot**  | **~64 KB RAW ARM926 code (XIP) — the loader**|
| `0x7e0000`   | —         | —         | certificates (DER/PEM)                       |
| `0x800000`   | —         | —         | image signature block                        |

Ghidra's Aggressive Instruction Finder found **0 functions** in all three decompressed LZSS
blobs (8 funcs / 46 ins across the whole 12 MB — noise), but **65 functions / 2185 instructions**
in the raw `0x7f0000` region, all cleanly decoded as ARMv5. That is the proof the CPU is
**ARM926EJ-S / ARMv5** (see `docs/02-soc-id.md`) — and the proof the executable lives raw in
flash, executed in place, not inside the compressed payloads.

## What the boot ROM is / does

Strings + disassembly identify a signed-image boot loader (`BOOT Version 20150624140241`,
internal `v 0.1.79+ 25-Jun-2015`). Pipeline: locate image → **validate signature — RSA-2048 over a SHA-256 hash** (`Starting
hash`, `Signature valid`, `unsigned - will not boot an image`; public key in the embedded cert
@`0x7e0000`, HP root — see **`docs/31`**) → **decrypt** (`Starting
decrypt`) → **decompress** (`Decompressing: compressed size … decompressed size …`,
`Unknown compression type`) → **copy/boot** (`Copying image`, `Loading type`, `ok - boot to`).
It recognizes magics `iLO3iLO4iLO5iLO6iLO7` — the same loader lineage spans iLO3–7.

**Note on encryption:** the loader has a decrypt stage, so iLO images *can* be encrypted. FW
1.94's resource image at `0x440` decompressed with plain LZSS (no decrypt needed), but other
images/sections may require the decrypt path — relevant to the owner's "unpacking" concern.

## Hardware facts recovered (for the OpenBMC port)

- **MMIO register base `0xC0000000`** (12 refs; also `0xC0000C00`, `0xC04C4B00`). Example from
  disasm: `mov r0,#0xc0000000 ; ldr r0,[r0,#0x98]`. This is the Gromit-XE peripheral window.
- **ARM exception model (confirms ARM926 + MMU, not Cortex-M):** handlers print `SPSR`,
  `Prefetch Abort ifsr=`, `Data Abort FAR=…, dfsr=`, `Undefined Instruction`, `FIQ`, IRQ vector
  — `ifsr`/`dfsr`/`FAR` are CP15 fault registers, present only on A/R-class ARM (here ARMv5 926).
- **DRAM bring-up — FULLY MAPPED (`docs/30`):** DDR3 init fn `0x7f198c` + (reg,val) table `0x7f1a0c`.
  Controller `0xC0000018-0x48`; `0xC0000020` = ctrl (write 0x3 start / read bit7 ready / write 0x2),
  timing `0x28/2c/30/40`, refresh `+0x48`, MEMCFG `0xC0000064` (strap `0x801F00A4 >>1`). Phases:
  warm-check bit7 → 500-tick delay → table phase1 → cal-wait bit7 → table phase2 → MEMCFG.
- **Clocking / straps:** `FRQUSB`, `FRQLPC`, `FRQMAC` (USB/LPC/MAC clock freqs), `HSTRAP`,
  `SSTRAP` (hardware/software strap pins), `FPGA rev`, `ASIC rev`, `CPLDRD` (CPLD read).
- **Boot-state tags:** `SCBNON/SCBCLD/SCBWRM/SCBEND` (SCB cold/warm reset states), `NVROVR`
  (NVRAM override countdown — recovery/escape hatch).

## Open / next

- **Load/run address — RESOLVED: `0xFFFF0000`.** The boot ROM starts with the ARM exception
  vector table (`0x7f0000`: `b 0x7f002c` reset, undef/swi/pabt/dabt/irq/fiq) and self-references
  **`0xFFFF0xxx`** literals → true runtime base = **`0xFFFF0000`** (ARM926 high vectors). The 8 MB
  SPI flash is mapped at **`0xFF800000`–`0xFFFFFFFF`**, so flash `0x7f0000` = `0xFFFF0000`. The
  **ARM926 resets directly into the flash boot ROM** (reset handler `0xFFFF002c`; CP15/stack
  `0x20004000`/clock/DDR init → verify → boot). **No on-die mask ROM above it** (iLO 3 predates HPE
  Silicon Root of Trust) → flash boot ROM = top of trust chain. See **`docs/31`** (reflash verdict).
- **Main INTEGRITY OS code location — FOUND.** It IS the 12 MB `0x440` image (phase-1 shifted);
  base `0x00000000`, 32 MB space. See `docs/04-main-os.md`.
- Map the `0xC0000000` peripheral block (UART/I2C/SMBus/GPIO/SPI-flash/MAC offsets) from the
  boot ROM's register accesses — this feeds the eventual `meta-ilo3` devicetree.
