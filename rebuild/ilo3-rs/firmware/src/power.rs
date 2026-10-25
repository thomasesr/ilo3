//! Host power control — power button, on/off/reset/NMI HAL (Phase 2).
//!
//! Ported from docs/24 (all CONFIRMED by disassembly). iLO drives the host's
//! virtual PWRBTN#/WAKEUP# line and reads power-good/reset state. Graceful vs
//! force is the ATX convention: hold duration, not a different register.
//!
//! Registers:
//!   0x01F02046 b  virtual PWRBTN# — clear(0), press(1), delay, release(0)
//!   0x01F0005C/5D warm-reset actuator — write +0x5C=0 then +0x5D=0x80
//!   0x01F0006E b  NMI — set bit2 (0x04), hold ~1 s, clear
//!   0x01F000E7 b  CSM force cold-cycle — write 0xB2
//!   0x01F02070 w  power-state — bit24 (0x0100_0000) PGOOD, bit25 reset-active
//! Force-off also pre-sets CPLD +0x05 = 2 (via crate::cpld).

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

use crate::{cpld, timer};

const VBTN: *mut u8 = 0x01F0_2046 as *mut u8;
const RST_LO: *mut u8 = 0x01F0_005C as *mut u8;
const RST_HI: *mut u8 = 0x01F0_005D as *mut u8;
const NMI: *mut u8 = 0x01F0_006E as *mut u8;
const CSM: *mut u8 = 0x01F0_00E7 as *mut u8;
const PSTATE: *const u32 = 0x01F0_2070 as *const u32;

const PGOOD: u32 = 1 << 24;
const IN_RESET: u32 = 1 << 25;
const NMI_BIT: u8 = 1 << 2;
const CSM_COLD: u8 = 0xB2;

// Button hold durations (ms). ON/force CONFIRMED; graceful hold inferred.
const MS_ON: u32 = 100;
const MS_GRACEFUL: u32 = 200;
const MS_FORCE: u32 = 6000;
const CPLD_FORCE_OFF: usize = 0x05; // pre-set = 2 before a force-off

/// Host power state.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum State {
    Off,
    On,
    InReset,
}

/// Read the host power state (0x01F02070 bit24/25).
pub fn state() -> State {
    // SAFETY: fixed, valid power-state MMIO.
    let v = unsafe { read_volatile(PSTATE) };
    if v & PGOOD == 0 {
        State::Off
    } else if v & IN_RESET != 0 {
        State::InReset
    } else {
        State::On
    }
}

/// Pulse the virtual power button for `ms` milliseconds (press → hold → release).
fn press(ms: u32) {
    // SAFETY: fixed, valid PWRBTN# MMIO; delay is a busy-wait off the monotonic timer.
    unsafe {
        write_volatile(VBTN, 0); // ensure released
        write_volatile(VBTN, 1); // press
    }
    timer::delay_us(ms * 1000);
    unsafe { write_volatile(VBTN, 0) }; // release
}

/// Momentary press to power the host on (100 ms, CONFIRMED).
pub fn power_on() {
    press(MS_ON);
}

/// Request an ACPI graceful shutdown (short momentary press).
pub fn graceful_shutdown() {
    press(MS_GRACEFUL);
}

/// Force the host off (hold >= 6 s; pre-set the CPLD force-off latch first).
/// Blocks for ~6 s — rare path, keep off the executor hot loop.
pub fn force_off() {
    cpld::set_bit(CPLD_FORCE_OFF, 1, true); // CPLD +0x05 = 2 (bit1)
    press(MS_FORCE);
}

/// Warm reset / restart (2-byte actuator sequence).
pub fn warm_reset() {
    // SAFETY: fixed, valid warm-reset MMIO; order matters (lo then hi).
    unsafe {
        write_volatile(RST_LO, 0);
        write_volatile(RST_HI, 0x80);
    }
}

/// Inject an NMI to the host (set bit2, hold ~1 s, clear).
pub fn nmi() {
    // SAFETY: fixed, valid NMI MMIO.
    unsafe {
        let v = read_volatile(NMI as *const u8);
        write_volatile(NMI, v | NMI_BIT);
    }
    timer::delay_us(1_000_000);
    unsafe {
        let v = read_volatile(NMI as *const u8);
        write_volatile(NMI, v & !NMI_BIT);
    }
}

/// Force a cold power cycle via the CSM (write 0xB2). No graceful path.
pub fn cold_cycle() {
    // SAFETY: fixed, valid CSM MMIO.
    unsafe { write_volatile(CSM, CSM_COLD) };
}
