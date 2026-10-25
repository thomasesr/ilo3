//! Gromit-XE I2C/SMBus — 8-engine controller HAL (Phase 2).
//!
//! Ported from `i2c-gxe.c` (docs/05, docs/16). Eight independent engines share a
//! register file at 0x01F10100; each does one ≤16-byte transfer via a small
//! descriptor + buffer, polled to completion.
//!
//! Regions (CONFIRMED): engine regs @ base + e*0x10 (status +0, ctrl +1, data
//! +4, cmd +8); TX/RX buffer @ base + 0x80 + e*0x10 (+0x200 for engines 6,7),
//! 16 bytes; descriptor @ base + 0xE0 + e*4 (status +0, cmd +1, len +2).
//!
//! CONFIRM-ON-HW: the command-byte encoding (addr<<1|rd in buf[0]; desc.cmd bit0
//! = start, bit1 = read) and the descriptor-write-is-trigger model (fw 0xe21c8).

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

const BASE: usize = 0x01F1_0100;
pub const NR_ENGINES: usize = 8;
pub const MAX_LEN: usize = 16;

const ENG_STATUS: usize = 0x00;
const ENG_CTRL: usize = 0x01;
const ENG_DATA: usize = 0x04;

const DESC_STATUS: usize = 0; // 0x2 = done; bit2/3 = error
const DESC_CMD: usize = 1; // bit0 = start
const DESC_LEN: usize = 2;

const DESC_DONE: u8 = 0x02;
const DESC_ERR: u8 = (1 << 2) | (1 << 3);
const CMD_START: u8 = 0x01;
const CMD_RD: u8 = 0x02; // BEST GUESS

const POLL_MAX: u32 = 1_000_000; // ~bounded stand-in for the 100 ms fw timeout

/// I2C transfer error.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    BadEngine,
    TooLong,
    Timeout,
    Bus,
}

#[inline]
fn eng(e: usize, off: usize) -> *mut u8 {
    (BASE + e * 0x10 + off) as *mut u8
}
#[inline]
fn buf(e: usize) -> usize {
    BASE + 0x80 + e * 0x10 + if e > 5 { 0x200 } else { 0 }
}
#[inline]
fn desc(e: usize, off: usize) -> *mut u8 {
    (BASE + 0xE0 + e * 4 + off) as *mut u8
}
#[inline]
fn rb(p: *mut u8) -> u8 {
    unsafe { read_volatile(p as *const u8) }
}
#[inline]
fn wb(p: *mut u8, v: u8) {
    unsafe { write_volatile(p, v) };
}

/// Abort/reset one engine (CONFIRMED sequence, fw 0xe21c8).
pub fn reset(e: usize) {
    if e >= NR_ENGINES {
        return;
    }
    wb(eng(e, ENG_CTRL), 0x00);
    wb(eng(e, ENG_DATA), 0x80);
    wb(eng(e, ENG_DATA), 0x33);
}

/// Core transfer: buf[0] = addr<<1|rd, write payload in, start, poll, read out.
fn run(e: usize, addr: u8, rd: bool, tx: &[u8], rx: &mut [u8]) -> Result<(), Error> {
    if e >= NR_ENGINES {
        return Err(Error::BadEngine);
    }
    let len = tx.len().max(rx.len());
    if len > MAX_LEN {
        return Err(Error::TooLong);
    }
    let b = buf(e);
    // addressing byte, then payload for writes
    wb(b as *mut u8, (addr << 1) | rd as u8);
    for (i, &v) in tx.iter().enumerate() {
        wb((b + 1 + i) as *mut u8, v);
    }
    // descriptor: len then cmd (bit0 = start)
    wb(desc(e, DESC_LEN), len as u8);
    wb(desc(e, DESC_CMD), CMD_START | if rd { CMD_RD } else { 0 });

    // poll status: 0x2 = done, bit2/3 = bus error
    let mut n = 0;
    loop {
        let s = rb(desc(e, DESC_STATUS));
        if s & DESC_ERR != 0 {
            reset(e);
            return Err(Error::Bus);
        }
        if s & DESC_DONE != 0 {
            break;
        }
        n += 1;
        if n > POLL_MAX {
            reset(e);
            return Err(Error::Timeout);
        }
    }
    for (i, v) in rx.iter_mut().enumerate() {
        *v = rb((b + 1 + i) as *mut u8);
    }
    Ok(())
}

/// Write `data` to 7-bit `addr` on `engine`.
pub fn write(engine: usize, addr: u8, data: &[u8]) -> Result<(), Error> {
    run(engine, addr, false, data, &mut [])
}

/// Read `buf.len()` bytes from 7-bit `addr` on `engine`.
pub fn read(engine: usize, addr: u8, buf: &mut [u8]) -> Result<(), Error> {
    run(engine, addr, true, &[], buf)
}
