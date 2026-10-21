//! Gromit-XE interrupt controller — 2-bank root irqchip (HAL, Phase 1).
//!
//! Two 32-line banks, 64 IRQs total (docs/12, ported from `irq-gxe.c`):
//!   bank0 (IRQ 0-31)  @ 0xCEFF0000   (IC0)
//!   bank1 (IRQ 32-63) @ 0x80F00000   (IC1)
//!
//! Per-bank registers (identity-mapped MMIO, VA=PA):
//!   +0x0C  PENDING   read: raw/pending status                 (docs/12)
//!   +0x10  ENABLE    1 = line enabled      (CONFIRMED, fw 0x3404)
//!   +0x14  DISABLE   1 = line masked (= ~ENABLE)  (CONFIRMED, init)
//!   +0x30  EOI       write 1<<line to clear pending            (docs/12)
//!
//! The firmware keeps ENABLE and DISABLE strictly complementary and rewrites
//! both on every change, so we shadow the enable mask and push ENABLE=shadow,
//! DISABLE=~shadow. PENDING(+0x0C)/EOI(+0x30) are the best static reading —
//! CONFIRM ON HARDWARE (see irq-gxe.c).

// enable()/disable() are the HAL surface wired once interrupts are live (the
// MMU sub-phase); init()/pending()/eoi() are already exercised.
#![allow(dead_code)]

use core::cell::Cell;
use core::ptr::{read_volatile, write_volatile};
use critical_section::Mutex;

const BANK_BASE: [usize; 2] = [0xCEFF_0000, 0x80F0_0000];
const PENDING: usize = 0x0C;
const ENABLE: usize = 0x10;
const DISABLE: usize = 0x14;
const EOI: usize = 0x30;

pub const NR_BANKS: usize = 2;
pub const PER_BANK: usize = 32;
pub const NR_IRQS: usize = NR_BANKS * PER_BANK;

/// Shadow of each bank's ENABLE register (hardware regs are write-mostly).
static SHADOW: Mutex<Cell<[u32; NR_BANKS]>> = Mutex::new(Cell::new([0; NR_BANKS]));

#[inline]
fn reg(bank: usize, off: usize) -> *mut u32 {
    (BANK_BASE[bank] + off) as *mut u32
}

/// Mask every line and clear all pending — call once at boot before unmasking.
pub fn init() {
    critical_section::with(|cs| {
        SHADOW.borrow(cs).set([0; NR_BANKS]);
        for b in 0..NR_BANKS {
            // SAFETY: fixed, valid MMIO for each bank.
            unsafe {
                write_volatile(reg(b, ENABLE), 0);
                write_volatile(reg(b, DISABLE), !0);
                write_volatile(reg(b, EOI), !0);
            }
        }
    });
}

#[inline]
fn push(cs: critical_section::CriticalSection, bank: usize, mask: u32) {
    // SAFETY: ENABLE=shadow / DISABLE=~shadow, mirroring the firmware.
    unsafe {
        write_volatile(reg(bank, ENABLE), mask);
        write_volatile(reg(bank, DISABLE), !mask);
    }
    SHADOW.borrow(cs).set({
        let mut s = SHADOW.borrow(cs).get();
        s[bank] = mask;
        s
    });
}

/// Unmask one IRQ line (0..64).
pub fn enable(irq: usize) {
    let (bank, line) = (irq / PER_BANK, irq % PER_BANK);
    critical_section::with(|cs| {
        let mask = SHADOW.borrow(cs).get()[bank] | (1 << line);
        push(cs, bank, mask);
    });
}

/// Mask one IRQ line.
pub fn disable(irq: usize) {
    let (bank, line) = (irq / PER_BANK, irq % PER_BANK);
    critical_section::with(|cs| {
        let mask = SHADOW.borrow(cs).get()[bank] & !(1 << line);
        push(cs, bank, mask);
    });
}

/// Acknowledge (clear pending) one IRQ line at the controller.
pub fn eoi(irq: usize) {
    let (bank, line) = (irq / PER_BANK, irq % PER_BANK);
    // SAFETY: write-1-to-clear the pending bit for this line.
    unsafe { write_volatile(reg(bank, EOI), 1 << line) };
}

/// Pending-and-enabled lines of a bank (what the root handler dispatches on).
#[inline]
pub fn pending(bank: usize) -> u32 {
    critical_section::with(|cs| {
        // SAFETY: fixed, valid MMIO read.
        let raw = unsafe { read_volatile(reg(bank, PENDING) as *const u32) };
        raw & SHADOW.borrow(cs).get()[bank]
    })
}
