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

mod uart;
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
    "    ldr  sp, =_stack_top",        // SVC stack = top of RAM
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
    let _ = writeln!(con, "[ilo3-rs] phase 1: polled console up. halting.");
    loop {
        wfi();
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
