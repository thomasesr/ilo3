# ilo3-rs — clean-room iLO 3 (Gromit-XE) BMC firmware in Rust

A `no_std` Rust firmware for the HP iLO 3 BMC (Gromit-XE ASIC, ARM926EJ-S /
ARMv5TEJ) on the HP ProLiant **DL380p Gen7**. Clean-room rewrite of the EOL HP
firmware — see `../../PLAN-rust.md` (roadmap), `../../docs` (RE spec),
`../../docs/47-rtos-realtime-analysis.md` (execution-model decision: embassy).

## Status: Phase 1 — bring-up skeleton

- ✅ Builds + links for `armv5te-none-eabi` → correct ARM32 LE ELF, entry `0x20000000`.
- ✅ Reset/startup: SVC + IRQ/FIQ masked → CP15 (MMU + caches off) → SVC stack →
  zero `.bss` → `rust_main`. (`src/main.rs` `global_asm!`.)
- ✅ Polled console UART driver (`src/uart.rs`, Gromit-XE 8-bit @ `0xC00000F0`, docs/05).
- ⏳ **Not yet hardware-validated** — needs a board (or a custom QEMU machine). The
  stock `qemu-system-arm -M versatilepb` is ARM926 but has a different UART/RAM map.

Next (Phase 1 cont.): embassy executor + timer (`0xC0000098`) + irqchip
(2-bank, `0xCEFF0000`/`0x80F00000`) + `critical-section` impl (no LDREX on ARMv5).

## Layout

```
Cargo.toml              workspace (profiles: panic=abort, opt=z, LTO)
rust-toolchain.toml     pin nightly + rust-src (tier-3 target → build-std)
.cargo/config.toml      target armv5te-none-eabi, build-std=core,alloc, -Tlink.x
firmware/               the firmware binary crate
  link.x                memory map (DDR 0x20000000, 16M; docs/13)
  build.rs              puts link.x on the linker search path
  src/main.rs           reset/startup + rust_main + panic
  src/uart.rs           polled console UART HAL
```

## Build

```sh
cargo +nightly build --release
# flat image for flashing:
llvm-objcopy -O binary \
  target/armv5te-none-eabi/release/ilo3-fw ilo3-fw.bin
```

Requires the `nightly` toolchain with `rust-src` + `llvm-tools-preview`
(`rustup component add rust-src llvm-tools-preview`). The `armv5te-none-eabi`
target is tier-3 (no prebuilt core), so `-Zbuild-std` compiles core/alloc from
source — configured in `.cargo/config.toml`, no flags needed.

## Boot model (Phase 1)

Single flat image linked to **run from DDR at `0x20000000`** (VMA = LMA), as if
dropped there by an SPL/loader/debugger. The first-stage loader (SPI-NOR XIP →
DDR init `docs/30` → copy → jump) is a later sub-phase. Unsigned boot on real
hardware uses the sig-check flash patch (`docs/31`, offset `0x7f38bc`).
