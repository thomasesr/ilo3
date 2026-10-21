//! `critical-section` implementation for ARM926EJ-S (ARMv5TEJ), single core.
//!
//! ARMv5 has no LDREX/STREX (that's ARMv6+), so `core::sync::atomic` and
//! `embassy-sync` can't do lock-free RMW — they fall back to `critical-section`.
//! On a single-core BMC a critical section is simply "mask IRQs", so acquire =
//! set the CPSR I bit, release = restore it. FIQ is left unmasked (reserved for
//! a future thermal/watchdog fail-safe above the executor — see PLAN-rust.md).
//!
//! `RawRestoreState = bool` (feature `restore-state-bool`): the token records
//! whether IRQs were *enabled* on entry, so nested sections restore correctly.

use critical_section::{set_impl, Impl, RawRestoreState};

const CPSR_I: u32 = 0x80; // IRQ-disable bit (bit 7)

struct SingleCoreCs;
set_impl!(SingleCoreCs);

unsafe impl Impl for SingleCoreCs {
    unsafe fn acquire() -> RawRestoreState {
        let cpsr: u32;
        // SAFETY: reads CPSR, then masks IRQ via the control byte (`cpsr_c`
        // leaves the condition flags untouched).
        unsafe {
            core::arch::asm!("mrs {0}, cpsr", out(reg) cpsr, options(nomem, nostack, preserves_flags));
            core::arch::asm!("msr cpsr_c, {0}", in(reg) cpsr | CPSR_I, options(nomem, nostack, preserves_flags));
        }
        // true  => IRQs were enabled here, so release() must re-enable them.
        (cpsr & CPSR_I) == 0
    }

    unsafe fn release(were_enabled: RawRestoreState) {
        if were_enabled {
            let cpsr: u32;
            // SAFETY: clears the IRQ-disable bit to re-enable interrupts.
            unsafe {
                core::arch::asm!("mrs {0}, cpsr", out(reg) cpsr, options(nomem, nostack, preserves_flags));
                core::arch::asm!("msr cpsr_c, {0}", in(reg) cpsr & !CPSR_I, options(nomem, nostack, preserves_flags));
            }
        }
    }
}
