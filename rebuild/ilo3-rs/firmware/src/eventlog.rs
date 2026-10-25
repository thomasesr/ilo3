//! Event log store — IML + iLO log, coalescing + circular (Phase 5, docs/39).
//!
//! One record model, two logical views: IML (server-hardware events) and the iLO
//! event log (management events, with an actor user/IP). Duplicate events
//! coalesce (bump count + last-seen, don't re-log); the ring wraps (oldest drops)
//! when full. Backs the web event views, IPMI SEL, RIBCL, and SNMP traps.
//!
//! `raise()` is the EventBus entry point (log now; SNMP-trap / email hooks later).
//! Timestamps are the monotonic counter (no wall-clock until NTP, docs/18) — flag
//! when surfacing absolute time. Flash persistence = ping-pong across two blocks.

#![allow(dead_code)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

use critical_section::Mutex;

use crate::mac::ether_crc;
use crate::{spinor, timer};

const CAP: usize = 256; // ring capacity (oldest drops on wrap)
const LOG_A: u32 = 0x006A_0000;
const LOG_B: u32 = 0x006B_0000;
const BLOCK_SIZE: usize = 0x1_0000;
const MAGIC: u32 = 0x474C_5847; // "GXLG"

/// Event class/code constants (the EventBus taxonomy; maps toward the HP IML
/// catalog, docs/39). Kept compact; extend as producers are wired in.
pub mod class {
    pub const THERMAL: u16 = 0x0001;
    pub const POWER: u16 = 0x0002;
    pub const BOOT: u16 = 0x0003;
    pub const SECURITY: u16 = 0x0004;
}
pub mod code {
    pub const CRITICAL_TEMP: u16 = 0x0001;
    pub const SENSOR_FAIL: u16 = 0x0002;
    pub const BOOT_COMPLETE: u16 = 0x0001;
    pub const POWER_ON: u16 = 0x0001;
    pub const POWER_OFF: u16 = 0x0002;
    pub const LOGIN_FAIL: u16 = 0x0001;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Informational,
    Caution,
    Critical,
    Repaired,
    Corrected,
    Uncorrectable,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Iml, // server hardware (SystemRecord; also IPMI SEL)
    Ilo, // iLO management (MapRecord; logins/config, has an actor)
}

/// One coalescing log record.
#[derive(Clone)]
pub struct Record {
    pub number: u32,
    pub kind: LogKind,
    pub severity: Severity,
    pub class: u16,
    pub code: u16,
    pub first_ts: u64,
    pub last_ts: u64,
    pub count: u32,
    pub desc: String,
    pub actor: Option<String>, // user/IP for iLO events
}

struct Log {
    records: Vec<Record>, // ring, len <= CAP
    head: usize,          // oldest index when full (ring start)
    next_number: u32,
}

impl Log {
    const fn new() -> Self {
        Log { records: Vec::new(), head: 0, next_number: 1 }
    }

    fn raise(&mut self, kind: LogKind, sev: Severity, class: u16, code: u16, desc: &str, actor: Option<&str>) {
        let now = timer::now_ticks();
        // Coalesce: an open (non-Repaired) record of the same kind/class/code.
        for r in self.records.iter_mut() {
            if r.kind == kind && r.class == class && r.code == code && r.severity != Severity::Repaired {
                r.count = r.count.saturating_add(1);
                r.last_ts = now;
                return;
            }
        }
        let rec = Record {
            number: self.next_number,
            kind,
            severity: sev,
            class,
            code,
            first_ts: now,
            last_ts: now,
            count: 1,
            desc: desc.to_string(),
            actor: actor.map(|a| a.to_string()),
        };
        self.next_number = self.next_number.wrapping_add(1);
        if self.records.len() < CAP {
            self.records.push(rec);
        } else {
            self.records[self.head] = rec; // overwrite oldest
            self.head = (self.head + 1) % CAP;
        }
    }

    fn snapshot(&self, kind: LogKind) -> Vec<Record> {
        self.records.iter().filter(|r| r.kind == kind).cloned().collect()
    }

