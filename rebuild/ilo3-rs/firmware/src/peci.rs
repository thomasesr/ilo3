//! Gromit-XE PECI engine — CPU temperature transport HAL (Phase 2).
//!
//! Ported from `peci-gxe.c` (docs/26). Native PECI engine in the 0x01F00000
//! window at 0x01F00400 (NOT the CPLD +0x68, which is only the XNC target MUX).
//! Real completion is IRQ 60; this HAL polls STATUS instead (simpler, same regs).
//!
//! Registers (base 0x01F00400, CONFIRMED):
//!   +0x00 CMD    write a command-descriptor bus address = start xfer
//!   +0x04 STATUS (val & ~1) != 0 => bus error; 0xD1 = engine timeout
//!   +0x08 TIMING PECI bit-timing (preset chosen by the rev strap)
//!   +0x0C ENABLE write 0x80 to enable
//! Init path, the four registers, the timing values, 0xD1 and the STATUS error
//! test are CONFIRMED. CONFIRM-ON-HW: the command-descriptor byte layout and the
//! response offset (fw hands the engine a RAM buffer + IODPECI DMA; only partly
//! recovered) — the layout below mirrors the Linux driver's best-effort guess.

#![allow(dead_code)]

use core::ptr::{addr_of_mut, read_volatile, write_volatile};

const BASE: usize = 0x01F0_0400;
const CMD: usize = 0x00;
const STATUS: usize = 0x04;
const TIMING: usize = 0x08;
const ENABLE: usize = 0x0C;

const ENABLE_ON: u32 = 0x80;
const STATUS_ERR: u32 = 0xFFFF_FFFE; // any bit above bit0
const TIMEOUT_CODE: u32 = 0xD1;

const TIMING_SLOW: u32 = 0x3D09_0010;
const TIMING_FAST: u32 = 0x61A8_0019;
const STRAP: usize = 0x01F1_00A0; // rev strap, bits[23:16]
const STRAP_SLOW_MAX: u32 = 0x04;

const DESC_SZ: usize = 0x40;
const POLL_MAX: u32 = 1_000_000;

/// PECI error.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    TooLong,
    Timeout,
    Bus,
}

#[inline]
fn rl(off: usize) -> u32 {
    unsafe { read_volatile((BASE + off) as *const u32) }
}
#[inline]
fn wl(off: usize, v: u32) {
    unsafe { write_volatile((BASE + off) as *mut u32, v) };
}

/// Command/response descriptor buffer handed to the engine (identity-mapped, so
/// its VA == the bus address written to CMD). Cache-coherency for the engine's
/// DMA is a CONFIRM-ON-HW item (may need a non-cached mapping or D-cache flush).
#[repr(C, align(64))]
struct Desc([u8; DESC_SZ]);
static mut DESC: Desc = Desc([0; DESC_SZ]);

/// Enable the engine and program bit-timing from the rev strap. Call once.
pub fn init() {
    // SAFETY: fixed, valid strap + engine MMIO.
    let rev = (unsafe { read_volatile(STRAP as *const u32) } >> 16) & 0xFF;
    let timing = if rev <= STRAP_SLOW_MAX { TIMING_SLOW } else { TIMING_FAST };
    wl(TIMING, timing);
    wl(ENABLE, ENABLE_ON);
}

/// Run one PECI command: `tx` bytes out, `rx` bytes back from client `addr`
/// (0x30 + socket). Descriptor layout is best-effort (CONFIRM-ON-HW).
pub fn xfer(addr: u8, tx: &[u8], rx: &mut [u8]) -> Result<(), Error> {
    if tx.len() + 8 > DESC_SZ {
        return Err(Error::TooLong);
    }
    let buf = addr_of_mut!(DESC) as *mut u8;
    // SAFETY: single-user HAL; DESC is a fixed 64-byte buffer.
    unsafe {
        let mut n = 0usize;
        *buf.add(n) = 0x01; // valid/segment flag (fw const 1)
        n += 1;
        *buf.add(n) = addr;
        n += 1;
        *buf.add(n) = tx.len() as u8;
        n += 1;
        *buf.add(n) = rx.len() as u8;
        n += 1;
        for &b in tx {
            *buf.add(n) = b;
            n += 1;
        }

        wl(CMD, buf as u32); // start

        // Poll STATUS for completion (real engine signals via IRQ 60).
        let mut k = 0;
        loop {
            let s = rl(STATUS);
            if s == TIMEOUT_CODE {
                return Err(Error::Timeout);
            }
            // completion = STATUS resolves to no-error (bit0-only or zero)
            if s & STATUS_ERR == 0 {
                break;
            }
            k += 1;
            if k > POLL_MAX {
                return Err(Error::Bus);
            }
        }

        // response follows the command (offset = CONFIRM-ON-HW)
        for (i, v) in rx.iter_mut().enumerate() {
            *v = *buf.add(n + i);
        }
    }
    Ok(())
}
