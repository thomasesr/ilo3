//! KCS (Keyboard Controller Style) IPMI transport — BMC side (Phase 5, docs/36).
//!
//! The host system interface for IPMI (IPMI spec §9). The host writes a request
//! byte-by-byte via KCS phase commands, the BMC assembles it, dispatches through
//! `ipmi`, and streams the response back as the host reads. This encodes the
//! standard KCS state machine; the GXE register offsets/bit positions at
//! 0x01F0D400 are NOT in the RE (docs/05 only locates the block) — they are
//! best-reading placeholders, CONFIRM-ON-HW before enabling the service.

#![allow(dead_code)]

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

use crate::ipmi::{self, Request};

const BASE: usize = 0x01F0_D400;
// Register offsets — CONFIRM-ON-HW (standard KCS has a data + a status/command reg).
const REG_DATA: usize = 0x00; // host<->BMC data (read = host's byte, write = response byte)
const REG_STATUS: usize = 0x04; // read = status; the host writes phase commands to its cmd port

// Status register bits (IPMI spec §9.1).
const ST_OBF: u8 = 0x01; // output buffer full (BMC → host)
const ST_IBF: u8 = 0x02; // input buffer full (host → BMC)
const ST_CD: u8 = 0x08; // 1 = last host write was a command, 0 = data
const ST_STATE_MASK: u8 = 0xC0; // bits 6-7 = interface state

// KCS interface state (written into status bits 6-7).
const STATE_IDLE: u8 = 0x00;
const STATE_READ: u8 = 0x40;
const STATE_WRITE: u8 = 0x80;
const STATE_ERROR: u8 = 0xC0;

// Host control codes (written to the command port).
const CTL_GET_STATUS_ABORT: u8 = 0x60;
const CTL_WRITE_START: u8 = 0x61;
const CTL_WRITE_END: u8 = 0x62;
const CTL_READ: u8 = 0x68;

#[inline]
fn status() -> u8 {
    // SAFETY: fixed, valid KCS MMIO.
    unsafe { read_volatile((BASE + REG_STATUS) as *const u8) }
}
#[inline]
fn set_state(s: u8) {
    // SAFETY: preserve the low bits, set the state field.
    unsafe {
        let v = (read_volatile((BASE + REG_STATUS) as *const u8) & !ST_STATE_MASK) | s;
        write_volatile((BASE + REG_STATUS) as *mut u8, v);
    }
}
#[inline]
fn read_data() -> u8 {
    // SAFETY: fixed, valid KCS MMIO (clears IBF on read).
    unsafe { read_volatile((BASE + REG_DATA) as *const u8) }
}
#[inline]
fn write_data(b: u8) {
    // SAFETY: fixed, valid KCS MMIO (sets OBF).
    unsafe { write_volatile((BASE + REG_DATA) as *mut u8, b) };
}

enum Phase {
    Idle,
    Collecting(Vec<u8>),    // accumulating request bytes after WRITE_START
    Responding(Vec<u8>, usize), // response bytes + index being streamed
}

/// A BMC-side KCS channel. Call `poll()` whenever the host set IBF (or on a
/// timer); it advances the state machine one host transaction at a time.
pub struct Kcs {
    phase: Phase,
}

impl Kcs {
    pub const fn new() -> Self {
        Kcs { phase: Phase::Idle }
    }

    /// Advance the state machine if the host has written a byte (IBF set). Encodes
    /// the IPMI §9 BMC flow: WRITE_START → data… → WRITE_END (dispatch) → READ…
    pub fn poll(&mut self) {
        let st = status();
        if st & ST_IBF == 0 {
            return; // nothing from the host
        }
        let is_cmd = st & ST_CD != 0;
        let byte = read_data();

        if is_cmd {
            match byte {
                CTL_WRITE_START => {
                    self.phase = Phase::Collecting(Vec::new());
                    set_state(STATE_WRITE);
                }
                CTL_WRITE_END => {
                    // next data byte is the final request byte; mark and dispatch
                    if let Phase::Collecting(buf) = core::mem::replace(&mut self.phase, Phase::Idle) {
                        let resp = self.handle(&buf);
                        self.begin_response(resp);
                    }
                }
                CTL_READ => self.stream_next(),
                CTL_GET_STATUS_ABORT => {
                    self.phase = Phase::Idle;
                    set_state(STATE_IDLE);
                }
                _ => {
                    self.phase = Phase::Idle;
                    set_state(STATE_ERROR);
                }
            }
        } else if let Phase::Collecting(buf) = &mut self.phase {
            buf.push(byte);
        }
    }

    /// Decode the assembled KCS request and dispatch it.
    fn handle(&self, buf: &[u8]) -> Vec<u8> {
        // KCS request layout: [NetFn/LUN][Cmd][data...].
        if buf.len() < 2 {
            return Vec::new();
        }
        let netfn = buf[0] >> 2;
        let lun = buf[0] & 0x03;
        let cmd = buf[1];
        let resp = ipmi::dispatch(&Request { netfn, lun, cmd, data: &buf[2..] });
        // Response layout: [(NetFn|1)/LUN][Cmd][completion code][data...].
        let mut out = Vec::with_capacity(3 + resp.data.len());
        out.push(((netfn | 1) << 2) | lun);
        out.push(cmd);
        out.push(resp.cc);
        out.extend_from_slice(&resp.data);
        out
    }

    fn begin_response(&mut self, resp: Vec<u8>) {
        set_state(STATE_READ);
        self.phase = Phase::Responding(resp, 0);
    }

    /// Emit the next response byte in reply to a host READ.
    fn stream_next(&mut self) {
        if let Phase::Responding(resp, idx) = &mut self.phase {
            if *idx < resp.len() {
                write_data(resp[*idx]);
                *idx += 1;
                if *idx >= resp.len() {
                    set_state(STATE_IDLE);
                }
            } else {
                set_state(STATE_IDLE);
                self.phase = Phase::Idle;
            }
        }
    }
}
