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

/// Debug CHIF channel id (our own; the host tool uses the same).
const DBG_CH: u16 = 0xDB90;
/// Max payload per CHIF message (16 KB region minus header slack).
const MAX_RESP: usize = 0x3F00;

const CMD_PING: u8 = 0x01;
const CMD_DRAIN: u8 = 0x02; // arg = max bytes
const CMD_MD: u8 = 0x03; // addr, arg = len
const CMD_MW: u8 = 0x04; // addr, arg = u32 value
const CMD_MB: u8 = 0x05; // addr, arg = u8 value

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
        _ => resp(1, b"?cmd"),
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
