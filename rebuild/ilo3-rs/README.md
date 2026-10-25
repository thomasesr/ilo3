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
- ✅ **MMU bring-up + live IRQ delivery** (`src/mmu.rs`): ARMv5 one-level (1 MB
  section) table — identity-map all 4 GB (docs/10 VA=PA), MMIO device / DDR
  write-back cached, `L1[0]`=fault to catch nulls; **high vectors** (SCTLR V=1 →
  `0xFFFF0000`) with section `0xFFF00000` aliased to a reserved top DDR MB
  (`_vectors_phys` = `0x20F00000`, link.x) so the vector table is writable. CP15
  enable sequence disasm-verified. Vectors installed, Wallace tick (IRQ 36) routed
  to a counting handler, CPSR I-bit cleared → `heartbeat` prints `irq_ticks`.
  (Wallace reload rate + tick ack still CONFIRM-ON-HW.)
- ✅ **embassy-time Driver + real async sleep** (`src/time_driver.rs`): global
  `embassy_time_driver::Driver` — `now()` = the 64-bit counter (1 MHz, matched to
  `tick-hz-1_000_000`) + a standalone heapless timer queue (cap 16). Tick-based
  wake: `schedule_wake` parks the waker; the live Wallace IRQ calls `on_tick` →
  `next_expiration` wakes elapsed deadlines. `heartbeat`/`ticker` now use
  `Timer::after` (interrupt-driven), not busy-wait. (Resolution = Wallace period.)
- ✅ **embassy executor runs on ARMv5** (`embassy-executor` 0.10, `platform-spin`).
  `rust_main` starts it with two async tasks (`heartbeat`/`ticker`) that interleave
  on `Timer::after` deadlines. Proven by the linked ELF (task pools, `Executor::run`, poll fns).
- ✅ **ARMv5 atomics friction solved** — ARM926 has no LDREX, and the stock target
  declares no atomics, so `core::sync::atomic` (used by embassy/cordyceps) wouldn't
  build. Custom target `armv5te-ilo3.json` claims `atomic-cas`+32-bit atomics; LLVM
  then emits `__atomic_*` libcalls, provided in `src/atomics.rs` as single-core
  interrupt-masking shims (correct because the BMC is single-core).
- ✅ **SPL / first-stage loader** (`spl/` crate): the HP boot-ROM replacement
  (docs/31), XIP from flash at `0xFFFF0000` (= flash `0x7f0000`, reset target).
  Reset vectors + CP15 (MMU/caches off) + scratch SP `0x20004000` → **DDR3 init
  register-exact from docs/30** (`spl/src/ddr.rs`) → raise SP into high DRAM →
  copy main firmware flash→DDR → jump `0x20000000` (`spl/src/flash.rs`; flash is
  XIP-readable so load = memcpy). `tools/pack-flash.py` assembles the 8 MB flash
  image (payload+header @0, SPL @`0x7f0000`) — byte-verified. SPL flat bin 1.4 KB.
  `ddr.rs` is a verbatim port of the boot-ROM `0x7f1a0c` table (byte-verified) with
  a bounded cal-poll (timeout → halt, no infinite spin) and a `clock_mode()` self-
  check. Clock gap resolved by deeper RE (docs/30): the operative path reprograms
  no PLL — DDR trains on the strap-default clock (CONFIRM-ON-HW via SPI programmer).
- ⏳ **Not yet hardware-validated** — needs a board (or a custom QEMU machine). The
  stock `qemu-system-arm -M versatilepb` is ARM926 but has a different UART/RAM map.

**Phase 2 is complete** (bar the CHIF-dependent SMBIOS path): all 6 register drivers + five
services — **power**, **sensors**, the safety-critical **thermal/fan** controller (live embassy
task, docs/42 fail-safe: boot=MAX, sensor error→MAX, critical→shutdown), **psu** (panda telemetry),
and **drives** (backplane SEP). Phase 1 (MMU, live IRQ, async time, SPL) is done.

