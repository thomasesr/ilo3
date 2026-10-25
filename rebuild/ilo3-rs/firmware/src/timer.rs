//! Gromit-XE timers — monotonic counter + Wallace clockevent (HAL, Phase 1).
//!
//! Clocksource (docs/12, ported from `timer-gxe.c`):
//!   free-running 64-bit up-counter in the core block, read lo-then-hi:
//!     COUNTER_LO @ 0xC0000098   COUNTER_HI @ 0xC000009C   (CONFIRMED)
//!   The firmware reads it with a single `ldrd`; across a 32-bit bus we
//!   re-read HI to guard the LO rollover.
//!
//! Clockevent — "Wallace" companion block @ 0xD1000000, IRQ 36 (docs/12):
//!   STATUS @+0x00  bit7 (0x80) = tick pending        (CONFIRMED)
//!   CTRL   @+0x09  write 0x80 = enable               (CONFIRMED)
//!   RELOAD @+0x04  down-counter reload               (BEST GUESS — CONFIRM ON HW)
//!   ack = write-1-to-clear STATUS bit7               (BEST GUESS — CONFIRM ON HW)
//!
//! `TIMER_HZ` is the DT `clock-frequency` placeholder (1 MHz, docs/13) — the
//! real rate is strap/PLL-derived and must be measured on hardware.

// The Wallace clockevent (periodic tick) is wired once IRQs are live (the MMU
// sub-phase); the monotonic counter + delay are already exercised.
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

/// Counter tick rate. PLACEHOLDER (docs/13) — fix once measured on hardware.
pub const TIMER_HZ: u32 = 1_000_000;

const CNT_LO: usize = 0xC000_0098;
const CNT_HI: usize = 0xC000_009C;

/// Rollover-safe read of the 64-bit free-running counter.
#[inline]
pub fn now_ticks() -> u64 {
    loop {
        // SAFETY: fixed, valid MMIO reads of the counter halves.
        unsafe {
            let hi = read_volatile(CNT_HI as *const u32);
            let lo = read_volatile(CNT_LO as *const u32);
            if hi == read_volatile(CNT_HI as *const u32) {
                return ((hi as u64) << 32) | lo as u64;
            }
        }
    }
}

/// Busy-wait for `us` microseconds off the monotonic counter.
pub fn delay_us(us: u32) {
    let ticks = (us as u64 * TIMER_HZ as u64) / 1_000_000;
    let start = now_ticks();
    while now_ticks().wrapping_sub(start) < ticks {}
}

// ---------------- Wallace clockevent (periodic OS tick) ----------------

/// Wallace clockevent IRQ line (docs/12: IRQ 36 = bank1 line 4).
pub const WALLACE_IRQ: usize = 36;

const WAL_BASE: usize = 0xD100_0000;
const CE_STATUS: usize = 0x00;
const CE_RELOAD: usize = 0x04;
const CE_CTRL: usize = 0x09;
const CE_PENDING: u8 = 0x80;
const CE_ENABLE: u8 = 0x80;

#[inline]
fn wal(off: usize) -> usize {
    WAL_BASE + off
}

/// Program the Wallace timer for a periodic tick of `period` counter ticks and
/// enable it. The matching IRQ (36) must be unmasked at the irqchip separately.
pub fn wallace_start_periodic(period: u32) {
    // SAFETY: disable, load reload, then enable — fixed, valid MMIO.
    unsafe {
        write_volatile(wal(CE_CTRL) as *mut u8, 0); // disable while arming
        write_volatile(wal(CE_RELOAD) as *mut u32, period);
        write_volatile(wal(CE_CTRL) as *mut u8, CE_ENABLE);
    }
}

/// True if the Wallace timer has a tick pending (STATUS bit7).
#[inline]
pub fn wallace_pending() -> bool {
    // SAFETY: fixed, valid MMIO read.
    (unsafe { read_volatile(wal(CE_STATUS) as *const u8) } & CE_PENDING) != 0
}

/// Acknowledge a Wallace tick (write-1-to-clear the pending bit).
#[inline]
pub fn wallace_ack() {
    // SAFETY: write-1-to-clear STATUS bit7 (BEST GUESS — CONFIRM ON HW).
    unsafe { write_volatile(wal(CE_STATUS) as *mut u8, CE_PENDING) };
}
