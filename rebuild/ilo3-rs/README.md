# ilo3-rs — clean-room iLO 3 (Gromit-XE) BMC firmware in Rust

A `no_std` Rust firmware for the HP iLO 3 BMC (Gromit-XE ASIC, ARM926EJ-S /
ARMv5TEJ) on the HP ProLiant **DL380p Gen7**. Clean-room rewrite of the EOL HP
firmware — see `../../PLAN-rust.md` (roadmap), `../../docs` (RE spec),
`../../docs/47-rtos-realtime-analysis.md` (execution-model decision: embassy).

## Status: Phase 1 — bring-up skeleton

- ✅ Builds + links for `armv5te-ilo3` (custom target) → correct ARM32 LE ELF, entry `0x20000000`.
- ✅ Reset/startup: SVC + IRQ/FIQ masked → CP15 (MMU + caches off) → IRQ + SVC
  stacks → zero `.bss` → `rust_main`. (`src/main.rs` `global_asm!`.)
- ✅ Polled console UART driver (`src/uart.rs`, Gromit-XE 8-bit @ `0xC00000F0`, docs/05).
- ✅ `critical-section` impl (`src/critsec.rs`) — CPSR IRQ-mask, single core (no
  LDREX on ARMv5 → `core::sync::atomic`/embassy-sync route through it).
- ✅ 2-bank irqchip HAL (`src/intc.rs`, docs/12) — 64 IRQs, ENABLE/DISABLE/PENDING/EOI;
  `init()` masks+clears (exercised).
- ✅ 64-bit monotonic timer + `delay_us` (`src/timer.rs`, `0xC0000098`, docs/12);
  Wallace clockevent (`0xD1000000`, IRQ36) register-exact, wired once IRQs are live.
- ✅ ARM exception vectors + root IRQ dispatch (`src/vectors.rs`) — 8-entry table,
  `_irq_entry` → `irq_dispatch` → per-line handlers; disasm-verified.
- ⏳ **IRQ delivery gated on the MMU sub-phase**: ARM926 has no VBAR, so the vector
  base is `0x0`/`0xFFFF0000`, neither our DDR — Phase 1 drives time off the polled
  counter; the IRQ path is ready and unmasks once the MMU maps RAM at the base.
- ✅ **embassy executor runs on ARMv5** (`embassy-executor` 0.10, `platform-spin`).
  `rust_main` starts it with two async tasks (`heartbeat`/`ticker`) that interleave
  via `yield_now`. Proven by the linked ELF (task pools, `Executor::run`, poll fns).
- ✅ **ARMv5 atomics friction solved** — ARM926 has no LDREX, and the stock target
  declares no atomics, so `core::sync::atomic` (used by embassy/cordyceps) wouldn't
  build. Custom target `armv5te-ilo3.json` claims `atomic-cas`+32-bit atomics; LLVM
  then emits `__atomic_*` libcalls, provided in `src/atomics.rs` as single-core
  interrupt-masking shims (correct because the BMC is single-core).
- ⏳ **IRQ delivery gated on the MMU sub-phase**: ARM926 has no VBAR, so the vector
  base is `0x0`/`0xFFFF0000`, neither our DDR — Phase 1 drives time off the polled
  counter; the IRQ path is ready and unmasks once the MMU maps RAM at the base.
- ⏳ **Not yet hardware-validated** — needs a board (or a custom QEMU machine). The
  stock `qemu-system-arm -M versatilepb` is ARM926 but has a different UART/RAM map.

Next (Phase 1 cont.): `embassy-time-driver` over `src/timer.rs` (replaces the busy-wait
`delay_us` with real `Timer::after`; needs the Wallace tick IRQ, so it rides on) → MMU
bring-up (unblocks IRQ delivery) → SPL (DDR init `docs/30` + SPI-NOR load) → HW validation.

## Layout

```
Cargo.toml              workspace (profiles: panic=abort, opt=z, LTO)
rust-toolchain.toml     pin nightly + rust-src (tier-3 target → build-std)
armv5te-ilo3.json       custom target (armv5te + atomic-cas; see atomics.rs)
.cargo/config.toml      target=armv5te-ilo3.json, build-std, json-target-spec, -Tlink.x
firmware/               the firmware binary crate
  link.x                memory map (DDR 0x20000000, 16M; docs/13)
  build.rs              puts link.x on the linker search path
  src/main.rs           reset/startup (IRQ+SVC stacks) + rust_main + embassy tasks
  src/uart.rs           polled console UART HAL
  src/atomics.rs        __atomic_* libcall shims (single-core, no LDREX)
  src/critsec.rs        critical-section impl (CPSR IRQ mask, ARMv5)
  src/intc.rs           2-bank irqchip HAL (64 IRQs; docs/12)
  src/timer.rs          64-bit monotonic counter + Wallace clockevent (docs/12)
  src/vectors.rs        ARM exception vectors + root IRQ dispatch
```

## Build

```sh
cargo +nightly build --release
# flat image for flashing:
llvm-objcopy -O binary \
  target/armv5te-ilo3/release/ilo3-fw ilo3-fw.bin
```

Requires the `nightly` toolchain with `rust-src` + `llvm-tools-preview`
(`rustup component add rust-src llvm-tools-preview`). The custom `armv5te-ilo3`
target is tier-3 (no prebuilt core), so `-Zbuild-std` compiles core/alloc from
source; `-Zjson-target-spec` enables the `.json` target — both configured in
`.cargo/config.toml`, no command-line flags needed.

## Boot model (Phase 1)

Single flat image linked to **run from DDR at `0x20000000`** (VMA = LMA), as if
dropped there by an SPL/loader/debugger. The first-stage loader (SPI-NOR XIP →
DDR init `docs/30` → copy → jump) is a later sub-phase. Unsigned boot on real
hardware uses the sig-check flash patch (`docs/31`, offset `0x7f38bc`).
