# iLO 3 main OS — INTEGRITY monolith (ARM926, base 0x00000000)

Date: 2026-10-20. Tool: Ghidra 12.1.4 headless, `ARM:LE:32:v5`.
Source: main image `iLO3` header @ flash `0x440` → LZSS payload `0x880..0x66d380`
→ `tools/ilo_lzss.py` → 12 MB. Realigned copy: `decompiled/os_main_base0.bin`.

## Correction to docs/03

docs/03 said the 12 MB payload was "resources, not code." **That was wrong.** The whole 12 MB
image IS the main Green Hills INTEGRITY OS — a flat ARM926 monolith of code + rodata + data.
It looked code-free only because the ARM instructions sit at **byte-phase 1** in the
decompressed stream (every `stmfd` word is at file offset ≡ 1 mod 4), so every 4-aligned scan
and every base-0/`0x7ffe0000` disassembly missed them.

Evidence once realigned: Ghidra's Aggressive Instruction Finder on `img.bin[1:]` at base 0
finds **17,152 functions / 1,489,081 instructions**; `mov`(e1a0) appears ~207 k times.

## How to disassemble it

```
python3 tools/ilo_lzss.py firmware/staging/ilo3_194.bin os.bin   # 12 MB, header-auto
dd if=os.bin of=os_base0.bin bs=1 skip=1                          # drop 1 lead byte -> 4-align
# Ghidra: import os_base0.bin, processor ARM:LE:32:v5, base 0x00000000
```

The decompressed stream carries one spurious lead byte before the image proper; dropping it
puts all ARM code on the 4-byte grid. (Equivalently, load the undropped image at base `0x…3`.)

## RAM load base = 0x00000000 (32 MB space)

- **BL targets resolve at base 0.** Sample function @ `0x2a2b4` (APCS frame prologue
  `mov ip,sp ; stmdb sp!,{fp,ip,lr,pc} ; sub fp,ip,#4`) calls `bl 0x2a270`, `bl 0x2b3c8`,
  `bl 0x2b320` — all land on real function entries. `swi 0x0` = INTEGRITY syscall gate.
- **Literal-pool pointers cluster in `0x00000000`–`0x02000000`** (dominant in 0–1 MB).
- Decompressed-image header word @+0x18 = `0x02000000` → **32 MB** DRAM/address space, matching
  the pointer range. (The earlier `0x7ffe0000` header word is not the code base — likely a
  flash-window or config address.)
- Compiler: Green Hills, **APCS frame pointers** (r11/fp, r12/ip), ARM (not Thumb) throughout.

## Still open

- Exact kernel link details within the 32 MB (vector-table location: low `0x0` vs high
  `0xFFFF0000`; kernel vs per-AddressSpace segment bases). Statistical base-fit is noisy because
  `stmfd` is ubiquitous; refine by tracing the boot ROM's copy/jump target and the reset path.
- How the boot ROM (`0x7f0000`) loads this image: its `load_address` field is `0xffffffff`
  (not the `0x20001000` used by the type-2 images), so a different loader path / embedded base
  applies — trace `Copying image` / `Decompressing` in the boot ROM against this image.
