//! iLO self-reset primitive.
//!
//! RE note (docs/28/49): HP's `reset_ilo()` — called after a firmware flash by
//! `http_flash_complete_thread` — does NOT write a single reset register; it routes
//! the reset through INTEGRITY IODevice kernel objects, and `bmc_wdt_*` is the IPMI
//! HOST watchdog, not the iLO self-reset. There is no clean app-level "reset
//! register" to mirror. So we use the certain primitive: a soft reset via the ARM
//! reset vector.
//!
//! This masks interrupts, turns the MMU + caches off (so `0xFFFF0000` is flash),
//! invalidates caches, and branches to the flash reset vector — re-running the SPL
//! boot chain (which handles a warm DRAM). It is a SOFT reset: it deliberately does
//! NOT reset the PCI endpoint, so the host keeps the iLO device enumerated across
//! the reset — which is exactly what we want for a button-triggered re-entry into
//! the SPL recovery window with the host still up (docs/49).
//!
//! CONFIRM-ON-HW: a true SoC/WDT reset would also reset peripherals; if one is
//! found behind the kernel abstraction, prefer it for a full cold path.

use core::arch::asm;

/// Soft-reset the iLO: mask IRQ/FIQ, MMU + caches off, invalidate, jump to the
/// flash reset vector (0xFFFF0000). Never returns.
///
/// # Safety
/// Tears down the running system unconditionally. Callers must be done.
pub unsafe fn soft_reset() -> ! {
    // noreturn forbids output operands, so r1 is a hardcoded scratch (clobber is
    // irrelevant — we never come back); r0 carries the flash reset vector.
    asm!(
        "mrs r1, cpsr",
        "orr r1, r1, #0xC0",          // mask IRQ + FIQ
        "msr cpsr_c, r1",
        "mrc p15, 0, r1, c1, c0, 0",
        "bic r1, r1, #0x5",           // MMU (M) + D-cache (C) off
        "bic r1, r1, #0x1000",        // I-cache (I) off
        "mcr p15, 0, r1, c1, c0, 0",
        "mov r1, #0",
        "mcr p15, 0, r1, c7, c7, 0",  // invalidate I+D cache
        "bx r0",                      // -> flash reset vector
        in("r0") 0xFFFF_0000u32,
        options(noreturn),
    )
}
