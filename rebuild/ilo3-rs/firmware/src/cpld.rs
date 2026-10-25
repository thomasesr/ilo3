//! System CPLD / power-micro — bit-addressable register window (HAL, Phase 2).
//!
//! Ported from `gpio-gxe-cpld.c` (docs/08, docs/09). An 8-bit MMIO window at
//! 0x01F14000 (~0x200 bytes) fronting CPLD status/version, UID LED/button,
//! host-power sequencing, PSU enable/presence, fan latch (+0x0B) and PECI mux.
//!
//! Addressing mirrors the Linux gpio_chip: line N = byte (N >> 3), bit (N & 7).
//! Writes are read-modify-write; the firmware serialises them under a critical
//! section (the original used an INTEGRITY IODevice lock / a spinlock).
//!
//! Confirmed: UID LED = +0x00 bit6 (uid.c). PSU enable/present bit positions are
//! inferred (docs/09) — CONFIRM ON HW.

// HAL surface wired once sensor/power services consume it (Phase 2+).
#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

use critical_section as cs;

const BASE: usize = 0x01F1_4000;
pub const SIZE: usize = 0x200; // window bytes → SIZE*8 gpio lines

// Named register offsets (docs/09).
const R_STATUS: usize = 0x00; // bit6 UID LED, bit7 CPLD status
const R_VER_LO: usize = 0x01;
const R_VER_HI: usize = 0x02;
const R_PS_ENABLE: usize = 0x18; // PSU enable bitmask (bays 0..3) — inferred
const R_PS_PRESENT: usize = 0x19; // PSU presence (bays 0..3) — inferred
const R_PS_BAYS: usize = 0x1A; // populated bay count

const UID_BIT: u8 = 6;

#[inline]
fn reg(off: usize) -> *mut u8 {
    (BASE + off) as *mut u8
}

/// Read a raw byte register.
#[inline]
pub fn read_u8(off: usize) -> u8 {
    // SAFETY: fixed, valid CPLD MMIO byte.
    unsafe { read_volatile(reg(off) as *const u8) }
}

/// Read one bit of a byte register.
#[inline]
pub fn read_bit(off: usize, bit: u8) -> bool {
    read_u8(off) & (1 << bit) != 0
}

/// Read-modify-write one bit, serialised against other CPLD writers.
pub fn set_bit(off: usize, bit: u8, val: bool) {
    cs::with(|_| {
        let mut v = read_u8(off);
        if val {
            v |= 1 << bit;
        } else {
            v &= !(1 << bit);
        }
        // SAFETY: fixed, valid CPLD MMIO byte; RMW under critical section.
        unsafe { write_volatile(reg(off), v) };
    });
}

// ---- line-number (gpio_chip) view: line N = byte N>>3, bit N&7 ----

/// Read CPLD line `n` (0..SIZE*8).
#[inline]
pub fn line_get(n: usize) -> bool {
    read_bit(n >> 3, (n & 7) as u8)
}

/// Write CPLD line `n`.
#[inline]
pub fn line_set(n: usize, val: bool) {
    set_bit(n >> 3, (n & 7) as u8, val);
}

// ---- named helpers ----

/// Set the chassis UID LED (+0x00 bit6, CONFIRMED).
pub fn uid_led(on: bool) {
    set_bit(R_STATUS, UID_BIT, on);
}

/// Current chassis UID LED state.
pub fn uid_led_on() -> bool {
    read_bit(R_STATUS, UID_BIT)
}

/// CPLD status bit (+0x00 bit7).
pub fn status_bit() -> bool {
    read_bit(R_STATUS, 7)
}

/// CPLD/FPGA version bytes (+0x01, +0x02).
pub fn version() -> (u8, u8) {
    (read_u8(R_VER_LO), read_u8(R_VER_HI))
}

/// Number of populated PSU bays (+0x1A).
pub fn psu_bays() -> u8 {
    read_u8(R_PS_BAYS)
}

/// PSU present in `bay` (0..3) — presence bit at +0x19 (inferred, CONFIRM ON HW).
pub fn psu_present(bay: u8) -> bool {
    read_bit(R_PS_PRESENT, bay)
}

/// Enable/disable PSU `bay` (0..3) — +0x18 EXPBUS_PS_ENABLE (inferred).
pub fn psu_enable(bay: u8, on: bool) {
    set_bit(R_PS_ENABLE, bay, on);
}
