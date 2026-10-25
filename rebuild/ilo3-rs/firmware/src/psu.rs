//! Power-supply telemetry — "panda" (PANDEMONIUM) PSUs over I2C (Phase 2).
//!
//! Ported from docs/19, docs/20 (`src/ps_panda.c`). Smart PSUs, one per bay, on
//! the i2c-gxe buses. Transport is HP-proprietary (NOT PMBus): a 16-bit register
//! read with an HP checksum, 3 retries. Presence/enable come from the CPLD
//! (crate::cpld, docs/09); per-supply identity is the FRU EEPROM (separate addr).
//!
//! panda_read(engine, addr, reg):
//!   1. write [reg, cksum]  where cksum = (0x100 - reg - addr) & 0xff
//!   2. read  [lo, hi, pec]
//!   3. check (lo + hi + pec) & 0xff == 0
//!   4. value = (hi << 8) | lo ; 3 retries (~50 ms) on checksum/bus error
//!
//! Registers (CONFIRMED where noted): 0x00 fw rev, 0x02 status (bits[10:8] =
//! state/fault), 0x0A input current, 0x0C/0x12 Vin/power, 0x2C/2E/30 telemetry
//! triple (V/I/P), 0x32 peak power, 0x3A control (HEM/standby). The raw→physical
//! scale (value_adjust constants) is per-supply calibration — NOT recovered
//! (docs/20 Open), so getters return raw u16; apply the board scale downstream.

#![allow(dead_code)]

use crate::{cpld, i2c, timer};

const RETRIES: u32 = 3;
const RETRY_US: u32 = 50_000;

// Register (SMBus command) bytes.
pub const REG_FWREV: u8 = 0x00;
pub const REG_STATUS: u8 = 0x02;
pub const REG_IIN: u8 = 0x0A;
pub const REG_VIN: u8 = 0x0C;
pub const REG_PIN: u8 = 0x12;
pub const REG_PEAK: u8 = 0x32;
pub const REG_CTRL: u8 = 0x3A;

const CPLD_PS_PRESENT: usize = 0x19;

/// PSU access error.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Bus,
    Checksum,
}

/// 16-bit panda register read with the HP checksum + 3 retries.
pub fn panda_read(engine: usize, addr: u8, reg: u8) -> Result<u16, Error> {
    let cksum = 0u8.wrapping_sub(reg).wrapping_sub(addr); // (0x100 - reg - addr) & 0xff
    let mut last = Error::Bus;
    for attempt in 0..RETRIES {
        if attempt != 0 {
            timer::delay_us(RETRY_US);
        }
        if i2c::write(engine, addr, &[reg, cksum]).is_err() {
            last = Error::Bus;
            continue;
        }
        let mut r = [0u8; 3];
        if i2c::read(engine, addr, &mut r).is_err() {
            last = Error::Bus;
            continue;
        }
        // sum-to-zero check over [lo, hi, pec]
        if r[0].wrapping_add(r[1]).wrapping_add(r[2]) != 0 {
            last = Error::Checksum;
            continue;
        }
        return Ok(((r[1] as u16) << 8) | r[0] as u16);
    }
    Err(last)
}

/// 16-bit panda register write (mirror: [reg, lo, hi, cksum]).
pub fn panda_write(engine: usize, addr: u8, reg: u8, val: u16) -> Result<(), Error> {
    let lo = val as u8;
    let hi = (val >> 8) as u8;
    let cksum = 0u8
        .wrapping_sub(reg)
        .wrapping_sub(addr)
        .wrapping_sub(lo)
        .wrapping_sub(hi);
    i2c::write(engine, addr, &[reg, lo, hi, cksum]).map_err(|_| Error::Bus)
}

/// One power supply bay: which CPLD presence bit + which I2C bus/addr.
#[derive(Clone, Copy)]
pub struct Psu {
    pub bay: u8,       // CPLD presence/enable bit index
    pub engine: usize, // i2c engine
    pub addr: u8,      // 7-bit panda address
}

impl Psu {
    /// Present per the CPLD presence register (+0x19 bit = bay).
    pub fn present(&self) -> bool {
        cpld::read_bit(CPLD_PS_PRESENT, self.bay)
    }
    /// Enable/disable this bay (CPLD +0x18 EXPBUS_PS_ENABLE).
    pub fn enable(&self, on: bool) {
        cpld::psu_enable(self.bay, on);
    }

    fn read(&self, reg: u8) -> Result<u16, Error> {
        panda_read(self.engine, self.addr, reg)
    }

    /// Firmware revision word (→ maj.min downstream).
    pub fn fw_rev(&self) -> Result<u16, Error> {
        self.read(REG_FWREV)
    }
    /// Supply state/fault code = status bits[10:8].
    pub fn health(&self) -> Result<u8, Error> {
        Ok(((self.read(REG_STATUS)? >> 8) & 0x7) as u8)
    }
    /// Raw input current (apply the per-supply scale downstream).
    pub fn input_current_raw(&self) -> Result<u16, Error> {
        self.read(REG_IIN)
    }
    /// Raw peak power.
    pub fn peak_power_raw(&self) -> Result<u16, Error> {
        self.read(REG_PEAK)
    }
}

/// Bay table — PLACEHOLDER (bus/addr per bay = SDR data, docs/19).
pub const BAYS: &[Psu] = &[
    Psu { bay: 0, engine: 0, addr: 0x58 },
    Psu { bay: 1, engine: 0, addr: 0x59 },
];
