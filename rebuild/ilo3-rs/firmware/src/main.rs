//! Clean-room iLO 3 (Gromit-XE, ARM926EJ-S / ARMv5TEJ) BMC firmware.
//! Phase 1 bring-up: reset → CP15 → stack → BSS → polled UART console.
//!
//! Hardware facts (RE'd, see ../../../docs):
//!   console UART  @ 0xC00000F0  data +0x00, status +0x05 (TXRDY bit5, RXRDY bit0)  [docs/05]
//!   DDR RAM       @ 0x20000000  (image runs here)                                  [docs/13]
//! Clean-room: register addresses are facts; no HP code is used.
#![no_std]
#![no_main]
#![feature(impl_trait_in_assoc_type)]

extern crate alloc; // global allocator in src/heap.rs

use core::arch::global_asm;
use core::cell::Cell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;

use critical_section::Mutex;
use embassy_executor::Executor;
use embassy_time::{Duration, Timer};
use static_cell::StaticCell;

mod atomics; // __atomic_* libcall shims (single-core, no LDREX on ARMv5)
mod critsec; // critical-section impl (CPSR IRQ mask) — registered via set_impl!
mod intc; // 2-bank Gromit-XE irqchip
mod ipmi; // IPMI 2.0 message core + dispatch (docs/36)
mod kcs; // KCS IPMI transport, BMC side (docs/36)
mod chacha; // ChaCha20 block/permute (CSPRNG core; docs/32)
mod cpld; // system CPLD/pwrmicro bit-addressable window (UID/PSU/fan latch)
mod crypto; // TLS 1.3 crypto primitives + power-on self-test (docs/32)
mod heap; // global allocator over the DDR heap region
mod peci; // native PECI engine (CPU temperature)
mod power; // host power control (on/off/reset/NMI; docs/24)
mod psu; // panda PSU telemetry over I2C (docs/19,20)
mod rng; // software entropy pool + ChaCha20 CSPRNG (docs/32)
mod mac; // GXELAN MAC + MDIO (2 NICs; datapath = Phase 3)
mod mmu; // ARMv5 MMU: identity map + high-vector alias (unblocks live IRQ)
mod net; // smoltcp TCP/IP stack over the MAC (Phase 3; ring HW-gated)
mod drives; // drive backplane SEP: UID/presence/health (docs/23)
mod fan; // CPLD PWM/tach scan-chain fan control
mod i2c; // 8-engine I2C/SMBus controller
mod sensors; // temperature read mechanisms (PECI/LM75; docs/25)
mod spinor; // SMIF SPI boot-flash controller (read/erase/program)
mod thermal; // fan control service + fail-safe (SAFETY-CRITICAL; docs/42)
mod tls13; // TLS 1.3 key schedule + record protection (RFC 8446)
mod tls_handshake; // TLS 1.3 server handshake state machine (mTLS, Ed25519)
mod tls_stream; // TLS 1.3 async stream over an async transport (Phase 5 web)
mod x509; // minimal X.509 DER parse + Ed25519 cert/chain verify (mTLS)
mod tcp; // smoltcp TCP → async embedded-io-async bridge
mod web; // picoserve HTTPS server (Phase 5)
mod webauth; // web session auth: login + bearer sessions (docs/38)
mod certstore; // TLS cert/key store (PEM/DER, flash-backed; docs/32)
mod time_driver; // embassy-time Driver over the monotonic counter + Wallace tick
mod timer; // 64-bit monotonic counter + Wallace clockevent
mod nvram; // flash-backed config K-V store (docs/38)
mod users; // local account DB + privileges + argon2 (docs/38)
mod uart;
mod vectors; // ARM exception vectors + IRQ dispatch
use uart::Uart;

