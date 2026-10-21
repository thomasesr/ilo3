# iLO 3 SoC identification — `ilo3_194.bin`

**Finding: the iLO 3 management ASIC is HP's own SoC, codename "Gromit-XE" (abbrev. `GXE`).**
**CPU core: ARM926EJ-S (ARMv5TEJ) — Ghidra processor `ARM:LE:32:v5`.**
**GXE variant part number: 438893-503.**

Date: 2026-10-20. Tool: custom LZSS decompressor (`tools/ilo_lzss.py`) + `strings`.
Source: `firmware/staging/ilo3_194.bin` (FW 1.94, SHA256 `d84774ac…f754903`).
CPU core, variant PN, and Ghidra processor supplied by project owner (authoritative);
cross-checks against the binary below are consistent with ARMv5TEJ.

## How it was found

1. Payload from offset `0x40` is **compressed, not encrypted** (confirmed). Raw `strings`
   showed readable-but-fragmented ASCII — literal bytes broken up by 2-byte match tokens,
   the signature of an LZ stream. A truly encrypted payload yields uniform ~8.0 entropy and
   zero readable text; this image has mixed 2.8–8.0 window entropy.
2. Compression = **HP LZSS**, 0x1000-byte (4096) sliding window, identical to the algorithm
   in Synacktiv `ilo4_toolbox` (`scripts/iLO4/ilo4lib.py :: decompress`). Control byte,
   MSB-first bits: `1`=literal (1 byte); `0`=back-ref (2 bytes), `len=(b0>>4)+3`,
   `dist = wpos - (b1 | ((b0&0xf)<<8)) - 1`. Shared HP ROM lineage across iLO3/4/5.
3. Decompressing `0x40..0x660000` → 11.85 MB of clean text (ported to py3 as
   `tools/ilo_lzss.py`). Strings became fully readable.

## Evidence (decompressed strings)

- `*Hello There. This is part of the Gromit-XE verification.` ← explicit codename
- `iop_GromitHwGetSendBuffer` / `…GetRecvBuffer` / `…PutSendBuffer` / `…PutRecvBuffer`
  — the IO-processor ↔ host "Gromit" hardware mailbox driver.
- `chif_fast_post_s: GromitCtrlTestGo not set` — CHIF (channel interface to host) + Gromit ctrl.
- `GXE_JTAG_MODE`, `GXE_JTAG_FPGA_SEL`, `GXELAN_L`, `GXE` — `GXE` register/block prefix = Gromit-XE.
- `ASIC RTL: 0x%08lx`, `ASIC Revision ID`, `Get iLO ASIC Release Level`, `ASIC: 8..16`
  — chip revision/RTL version read from an on-die register at runtime; firmware refers to the
  part generically as "iLO ASIC" (no 3rd-party vendor string → HP-designed silicon).
- `ASIC supports 8 I2C engines`, `sawbase_memory()` (shared-memory window to host),
  `smifpkt_0050 … flash part` (SPI/SMIF flash controller) — integrated peripherals.

## SoC facts established

- **ASIC codename:** Gromit-XE (`GXE`). "Gromit" was the iLO 2 ASIC; "-XE" is the iLO 3 part.
- **Silicon:** HP-proprietary SoC (not Aspeed/Nuvoton/Emulex-PILOT). ASIC rev/RTL is a register.
- **RTOS:** Green Hills **INTEGRITY** (`rtos-i10.0`, `INTEGRITY-libs`, `__ghs_*`, build path
  `/home/delphyne2/autobuild/intbuild112210-…/rtos-i10.0/`). ARM target.
- **Integrated blocks (from driver strings):** 8× I2C engines, `GXELAN` on-die NIC, SPI/SMIF
  flash controller, JTAG/FPGA-program block, CHIF/sawbase host shared-memory channel, KVM/video.
