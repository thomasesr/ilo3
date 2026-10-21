//! Clean-room iLO 3 (Gromit-XE, ARM926EJ-S / ARMv5TEJ) BMC firmware.
//! Phase 1 bring-up: reset → CP15 → stack → BSS → polled UART console.
//!
//! Hardware facts (RE'd, see ../../../docs):
//!   console UART  @ 0xC00000F0  data +0x00, status +0x05 (TXRDY bit5, RXRDY bit0)  [docs/05]
//!   DDR RAM       @ 0x20000000  (image runs here)                                  [docs/13]
//! Clean-room: register addresses are facts; no HP code is used.
#![no_std]
#![no_main]

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::panic::PanicInfo;

mod critsec; // critical-section impl (CPSR IRQ mask) — registered via set_impl!
mod intc; // 2-bank Gromit-XE irqchip
mod timer; // 64-bit monotonic counter + Wallace clockevent
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

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    let mut con = Uart::console();
    let _ = writeln!(con, "\n[ilo3-rs] Gromit-XE bring-up — hello from Rust on ARM926EJ-S");
    let _ = writeln!(con, "[ilo3-rs] UART @ 0xC00000F0, RAM @ 0x20000000");

    // irqchip: mask all lines and clear pending (exercises critical-section).
    intc::init();
    let _ = writeln!(con, "[ilo3-rs] intc: 2 banks / 64 IRQs masked + cleared");

    // Interrupt delivery (vector install + IRQ unmask + Wallace tick) waits on
    // the MMU/SPL sub-phase — the CPU exception base (0x0 / 0xFFFF0000) isn't
    // our DDR until the MMU can map writable RAM there (see vectors.rs). The
    // vector table, IRQ dispatch and Wallace clockevent are register-exact and
    // ready; Phase 1 drives time off the polled monotonic counter instead.
    let _ = writeln!(
        con,
        "[ilo3-rs] timer: 64-bit monotonic counter @ 0xC0000098, {} Hz (placeholder)",
        timer::TIMER_HZ
    );

    let t0 = timer::now_ticks();
    for tick in 0u32.. {
        timer::delay_us(1_000_000);
        let now = timer::now_ticks();
        let _ = writeln!(
            con,
            "[ilo3-rs] tick {} — monotonic={} (+{} ticks)",
            tick,
            now,
            now.wrapping_sub(t0)
        );
    }
    unreachable!()
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
