//! SMIF SPI boot-flash controller — register-file HAL (Phase 2).
//!
//! Ported from `spi-gxe-smif.c` (docs/15, docs/17). The `gxekflsh` controller at
//! 0xC0000200 is a classic opcode/address/count/FIFO engine (NOT a 0xF0000000
//! window). One command = program the register file, launch via GO, poll BUSY.
//!
//! Register file (base 0xC0000200, all CONFIRMED):
//!   +0x00 u32 CTRL   clear bits[13:11] (0x3800) before each op
//!   +0x02 u8  MODE   3 = data transfer, 0 = no-data command
//!   +0x04 u8  GO/BUSY write = launch; read bit1 (0x02) = busy
//!   +0x05 u8  OPCODE SPI command byte (unlock 0x55,0xAA first on control ops)
//!   +0x06 u8  COUNT  data byte count (<= 0x40)
//!   +0x08 u32 ADDR   24-bit flash address
//!   +0x40 64B FIFO   program source / read dest / status result
//!
//! Opcodes: RDSR 0x05, WREN 0x06, READ 0x03, PP 0x02, BLOCK-ERASE(64K) 0xD8.
//! GO launch values (CONFIRM-ON-HW): STATUS 0x01, CMD 0x09, PROG 0x0D.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

const BASE: usize = 0xC000_0200;
const CTRL: usize = 0x00;
const MODE: usize = 0x02;
const GO: usize = 0x04;
const OPCODE: usize = 0x05;
const COUNT: usize = 0x06;
const ADDR: usize = 0x08;
const FIFO: usize = 0x40;

const CTRL_MODEMASK: u32 = 0x3800;
const MODE_DATA: u8 = 0x03;
const MODE_NONE: u8 = 0x00;
const BUSY: u8 = 0x02; // GO reg bit1
const FIFO_SZ: usize = 0x40; // 64 bytes
const SR_WIP: u8 = 0x01; // RDSR bit0

// GO launch values (CONFIRM-ON-HW).
const GO_STATUS: u8 = 0x01;
const GO_CMD: u8 = 0x09;
const GO_PROG: u8 = 0x0D;

// SPI opcodes.
const OP_RDSR: u8 = 0x05;
const OP_WREN: u8 = 0x06;
const OP_READ: u8 = 0x03;
const OP_PP: u8 = 0x02;
const OP_ERASE4K: u8 = 0x20; // sector erase (4 KB)
const OP_ERASE64K: u8 = 0xD8; // block erase (64 KB)
const OP_RDID: u8 = 0x9F; // JEDEC read-ID

/// Fitted part: Macronix MX25L6455E, 64 Mbit (8 MB) SPI NOR (confirmed from the
/// board). JEDEC RDID = mfr 0xC2 (Macronix), type 0x20 (MX25L), capacity 0x17.
pub const JEDEC_MX25L6455E: [u8; 3] = [0xC2, 0x20, 0x17];
pub const FLASH_SIZE: u32 = 8 * 1024 * 1024;

const POLL_MAX: u32 = 2_000_000; // bounded spin guard

/// SPI-NOR controller error.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Busy,     // controller GO/BUSY never cleared
    WipStuck, // flash write-in-progress never cleared
    TooLong,  // transfer exceeds the 64-byte FIFO
}

#[inline]
fn rb(off: usize) -> u8 {
    unsafe { read_volatile((BASE + off) as *const u8) }
}
#[inline]
fn wb(off: usize, v: u8) {
    unsafe { write_volatile((BASE + off) as *mut u8, v) };
}

