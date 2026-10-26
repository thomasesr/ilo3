//! UID-LED boot trace — the only pre-DDR, no-tap, no-desolder progress signal.
//!
//! The board has no tappable UART and the chip is resolder-each-iteration, so a
//! blind pre-DDR hang would cost a desolder to diagnose. The system UID LED is
//! driven by the CPLD at 0x01F14000 bit6 (CONFIRMED, docs/08) -- plain MMIO, alive
//! on standby power at reset, needs no DRAM and no PLL. So the SPL blinks it at
//! each milestone: watch the LED and the last stable blink-count tells you how far
//! boot got. A DDR failure switches to a distinct rapid blink instead of a silent
//! hang. Feature `led-trace` (default-on); drop it for production.
//!
//! Blink codes (cumulative over time):
//!   1x  SPL entry (CPU alive, running from flash)
//!   2x  clock check passed
//!   3x  DDR up
//!   4x  shared-mem self-test PASS (0x40000000 is backed RAM)
//!   5x  handing off (recovery window / main)
//!   3x LONG         = shared-mem self-test FAIL (0x40000000 not backed → host
//!                     interface dead); boot still continues
//!   rapid, forever  = DDR calibration FAILED (src/ddr.rs timeout)

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

const CPLD: usize = 0x01F1_4000; // CPLD status/UID byte (docs/08, docs/09)
const UID_BIT: u8 = 1 << 6;

#[inline]
fn set_uid(on: bool) {
    // SAFETY: CPLD MMIO, identity-mapped, alive at reset. RMW bit6.
    unsafe {
        let v = read_volatile(CPLD as *const u8);
        let v = if on { v | UID_BIT } else { v & !UID_BIT };
        write_volatile(CPLD as *mut u8, v);
    }
}

/// Busy-wait. `units` is coarse; one unit ~ a few ms at the strap-default clock
/// (exact timing does not matter — only that a blink is eye-visible).
fn delay(units: u32) {
    let mut n = units.wrapping_mul(200_000);
    while n != 0 {
        // volatile so the loop is not optimised away (no timer dep pre-DDR)
        unsafe { read_volatile(&n as *const u32) };
        n -= 1;
    }
}

/// Blink the UID LED `n` times, then a longer gap so counts read cleanly.
pub fn code(n: u32) {
    let mut i = 0;
    while i < n {
        set_uid(true);
        delay(1);
        set_uid(false);
        delay(1);
        i += 1;
    }
    delay(3); // inter-code gap
}

/// Blink the UID LED `n` times with LONG on/off periods — a rhythm clearly
/// distinct from the short milestone counts in `code()`. Used for a non-fatal
/// failure signal (e.g. the shared-mem self-test FAIL) where boot still continues.
pub fn long(n: u32) {
    let mut i = 0;
    while i < n {
        set_uid(true);
        delay(4);
        set_uid(false);
        delay(4);
        i += 1;
    }
    delay(6); // inter-code gap
}

/// Distinct never-ending rapid blink: an unmistakable error beacon (vs a silent
/// hang). Never returns.
pub fn panic_beacon() -> ! {
    loop {
        set_uid(true);
        delay(1);
        set_uid(false);
        delay(1);
    }
}