**Phase 3 (networking) is built** (`src/net.rs`): a full smoltcp 0.12 TCP/IP stack + DHCPv4 client
as a live embassy task, sitting on a `MacRing` seam. It is **HW-gated at the datapath** — the MAC
TX/RX DMA ring is RE-confirmed not statically recoverable (docs/11: the driver uses INTEGRITY DMA
object methods, not descriptor-base registers), so `StubRing` passes no traffic until the ring is
reconstructed on a live board. The stack, DHCP and socket plumbing are ready for that drop-in.

Phase 4 (crypto) has started: the **software entropy pool** (`src/rng.rs` + `chacha.rs`, docs/32)
is live — a ChaCha20 sponge/DRBG seeded from boot timer-jitter and continuously stirred by IRQ
timing + fan-tach/temperature noise, exposed as `rand_core::RngCore`. Its output is **UNVERIFIED**
(needs a NIST SP 800-90B assessment on real hardware before any key comes from it).

The **TLS 1.3 crypto primitives** are also in (`src/crypto.rs`): ChaCha20-Poly1305, SHA-256, HKDF,
X25519 — vetted RustCrypto crates that build clean on ARMv5, keyed from the entropy pool, with a
power-on self-test (KATs + AEAD round-trip) run at boot.

A **heap/global allocator** is now in too (`src/heap.rs`, ~14.8 MB DDR via embedded-alloc), so
`alloc` is available firmware-wide.

The **TLS 1.3 crypto core** is also in now: key schedule + record layer (`src/tls13.rs`, RFC 8446)
and the PEM/DER flash-backed **cert/key store** (`src/certstore.rs`). What remains of TLS is the
**handshake state machine** (ClientHello/extensions, ServerHello/key_share, Certificate +
CertificateVerify, Finished) + **X.509 chain verification** — the large negotiation part, best
validated against a real TLS client, which needs working networking first.

**Phase 7 (remote console) is built** — the docs/46 replacement, clean-room, all three parts:

- **7b KVM video**: `ws.rs` WebSocket framing (RFC 6455), `rfb.rs` RFB 3.8 server (RFC 6143),
  `hextile.rs` 16×16 tile encoder, `framebuf.rs` dirty-tile framebuffer seam, `kvm.rs` the
  TLS→WS→RFB embassy task, `novnc.rs` a built-in browser viewer, `capture.rs` the dvccap HAL
  scaffold (`docs/14`, CONFIRM-ON-HW).
- **7c vUSB/vMedia**: `hid.rs` HID keyboard/mouse, `usb_msc.rs` BOT/SCSI + `BlockDevice`,
  `vmedia.rs` image backend, `uhci.rs` UHCI register trap + frame-list walk, `vusb.rs` dispatch.
- **7a VSP/SOL**: `vsp.rs` line-protocol bridge scaffold + `vsp`/`textcons` CLI verbs.

A host-side **harness** (`../harness/`, x86_64) `#[path]`-includes the pure modules and runs their
tests: **10 pass** (WS accept-key vector, RFB ServerInit layout, Hextile round-trip + encoder
cases, SCSI parse/inquiry/capacity, HID reports). The datapaths stay HW-gated on the same backlog
below (MAC ring, CHIF shared-mem, capture registers).

Next: the TLS handshake state machine (crypto on its own task so a slow handshake can't stall the
executor, docs/47), then the management services (IPMI/SSH/SNMP/HTTPS) and the host CHIF interface
(also unblocks SMBIOS, docs/22). Hardware-validation backlog for the incoming SPI programmer, in
rough priority: **MAC DMA-ring reconstruction** (unblocks all networking + the TLS E2E test),
DDR/clock bring-up, the entropy-quality assessment, and the fan/sensor/PSU SDR placeholders +
CONFIRM-ON-HW items (incl. capture/UHCI/vMedia from Phase 7).

## Layout