    fn clear(&mut self, kind: LogKind) {
        self.records.retain(|r| r.kind != kind);
        self.head = 0;
    }
}

static LOG: Mutex<RefCell<Log>> = Mutex::new(RefCell::new(Log::new()));

/// Raise an event: coalesce + append to the store (the EventBus entry point).
pub fn raise(kind: LogKind, sev: Severity, class: u16, code: u16, desc: &str, actor: Option<&str>) {
    critical_section::with(|cs| LOG.borrow_ref_mut(cs).raise(kind, sev, class, code, desc, actor));
}

/// Snapshot one log view (IML or iLO) for web/IPMI/RIBCL rendering.
pub fn snapshot(kind: LogKind) -> Vec<Record> {
    critical_section::with(|cs| LOG.borrow_ref_mut(cs).snapshot(kind))
}

/// Number of records in a view.
pub fn count(kind: LogKind) -> usize {
    critical_section::with(|cs| LOG.borrow_ref_mut(cs).records.iter().filter(|r| r.kind == kind).count())
}

/// Clear one log view (CLEAR_IML / CLEAR_EVENTLOG).
pub fn clear(kind: LogKind) {
    critical_section::with(|cs| LOG.borrow_ref_mut(cs).clear(kind));
}

// ---- flash persistence (ping-pong across two blocks) ----

fn sev_to_u8(s: Severity) -> u8 {
    match s {
        Severity::Informational => 0,
        Severity::Caution => 1,
        Severity::Critical => 2,
        Severity::Repaired => 3,
        Severity::Corrected => 4,
        Severity::Uncorrectable => 5,
    }
}
fn u8_to_sev(v: u8) -> Severity {
    match v {
        1 => Severity::Caution,
        2 => Severity::Critical,
        3 => Severity::Repaired,
        4 => Severity::Corrected,
        5 => Severity::Uncorrectable,
        _ => Severity::Informational,
    }
}

fn serialize(records: &[Record], seq: u32) -> Vec<u8> {
    let mut b = Vec::with_capacity(512);
    b.extend_from_slice(&MAGIC.to_le_bytes());
    b.extend_from_slice(&seq.to_le_bytes());
    b.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for r in records {
        b.extend_from_slice(&r.number.to_le_bytes());
        b.push(if r.kind == LogKind::Iml { 0 } else { 1 });
        b.push(sev_to_u8(r.severity));
        b.extend_from_slice(&r.class.to_le_bytes());
        b.extend_from_slice(&r.code.to_le_bytes());
        b.extend_from_slice(&r.first_ts.to_le_bytes());
        b.extend_from_slice(&r.last_ts.to_le_bytes());
        b.extend_from_slice(&r.count.to_le_bytes());
        b.extend_from_slice(&(r.desc.len() as u16).to_le_bytes());
        b.extend_from_slice(r.desc.as_bytes());
        let actor = r.actor.as_deref().unwrap_or("");
        b.extend_from_slice(&(actor.len() as u16).to_le_bytes());
        b.extend_from_slice(actor.as_bytes());
    }
    let crc = ether_crc(&b);
    b.extend_from_slice(&crc.to_le_bytes());
    b
}

/// Persist the log to flash (ping-pong: write the inactive block, bump seq).
/// `active`/`seq` tracking is kept in statics alongside the log.
pub fn save() -> Result<(), spinor::Error> {
    static FLASH: Mutex<RefCell<(u8, u32)>> = Mutex::new(RefCell::new((0, 0)));
    let records = critical_section::with(|cs| {
        let log = LOG.borrow_ref_mut(cs);
        log.records.clone()
    });
    let (active, seq) = critical_section::with(|cs| *FLASH.borrow_ref(cs));
    let target = active ^ 1;
    let next = seq.wrapping_add(1);
    let image = serialize(&records, next);
    if image.len() > BLOCK_SIZE {
        return Err(spinor::Error::TooLong);
    }
    let addr = if target == 0 { LOG_A } else { LOG_B };
    spinor::erase_64k(addr)?;
    spinor::page_program(addr, &image)?;
    critical_section::with(|cs| *FLASH.borrow_ref_mut(cs) = (target, next));
    Ok(())
}
