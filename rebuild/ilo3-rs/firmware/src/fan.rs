//! Gromit-XE fan control — CPLD PWM/tach scan-chain HAL (Phase 2).
//!
//! Ported from `fan-gxe.c` (docs/25, docs/27). The GXE has NO flat per-fan
//! PWM/tach register array: PWM (open-drain FET bits) and tach ride a serial
//! shift register ("scan chain") latched through the CPLD. PWM duty is written to
//! the DATA bytes, CTRL set to update mode, then the CPLD latch strobe shifts the
//! bits out.
//!
//! Fan block base 0x01F00200 (CONFIRMED offsets):
//!   +0x0C CTRL        low 3 bits = mode (2 = update)
//!   +0x10..+0x17      8-byte PWM/FET scan-chain DATA
//!   +0x18 TACH_CTRL   write 0xFA = arm a tach sample
//!   +0x1C TACH_CFG    tach config word
//! Latch strobe: CPLD 0x01F1400B bit1 (set, delay, clear) — via crate::cpld.
//!
//! CONFIRM-ON-HW: PWM duty↔FET semantics + per-fan chain position (SDR/board),
//! and the tach result register + raw→RPM scale (fw reads via the SDR path).

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

use crate::{cpld, timer};

const BASE: usize = 0x01F0_0200;
const CTRL: usize = 0x0C;
const CTRL_MODE_MASK: u8 = 0x07;
const CTRL_MODE_UPDATE: u8 = 0x02;
const TACH_CTRL: usize = 0x18;
const TACH_ARM: u8 = 0xFA;
pub const NR_CHAN: usize = 8;

const CPLD_FAN_SYNC: usize = 0x0B; // CPLD latch byte
const CPLD_SYNC_BIT: u8 = 1; // bit1 = scan-chain latch strobe
const LATCH_US: u32 = 10;
const TACH_SETTLE_US: u32 = 1000;

#[inline]
fn data(ch: usize) -> *mut u8 {
    (BASE + 0x10 + ch) as *mut u8
}

/// Pulse the CPLD scan-chain latch strobe (RESYNC): set bit1, delay, clear.
fn latch() {
    cpld::set_bit(CPLD_FAN_SYNC, CPLD_SYNC_BIT, true);
    timer::delay_us(LATCH_US);
    cpld::set_bit(CPLD_FAN_SYNC, CPLD_SYNC_BIT, false);
}

/// Set fan channel `ch` (0..8) duty (0..255) and shift it out through the CPLD.
pub fn set_pwm(ch: usize, duty: u8) {
    if ch >= NR_CHAN {
        return;
    }
    // SAFETY: fixed, valid fan-block MMIO.
    unsafe {
        write_volatile(data(ch), duty);
        let ctrl = read_volatile((BASE + CTRL) as *const u8);
        let ctrl = (ctrl & !CTRL_MODE_MASK) | CTRL_MODE_UPDATE;
        write_volatile((BASE + CTRL) as *mut u8, ctrl);
    }
    latch();
}

/// Arm a tach sample for channel `ch` and read the raw 16-bit count.
/// CONFIRM-ON-HW: the result register (reuses the +0x10 window here) + scale.
pub fn read_tach_raw(ch: usize) -> u16 {
    if ch >= NR_CHAN {
        return 0;
    }
    // SAFETY: fixed, valid fan-block MMIO.
    unsafe {
        write_volatile((BASE + TACH_CTRL) as *mut u8, TACH_ARM);
    }
    timer::delay_us(TACH_SETTLE_US);
    // SAFETY: 16-bit read of the per-fan tach result (best-guess offset).
    unsafe { read_volatile((BASE + 0x10 + ch) as *const u16) }
}

/// Convert a raw tach count to RPM given the board `div` (SDR/DT, flagged).
/// Returns 0 if `div` or `raw` is 0 (stalled / unconfigured).
pub fn tach_to_rpm(raw: u16, div: u32) -> u32 {
    if raw == 0 || div == 0 {
        0
    } else {
        div / raw as u32
    }
}