```
Cargo.toml              workspace (members: firmware, spl; panic=abort, opt=z, LTO)
rust-toolchain.toml     pin nightly + rust-src (tier-3 target → build-std)
armv5te-ilo3.json       custom target (armv5te + atomic-cas; see atomics.rs)
.cargo/config.toml      target, build-std, json-target-spec (linker script per-crate in build.rs)
firmware/               the main firmware binary crate (runs from DDR @ 0x20000000)
  link.x                memory map (DDR 0x20000000, 16M; docs/13)
  build.rs              search path + emits -Tlink.x
  src/main.rs           reset/startup (IRQ+SVC stacks) + rust_main + embassy tasks
  src/uart.rs           polled console UART HAL
  src/debugsh.rs        bring-up serial debug shell (peek/poke; feature-gated)
  src/dbglog.rs         debug log ring + tee (host-pull early-boot buffer)
  src/dbgchif.rs        serial-over-CHIF debug service (host-pull; feature-gated)
  src/atomics.rs        __atomic_* libcall shims (single-core, no LDREX)
  src/critsec.rs        critical-section impl (CPSR IRQ mask, ARMv5)
  src/intc.rs           2-bank irqchip HAL (64 IRQs; docs/12)
  src/timer.rs          64-bit monotonic counter + Wallace clockevent (docs/12)
  src/vectors.rs        ARM exception vectors + root IRQ dispatch
  src/mmu.rs            ARMv5 MMU: identity 1 MB sections + high-vector alias
  src/time_driver.rs    embassy-time Driver (counter + Wallace-tick wake queue)
  src/cpld.rs           CPLD/pwrmicro bit-addressable window (UID/PSU/fan latch; docs/08,09)
  src/spinor.rs         SMIF flash controller read/erase/program (docs/15,17)
  src/i2c.rs            8-engine I2C/SMBus controller (docs/16)
  src/peci.rs           native PECI engine, CPU temperature (docs/26)
  src/fan.rs            CPLD PWM/tach scan-chain fan control (docs/27)
  src/mac.rs            GXELAN MAC + MDIO, 2 NICs (docs/11; datapath = Phase 3)
  src/power.rs          host power control: on/off/reset/NMI (docs/24)
  src/sensors.rs        temperature read mechanisms: LM75/PECI (docs/25)
  src/thermal.rs        fan control service + FAIL-SAFE, live task (SAFETY-CRITICAL; docs/42)
  src/psu.rs            panda PSU telemetry over I2C (docs/19,20)
  src/drives.rs         drive backplane SEP: UID/presence/health (docs/23)
  src/net.rs            smoltcp TCP/IP stack + DHCP over the MAC (Phase 3; ring HW-gated, docs/11)
  src/chacha.rs         self-contained ChaCha20 block/permute (CSPRNG core; docs/32)
  src/rng.rs            software entropy pool + ChaCha20 CSPRNG, RngCore (docs/32)
  src/crypto.rs         TLS 1.3 primitives (AEAD/SHA-256/HKDF/X25519) + power-on self-test
  src/heap.rs           global allocator over the DDR heap (~14.8 MB; embedded-alloc)
  src/tls13.rs          TLS 1.3 key schedule + record protection (RFC 8446)
  src/certstore.rs      TLS cert/key store: PEM/DER, flash-backed (docs/32)
  src/chif.rs           CHIF host-interface transport, BMC side (docs/28)
  src/smbios.rs         host SMBIOS inventory parser (CPU/memory; docs/22)
  src/chif_server.rs    CHIF channel->service routing (IPMI-over-PCI; docs/28)
  src/users.rs          account DB + privilege bitmap + argon2 hashing (docs/38)
  src/nvram.rs          flash-backed config K-V store, wear-leveled (docs/38)
  src/webauth.rs        web session auth: login + bearer sessions + Auth extractor
  src/ipmi.rs           IPMI 2.0 message core + command dispatch (docs/36)
  src/kcs.rs            KCS IPMI transport, BMC-side state machine (docs/36)
  src/eventlog.rs       coalescing IML + iLO event log store (docs/39)
  src/ber.rs            minimal ASN.1 BER codec (SNMP subset; docs/35)
  src/snmp.rs           SNMP v1/v2c agent + traps (docs/35)
  src/clp.rs            SMASH CLP command engine for the SSH CLI (docs/34)
  src/ribcl.rs          RIBCL XML scripting handler (docs/37)
  src/hextile.rs        Hextile 16x16 tile encoder (RFC 6143; Phase 7 KVM)
  src/framebuf.rs       framebuffer + dirty-tile tracking (Phase 7 KVM seam)
  src/ws.rs             WebSocket server framing over TLS (RFC 6455; Phase 7 KVM)
  src/rfb.rs            RFB 3.8 server state machine (RFC 6143; Phase 7 KVM)
  src/capture.rs        DVC video capture HAL (docs/14; CONFIRM-ON-HW)
  src/kvm.rs            KVM remote-console task (Phase 7)
  src/novnc.rs          KVM launch page / built-in viewer (Phase 7)
  src/usb_msc.rs        USB mass storage: BOT + SCSI block commands (docs/29)
  src/hid.rs            virtual USB HID keyboard/mouse (docs/29)
  src/vmedia.rs         virtual media block-device backend (docs/29)
  src/uhci.rs           virtual UHCI host controller: registers + frame-list walk (docs/29)
  src/vusb.rs           virtual USB device set: KVM/HID + UHCI dispatch (docs/29)
  src/vsp.rs            VSP/SOL text console bridge (docs/45)
spl/                    first-stage loader (XIP @ 0xFFFF0000; boot-ROM replacement)
  link-spl.x            flash XIP map (0xFFFF0000, 64K; no .data/.bss pre-DDR)
  build.rs              search path + emits -Tlink-spl.x
  src/main.rs           reset vectors + CP15 + scratch SP + spl_main
  src/ddr.rs            DDR3 init, register-exact (docs/30)
  src/flash.rs          payload header + copy-to-DDR + jump
  src/uart.rs           minimal polled console (banners)
tools/
  pack-flash.py         assemble the 8 MB flash image (payload + SPL)
  ilo3-chif-console.py  host CHIF debug console (observe + peek/poke/drain)
```