// ---- reset / startup (runs at 0x20000000, the image entry) ----
global_asm!(
    ".section .text._start",
    ".global _start",
    "_start:",
    "    msr  cpsr_c, #0xD3",          // SVC mode, IRQ+FIQ masked
    // disable MMU + I/D caches (SCTLR/CP15 c1) — flat physical for bring-up
    "    mrc  p15, 0, r0, c1, c0, 0",
    "    bic  r0, r0, #0x1",           // M  = 0 (MMU off)
    "    bic  r0, r0, #0x4",           // C  = 0 (D-cache off)
    "    bic  r0, r0, #0x1000",        // I  = 0 (I-cache off)
    "    mcr  p15, 0, r0, c1, c0, 0",
    "    mov  r0, #0",
    "    mcr  p15, 0, r0, c7, c7, 0",  // invalidate I+D caches
    "    mcr  p15, 0, r0, c8, c7, 0",  // invalidate TLB
    // set up the IRQ-mode banked stack, then return to SVC for the SVC stack
    "    msr  cpsr_c, #0xD2",          // IRQ mode, IRQ+FIQ masked
    "    ldr  sp, =_irq_stack_top",    // IRQ-mode sp (top 4 KB)
    "    msr  cpsr_c, #0xD3",          // back to SVC mode, IRQ+FIQ masked
    "    ldr  sp, =_svc_stack_top",    // SVC stack (below the IRQ stack)
    // zero .bss
    "    ldr  r0, =_sbss",
    "    ldr  r1, =_ebss",
    "    mov  r2, #0",
    "1:  cmp  r0, r1",
    "    bhs  2f",
    "    str  r2, [r0], #4",
    "    b    1b",
    "2:  bl   rust_main",
    // rust_main is -> !, but hang safe if it ever returns
    "3:  mcr  p15, 0, r0, c7, c0, 4",  // CP15 wait-for-interrupt (ARMv5)
    "    b    3b",
);

/// ARMv5 wait-for-interrupt (CP15 c7,c0,4 — ARMv5 has no `wfi` instruction).
#[inline(always)]
fn wfi() {
    // SAFETY: CP15 WFI, value in the source register is ignored.
    unsafe { core::arch::asm!("mcr p15, 0, {0}, c7, c0, 4", in(reg) 0u32) };
}

/// Live IRQ tick counter, bumped by the Wallace clockevent handler.
static TICKS: Mutex<Cell<u32>> = Mutex::new(Cell::new(0));

/// Wallace clockevent handler (IRQ 36): ack the timer and count the tick. Runs
/// in IRQ mode with IRQs masked; registered via `vectors::set_handler`.
fn wallace_tick() {
    timer::wallace_ack();
    critical_section::with(|cs| {
        let c = TICKS.borrow(cs);
        c.set(c.get().wrapping_add(1));
    });
    time_driver::on_tick(); // wake any embassy Timers whose deadline elapsed
    rng::feed_timing(timer::now_ticks() as u32); // IRQ-arrival jitter → entropy pool
}

/// Read the live IRQ tick count.
fn ticks() -> u32 {
    critical_section::with(|cs| TICKS.borrow(cs).get())
}