/// One command phase. `addr` None = control op (and triggers the 0x55/0xAA
/// unlock preamble, as the fw does for no-address ops). `tx`/`rx` ≤ 64 bytes.
fn xfer(opcode: u8, addr: Option<u32>, tx: &[u8], rx: &mut [u8], go: u8) -> Result<(), Error> {
    let len = tx.len().max(rx.len());
    if len > FIFO_SZ {
        return Err(Error::TooLong);
    }
    // CTRL mode-clear (fw clears bits[13:11] each op).
    let c = unsafe { read_volatile((BASE + CTRL) as *const u32) };
    unsafe { write_volatile((BASE + CTRL) as *mut u32, c & !CTRL_MODEMASK) };

    if addr.is_none() {
        wb(OPCODE, 0x55); // unlock preamble
        wb(OPCODE, 0xAA);
    }
    wb(MODE, if len != 0 { MODE_DATA } else { MODE_NONE });
    if let Some(a) = addr {
        unsafe { write_volatile((BASE + ADDR) as *mut u32, a & 0xFF_FFFF) };
    }
    wb(COUNT, len as u8);
    for (i, &b) in tx.iter().enumerate() {
        wb(FIFO + i, b);
    }
    wb(OPCODE, opcode);
    wb(GO, go); // launch

    // Wait for the command phase to finish (GO/BUSY bit1 clears).
    let mut n = 0;
    while rb(GO) & BUSY != 0 {
        n += 1;
        if n > POLL_MAX {
            return Err(Error::Busy);
        }
    }
    for (i, b) in rx.iter_mut().enumerate() {
        *b = rb(FIFO + i);
    }
    Ok(())
}

/// Read the flash status register (RDSR).
pub fn read_status() -> Result<u8, Error> {
    let mut sr = [0u8; 1];
    xfer(OP_RDSR, None, &[], &mut sr, GO_STATUS)?;
    Ok(sr[0])
}

/// Set the write-enable latch (WREN).
pub fn write_enable() -> Result<(), Error> {
    xfer(OP_WREN, None, &[], &mut [], GO_CMD)
}

/// Poll RDSR until write-in-progress clears (bounded).
fn wait_wip() -> Result<(), Error> {
    let mut n = 0;
    while read_status()? & SR_WIP != 0 {
        n += 1;
        if n > POLL_MAX {
            return Err(Error::WipStuck);
        }
    }
    Ok(())
}

/// Read `buf.len()` bytes from flash `addr`, split into 64-byte FIFO windows.
pub fn read(addr: u32, buf: &mut [u8]) -> Result<(), Error> {
    let mut off = 0usize;
    while off < buf.len() {
        let n = (buf.len() - off).min(FIFO_SZ);
        xfer(OP_READ, Some(addr + off as u32), &[], &mut buf[off..off + n], GO_CMD)?;
        off += n;
    }
    Ok(())
}

/// Program `data` at flash `addr` (data must lie within one already-erased
/// region). Split into 64-byte windows, WREN + WIP-wait around each.
pub fn page_program(addr: u32, data: &[u8]) -> Result<(), Error> {
    let mut off = 0usize;
    while off < data.len() {
        let n = (data.len() - off).min(FIFO_SZ);
        write_enable()?;
        xfer(OP_PP, Some(addr + off as u32), &data[off..off + n], &mut [], GO_PROG)?;
        wait_wip()?;
        off += n;
    }
    Ok(())
}

/// Erase the 64 KB block containing `addr` (opcode 0xD8).
pub fn erase_64k(addr: u32) -> Result<(), Error> {
    write_enable()?;
    xfer(OP_ERASE64K, Some(addr & !0xFFFF), &[], &mut [], GO_CMD)?;
    wait_wip()
}

/// Erase the 4 KB sector containing `addr` (opcode 0x20) — MX25L6455E supports
/// fine-grained erase, better for the small NVRAM/cert/log regions than 64 KB.
pub fn erase_4k(addr: u32) -> Result<(), Error> {
    write_enable()?;
    xfer(OP_ERASE4K, Some(addr & !0xFFF), &[], &mut [], GO_CMD)?;
    wait_wip()
}

/// Read the JEDEC ID (RDID 0x9F) → [manufacturer, type, capacity]. Compare to
/// `JEDEC_MX25L6455E` to confirm the fitted flash before any erase/program.
pub fn read_id() -> Result<[u8; 3], Error> {
    let mut id = [0u8; 3];
    xfer(OP_RDID, None, &[], &mut id, GO_STATUS)?;
    Ok(id)
}