## Build

```sh
cargo +nightly build --release          # builds both crates (firmware + spl)
# flat images:
llvm-objcopy -O binary target/armv5te-ilo3/release/ilo3-fw  ilo3-fw.bin
llvm-objcopy -O binary target/armv5te-ilo3/release/ilo3-spl ilo3-spl.bin
# assemble the 8 MB flash image (payload @0, SPL @0x7f0000) for an SPI programmer:
python3 tools/pack-flash.py ilo3-fw.bin ilo3-spl.bin flash.bin
```

Requires the `nightly` toolchain with `rust-src` + `llvm-tools-preview`
(`rustup component add rust-src llvm-tools-preview`). The custom `armv5te-ilo3`
target is tier-3 (no prebuilt core), so `-Zbuild-std` compiles core/alloc from
source; `-Zjson-target-spec` enables the `.json` target — both configured in
`.cargo/config.toml`, no command-line flags needed.

### Host harness (Phase 7 validation)

```sh
cd ../harness && cargo test   # runs the pure RFB/WS/Hextile/SCSI/HID tests on x86_64
```

## Boot model

Two stages, in one 8 MB flash image (`tools/pack-flash.py`):

1. **SPL** (`spl/`) lives at flash `0x7f0000` = `0xFFFF0000`, the ARM926 reset
   target (high vectors, VINITHI=1). On power-on it runs XIP: CP15 (MMU/caches
   off) → scratch SP `0x20004000` → DDR3 init (`docs/30`) → raises SP into high
   DRAM → copies the main firmware (header at flash base) into DDR → jumps.
2. **Main firmware** (`firmware/`) runs from DDR at `0x20000000` (VMA = LMA).

On hardware the image is written with an external SPI programmer — the clean
owner route of `docs/31` (replace the signed boot ROM with this unsigned SPL),
no sig-check patch needed since the SPL *is* the first stage. (Alternatively the
main image can be dropped at `0x20000000` by a debugger, skipping the SPL.)