/// Unmask IRQs at the CPU by clearing the CPSR I-bit (ARMv5 has no `cpsie`).
///
/// # Safety
/// Call only after the vectors are installed and IRQ handlers registered.
unsafe fn enable_irqs() {
    core::arch::asm!(
        "mrs {t}, cpsr",
        "bic {t}, {t}, #0x80",   // clear I (IRQ enable)
        "msr cpsr_c, {t}",
        t = out(reg) _,
        options(nomem, nostack, preserves_flags),
    );
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    let mut con = Uart::console();
    let _ = writeln!(con, "\n[ilo3-rs] Gromit-XE bring-up — hello from Rust on ARM926EJ-S");
    let _ = writeln!(con, "[ilo3-rs] UART @ 0xC00000F0, RAM @ 0x20000000");

    // MMU on: identity map + write-back DDR + high-vector alias. This puts
    // writable RAM under the CPU exception base (0xFFFF0000) so the vector
    // table is installable and interrupts can finally be taken.
    mmu::init();
    let _ = writeln!(
        con,
        "[ilo3-rs] mmu: on — identity 1 MB sections, DDR cached, high vectors @ {:#010x}",
        mmu::VECTOR_BASE
    );

    // Global allocator over the DDR heap region (now that the MMU maps it).
    let heap_sz = heap::init();
    let _ = writeln!(con, "[ilo3-rs] heap: {} KB DDR global allocator", heap_sz / 1024);

    // irqchip: mask all lines and clear pending (exercises critical-section).
    intc::init();
    let _ = writeln!(con, "[ilo3-rs] intc: 2 banks / 64 IRQs masked + cleared");

    // Install the exception vectors at the (now mapped) high-vector base and
    // make them visible to instruction fetch.
    // SAFETY: mmu::init() mapped 64+ writable bytes at VECTOR_BASE for this.
    unsafe { vectors::install_vectors(mmu::VECTOR_BASE) };
    mmu::sync_icache();
    let _ = writeln!(con, "[ilo3-rs] vectors: installed @ {:#010x}", mmu::VECTOR_BASE);

    // Live-IRQ proof: route the Wallace periodic tick (IRQ 36) to a counting
    // handler, unmask it, arm the timer, then drop the CPSR I-bit. Tick rate is
    // strap/PLL-derived — the reload is a placeholder (CONFIRM ON HW).
    vectors::set_handler(timer::WALLACE_IRQ, wallace_tick);
    intc::enable(timer::WALLACE_IRQ);
    timer::wallace_start_periodic(timer::TIMER_HZ); // ~1 tick/placeholder-second
    // SAFETY: vectors installed + handler registered before IRQs are unmasked.
    unsafe { enable_irqs() };
    let _ = writeln!(
        con,
        "[ilo3-rs] timer: monotonic @ 0xC0000098 + Wallace tick IRQ {} live",
        timer::WALLACE_IRQ
    );

    // Seed the software entropy pool from timer jitter (docs/32: no HW TRNG).
    // Blocks briefly harvesting samples; the Wallace IRQ keeps reseeding it.
    rng::init();
    let _ = writeln!(con, "[ilo3-rs] rng: entropy pool seeded (ChaCha20 CSPRNG) — QUALITY UNVERIFIED");

    // Crypto power-on self-test: KATs (SHA-256, X25519) + AEAD round-trip. A FAIL
    // means a miscompile/bug in the primitives — refuse to trust crypto if so.
    if crypto::selftest() {
        let _ = writeln!(con, "[ilo3-rs] crypto: self-test PASS (ChaCha20-Poly1305 / SHA-256 / X25519)");
    } else {
        let _ = writeln!(con, "[ilo3-rs] crypto: self-test FAIL — primitives unusable");
    }

    // Web auth: provision the account DB (creates a default Administrator with a
    // random password, logged once, if empty). Runs one Argon2 hash — slow, but
    // one-time at boot and before the executor, so nothing is stalled.
    webauth::init();

    // Safety-critical: force fans to MAX before anything else runs — the boot
    // default is cooling-ON, never off (docs/42). The thermal task re-asserts
    // this on its first poll and takes over control from there.
    thermal::init_failsafe();
    let _ = writeln!(con, "[ilo3-rs] thermal: boot fail-safe — fans MAX");

    let _ = writeln!(con, "[ilo3-rs] starting embassy executor (platform-spin)...");

    // Shared smoltcp socket set — used by both net_task (poll) and web_task.
    tcp::init();

    // Hand the console to the async world and start the cooperative executor.
    let executor = EXECUTOR.init(Executor::new());
    executor.run(|spawner| {
        spawner.spawn(heartbeat().unwrap());
        spawner.spawn(ticker().unwrap());
        spawner.spawn(thermal::thermal_task().unwrap());
        spawner.spawn(net::net_task().unwrap());
        spawner.spawn(web::web_task().unwrap());
    });
}

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Proof-of-life async task: prints the monotonic counter once a second. The
/// `Timer::after` sleep parks the executor and is woken by the Wallace tick IRQ
/// (via the embassy-time Driver) — real interrupt-driven async, not busy-wait.
#[embassy_executor::task]
async fn heartbeat() {
    let mut con = Uart::console();
    let mut n = 0u32;
    loop {
        let _ = writeln!(
            con,
            "[hb] heartbeat {} — monotonic={} irq_ticks={}",
            n,
            timer::now_ticks(),
            ticks()
        );
        n = n.wrapping_add(1);
        Timer::after(Duration::from_secs(1)).await;
    }
}

/// Second task — interleaves with `heartbeat` on a different async cadence,
/// proving the time Driver services multiple concurrent deadlines.
#[embassy_executor::task]
async fn ticker() {
    let mut con = Uart::console();
    loop {
        let _ = writeln!(con, "[tk] .");
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let mut con = Uart::console();
    let _ = writeln!(con, "\n[ilo3-rs] PANIC: {}", info);
    loop {
        wfi();
    }
}

// let `writeln!` target the UART
impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_bytes(s.as_bytes());
        Ok(())
    }
}