- **Crypto:** OpenSSL 0.9.8e / SSL-C 2.1.1, Allegro toolkit, Corsec FIPS module task names
  (`Corsec_asic_server`, `Corsec_allegro/klite/rsasslc`) — "Corsec" = FIPS lab, not a codename.

## CPU core (resolved — confirmed from real code)

- **ARM926EJ-S**, architecture **ARMv5TEJ** (ARM + Thumb-1 + DSP-E + Jazelle; MMU + caches).
- Ghidra processor/variant: **`ARM:LE:32:v5`** (little-endian). Do NOT use v6/v7/v8 decoders.
- GXE SoC variant PN **438893-503**.
- **Verified by disassembly:** the raw boot ROM at flash `0x7f0000` disassembles cleanly as
  ARMv5 (65 functions, 2185 instructions) — matched STMDB/LDMIA prologues, `umull`+shift
  divide-by-constant, `0xC0000000` MMIO. Its exception handlers reference CP15 fault registers
  (`ifsr`/`dfsr`/`FAR`/`SPSR`), which exist only on A/R-class ARM — consistent with ARM926, and
  excluding the Cortex-M reading that an earlier (contaminated) pass had produced. Full detail:
  `docs/03-bootloader.md`.

## Methodology warning (learned here)

A **permissive decoder fabricates instructions from non-code bytes.** During analysis, forcing
`ARM:LE:32:v8` (Thumb) onto a data/mis-decompressed region produced a fake but plausible
"Thumb-2" picture — `CBZ/CBNZ/IT/MOVW/BFI` and even **NEON** (`vcadd.f16`, `vaba.u32`). NEON on
an ARM926 is impossible → that output was proof the bytes were **not code** (or not cleanly
decompressed), not evidence of Thumb-2. Always decode with the *correct, minimal* ISA
(`ARM:LE:32:v5`); if v5 yields garbage where v8 yields exotic SIMD, the input is the problem.

## Resolved since first draft

- **Decompressor fidelity — OK.** `tools/ilo_lzss.py` now carries two implementations
  (continuous `decompress_stream` + faithful chunked `decompress_chunked` port of
  `decompress_all`) and `--verify` proves they are **byte-identical** and match the header's
  `decompressed_size` (within trailing pad). Decompression was never the problem.
- **Why early disasm failed — wrong section, not bad unpack.** The LZSS payloads are
  *resources* (web UI, strings, data), containing ~no CPU code. The code is **raw/uncompressed
  at flash `0x7f0000`** (boot ROM, XIP). See `docs/03-bootloader.md`.

## Open / not yet determined

- **Main INTEGRITY OS code location + load base.** Boot ROM is found; the main OS executable is
  not in the 12 MB resource blob. Likely relocated to a high RAM base (image header hints
  `0x7ffe0000`). Find it, then disassemble at its true base.
- **Boot-ROM runtime address** (flash-mapped XIP vs copied-to-SRAM) — trace the reset vector.
- Numeric ASIC Revision ID / RTL value for this specific board (register read at runtime).

## Impact on the OpenBMC port

Gromit-XE is **not** `meta-gxp` (GXP = iLO4/iLO5-era, different ASIC). `openbmc/meta-hpe/meta-gxp`
is therefore a **structural template only**, not reusable silicon support. A port needs a new
`meta-ilo3` layer with its own devicetree describing the Gromit-XE blocks above — and mainline
Linux has no Gromit-XE support, so this is a from-scratch SoC bring-up, not a board add.

The CPU being **ARM926EJ-S / ARMv5TEJ** also means the OpenBMC tune is **armv5te**, *older* than
`meta-gxp`'s `armv6-novfp` — another reason meta-gxp is template-only. Note too that ARMv5 is at
the edge of modern kernel/toolchain support; mainline Linux still builds ARMv5 (`arch/arm`,
`OABI`/EABI soft-float), but Yocto ARMv5TE tunes and current glibc are a real constraint to
validate early.
