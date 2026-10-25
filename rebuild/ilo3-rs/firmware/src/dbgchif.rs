//! Serial-over-CHIF debug service — host-pull bring-up debugger (feature-gated).
//!
//! The host (which boots AFTER iLO) connects over CHIF — pure PCI, no MAC ring /
//! network / UART / JTAG — and pulls debug data on demand. Because it is
//! host-PULL (DRAIN), early-boot output buffered in dbglog is not missed. The
//! host tool is `tools/ilo3-chif-console.py`.
//!
//! Wire protocol on the debug channel (`DBG_CH`):
//!   request  (host→iLO): [cmd u8][addr u32 LE][arg u32 LE]  (9 bytes)
//!   response (iLO→host): [status u8][len u16 LE][data...]
//!
//! DANGER: MD/MW/MB are raw peek/poke over all address space, exactly like the
//! serial shell — behind the `debug-shell` cargo feature; never ship enabled.

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use embassy_time::{Duration, Timer};

use crate::chif::Chif;
use crate::dbglog;
use crate::spinor;

/// Debug CHIF channel id (our own; the host tool uses the same).
const DBG_CH: u16 = 0xDB90;
/// Max payload per CHIF message (16 KB region minus header slack).
const MAX_RESP: usize = 0x3F00;

const CMD_PING: u8 = 0x01;
const CMD_DRAIN: u8 = 0x02; // arg = max bytes
const CMD_MD: u8 = 0x03; // addr, arg = len
const CMD_MW: u8 = 0x04; // addr, arg = u32 value
const CMD_MB: u8 = 0x05; // addr, arg = u8 value
// --- self-reflash from host (no resolder) ---
const CMD_FID: u8 = 0x10; // flash JEDEC id
const CMD_FERASE: u8 = 0x11; // addr, arg = len  (64 KB blocks)
const CMD_FWRITE: u8 = 0x12; // addr, arg = len, data = req[9..9+len]
const CMD_FREAD: u8 = 0x13; // addr, arg = len
const CMD_REBOOT: u8 = 0x14; // soft reset -> re-run flash boot (no response)

// Status codes in the response.
const ST_OK: u8 = 0;
const ST_BADREQ: u8 = 1;
const ST_PROT: u8 = 2; // target overlaps the write-protected SPL/boot region
const ST_FLASH: u8 = 3; // spinor error

// The SPL / boot region is write-protected from the host: it is the always-good
// recovery path. A bad host-reflash of the payload can be re-flashed from host; a
// nuked SPL would need the SPI programmer again. Only the programmer writes here.
const SPL_BASE: u32 = 0x7f_0000;
const FLASH_END: u32 = spinor::FLASH_SIZE;

/// True if [addr, addr+len) touches the protected SPL/boot region.
fn hits_spl(addr: u32, len: u32) -> bool {
    let end = addr.saturating_add(len);
    addr < FLASH_END && end > SPL_BASE
}

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

fn resp(status: u8, data: &[u8]) -> Vec<u8> {
    let n = data.len().min(MAX_RESP);
    let mut v = Vec::with_capacity(3 + n);
    v.push(status);
    v.extend_from_slice(&(n as u16).to_le_bytes());
    v.extend_from_slice(&data[..n]);
    v
}

