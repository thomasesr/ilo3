//! ARMv5 exception vectors + root IRQ dispatch (Phase 1 scaffolding).
//!
//! ARM926EJ-S has no VBAR (that's ARMv6+): the vector base is either 0x00000000
//! (CP15 c1 V=0) or 0xFFFF0000 (V=1). Our Phase-1 image runs MMU-off from DDR
//! at 0x20000000, so neither base is our RAM — `install_vectors()` copies the
//! 8-entry table to the chosen base at runtime. Until the MMU/SPL sub-phase can
//! place writable RAM at the exception base, interrupts stay masked and the
//! demo polls; this path is register-exact and ready, not yet HW-exercised.
//!
//! IRQ return uses the classic ARMv5 sequence (`ldmfd sp!, {..., pc}^`) — `srs`
//! / `rfe` are ARMv6+. The IRQ-mode stack is set up in `_start` (src/main.rs).

// set_handler()/install_vectors() are wired once the MMU can place writable RAM
// at the exception base (see module docs); irq_dispatch is reached from asm.
#![allow(dead_code)]

use crate::intc;
use core::cell::Cell;
use critical_section::Mutex;

core::arch::global_asm!(
    ".section .text.vectors",
    ".global _vectors",
    ".align 5",
    "_vectors:",
    "    ldr pc, [pc, #24]",   // reset    -> _vector_table[0]
    "    ldr pc, [pc, #24]",   // undef
    "    ldr pc, [pc, #24]",   // swi
    "    ldr pc, [pc, #24]",   // prefetch abort
    "    ldr pc, [pc, #24]",   // data abort
    "    ldr pc, [pc, #24]",   // reserved
    "    ldr pc, [pc, #24]",   // irq
    "    ldr pc, [pc, #24]",   // fiq
    "_vector_table:",
    "    .word _trap",         // reset   (installed table's reset is unused)
    "    .word _trap",         // undef
    "    .word _trap",         // swi
    "    .word _trap",         // prefetch abort
    "    .word _trap",         // data abort
    "    .word _trap",         // reserved
    "    .word _irq_entry",    // irq
    "    .word _trap",         // fiq
    // unhandled exceptions hang (a real abort handler lands later)
    "_trap:",
    "    b _trap",
    // --- IRQ entry: save scratch, dispatch, return restoring CPSR from SPSR ---
    "_irq_entry:",
    "    sub   lr, lr, #4",
    "    stmfd sp!, {{r0-r3, r12, lr}}",
    "    bl    irq_dispatch",
    "    ldmfd sp!, {{r0-r3, r12, pc}}^",
);

extern "C" {
    static _vectors: u8;
}

/// Per-line handler table (fn address, 0 = none). `usize` is `Copy` so it lives
/// behind a plain `Cell`; set/called with IRQs masked, so single-core safe.
static HANDLERS: Mutex<Cell<[usize; intc::NR_IRQS]>> =
    Mutex::new(Cell::new([0; intc::NR_IRQS]));

/// Register `f` as the handler for `irq` (0..64).
pub fn set_handler(irq: usize, f: fn()) {
    critical_section::with(|cs| {
        let mut t = HANDLERS.borrow(cs).get();
        t[irq] = f as usize;
        HANDLERS.borrow(cs).set(t);
    });
}

#[inline]
fn handler(irq: usize) -> Option<fn()> {
    let addr = critical_section::with(|cs| HANDLERS.borrow(cs).get()[irq]);
    if addr == 0 {
        None
    } else {
        // SAFETY: addr was stored from a `fn()` in set_handler.
        Some(unsafe { core::mem::transmute::<usize, fn()>(addr) })
    }
}

/// Root flow handler: walk both banks' pending-and-enabled lines, run each
/// registered handler, then EOI. Called from `_irq_entry` in IRQ mode.
#[no_mangle]
extern "C" fn irq_dispatch() {
    for bank in 0..intc::NR_BANKS {
        let mut p = intc::pending(bank);
        while p != 0 {
            let line = p.trailing_zeros() as usize;
            let irq = bank * intc::PER_BANK + line;
            if let Some(h) = handler(irq) {
                h();
            }
            intc::eoi(irq);
            p &= p - 1; // clear lowest set bit
        }
    }
}

/// Copy the 8-entry vector table (+ address pool, 64 B) to the CPU exception
/// base. `base` is 0x00000000 (CP15 V=0) or 0xFFFF0000 (V=1); it must be
/// writable RAM, which on this SoC needs the MMU (see module docs).
///
/// # Safety
/// `base` must point to 64 bytes of writable memory that the CPU uses as its
/// exception base. Call once, before unmasking any interrupt.
pub unsafe fn install_vectors(base: usize) {
    let src = &_vectors as *const u8;
    core::ptr::copy_nonoverlapping(src, base as *mut u8, 64);
}