/// Handle one request, produce the response bytes.
fn handle(req: &[u8]) -> Vec<u8> {
    if req.is_empty() {
        return resp(1, b"empty");
    }
    let cmd = req[0];
    let addr = if req.len() >= 5 { le32(req, 1) } else { 0 };
    let arg = if req.len() >= 9 { le32(req, 5) } else { 0 };
    match cmd {
        CMD_PING => resp(0, b"iLO3-rs dbgchif v1"),
        CMD_DRAIN => {
            let n = (arg as usize).min(MAX_RESP);
            let mut buf = vec![0u8; n];
            let got = dbglog::drain(&mut buf);
            resp(0, &buf[..got])
        }
        CMD_MD => {
            let n = (arg as usize).min(MAX_RESP);
            let mut buf = vec![0u8; n];
            // SAFETY: raw debug read; an unmapped addr can fault (bring-up tool).
            for (i, b) in buf.iter_mut().enumerate() {
                *b = unsafe { read_volatile((addr as usize + i) as *const u8) };
            }
            resp(0, &buf)
        }
        CMD_MW => {
            // SAFETY: raw debug write (bring-up only).
            unsafe { write_volatile(addr as *mut u32, arg) };
            resp(0, b"ok")
        }
        CMD_MB => {
            unsafe { write_volatile(addr as *mut u8, arg as u8) };
            resp(0, b"ok")
        }
        CMD_FID => match spinor::read_id() {
            Ok(id) => resp(ST_OK, &id),
            Err(_) => resp(ST_FLASH, b"id"),
        },
        CMD_FERASE => {
            if hits_spl(addr, arg) {
                return resp(ST_PROT, b"spl");
            }
            // erase every 64 KB block covering [addr, addr+arg)
            let start = addr & !0xFFFF;
            let end = addr.saturating_add(arg);
            let mut a = start;
            while a < end {
                if let Err(_) = spinor::erase_64k(a) {
                    return resp(ST_FLASH, b"erase");
                }
                a += 0x1_0000;
            }
            resp(ST_OK, b"ok")
        }
        CMD_FWRITE => {
            let len = arg as usize;
            if req.len() < 9 + len {
                return resp(ST_BADREQ, b"short");
            }
            if hits_spl(addr, arg) {
                return resp(ST_PROT, b"spl");
            }
            match spinor::page_program(addr, &req[9..9 + len]) {
                Ok(()) => resp(ST_OK, b"ok"),
                Err(_) => resp(ST_FLASH, b"write"),
            }
        }
        CMD_FREAD => {
            let n = (arg as usize).min(MAX_RESP);
            let mut buf = vec![0u8; n];
            match spinor::read(addr, &mut buf) {
                Ok(()) => resp(ST_OK, &buf),
                Err(_) => resp(ST_FLASH, b"read"),
            }
        }
        CMD_REBOOT => reboot(), // never returns
        _ => resp(ST_BADREQ, b"?cmd"),
    }
}

/// Soft-reset: mask interrupts, turn the MMU + caches off, jump to the flash reset
/// vector (0xFFFF0000 is flash when the MMU is off) to re-run the boot chain and
/// load the freshly-flashed payload. A clean path is an AC power-cycle; this just
/// saves the manual step. CONFIRM-ON-HW: a true SoC/WDT reset would be cleaner.
fn reboot() -> ! {
    // noreturn forbids output operands, so r1 is a hardcoded scratch (clobber is
    // irrelevant — we never come back); r0 carries the flash reset vector.
    unsafe {
        core::arch::asm!(
            "mrs r1, cpsr",
            "orr r1, r1, #0xC0",          // mask IRQ + FIQ
            "msr cpsr_c, r1",
            "mrc p15, 0, r1, c1, c0, 0",
            "bic r1, r1, #0x5",           // MMU (M) + D-cache (C) off
            "bic r1, r1, #0x1000",        // I-cache (I) off
            "mcr p15, 0, r1, c1, c0, 0",
            "mov r1, #0",
            "mcr p15, 0, r1, c7, c7, 0",  // invalidate I+D cache
            "bx r0",                      // -> flash reset vector
            in("r0") 0xFFFF_0000u32,
            options(noreturn),
        )
    }
}

/// The CHIF debug service task. Owns its own CHIF instance for bring-up (the
/// production `chif_server` folds this channel in later). `window_phys`/`log2` are
/// the shared-mem window from the platform — CONFIRM-ON-HW (docs/28).
#[embassy_executor::task]
pub async fn dbgchif_task() {
    let mut chif = Chif::new();
    chif.init(0x4000_0000, 20); // 1 MB window — CONFIRM-ON-HW
    let _ = chif.open_channel(DBG_CH);
    let mut rx = [0u8; 0x4000];
    loop {
        if let Some(msg) = chif.poll_rx(&mut rx) {
            if msg.channel == DBG_CH {
                let out = handle(&rx[..msg.len]);
                chif.send(DBG_CH, msg.ccb, &out);
            }
            chif.rx_release(msg.ccb);
        } else {
            Timer::after(Duration::from_millis(5)).await;
        }
    }
}
