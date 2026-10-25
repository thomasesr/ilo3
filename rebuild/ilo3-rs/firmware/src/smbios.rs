//! Host inventory via SMBIOS over ROM-CHIF (Phase 6, docs/22).
//!
//! CPU + memory inventory come from the host BIOS SMBIOS/DMI table, streamed to
//! the BMC over the ROM-CHIF channel (romchf_service) and parsed here. PECI/SPD
//! are thermal/health only — SMBIOS is the authoritative inventory (as iLO does).
//!
//! This module is the SMBIOS record parser (type 4 Processor, type 17 Memory
//! Device) + the opcode-3 "get record by type" request builder. The transport is
//! crate::chif (HW-gated); feed the fetched record bytes to the parsers here. The
//! parsers are standard DMTF SMBIOS and testable without hardware.

#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;

/// SMBIOS structure header (type, formatted length, handle).
pub struct Header {
    pub rtype: u8,
    pub length: u8,
    pub handle: u16,
}

pub fn header(rec: &[u8]) -> Option<Header> {
    if rec.len() < 4 {
        return None;
    }
    Some(Header { rtype: rec[0], length: rec[1], handle: u16::from_le_bytes([rec[2], rec[3]]) })
}

/// Resolve a 1-based SMBIOS string reference from the string-set that follows the
/// formatted area (`length`..). Index 0 = "no string".
fn string(rec: &[u8], length: usize, index: u8) -> String {
    if index == 0 || length >= rec.len() {
        return String::new();
    }
    let mut n = 1u8;
    let mut start = length;
    let mut i = length;
    while i < rec.len() {
        if rec[i] == 0 {
            if n == index {
                return String::from_utf8_lossy(&rec[start..i]).into_owned();
            }
            n += 1;
            start = i + 1;
            if rec.get(i + 1) == Some(&0) {
                break; // double-null = end of string set
            }
        }
        i += 1;
    }
    String::new()
}

#[inline]
fn u16at(rec: &[u8], off: usize) -> u16 {
    rec.get(off..off + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

/// A processor, from SMBIOS type 4.
pub struct Cpu {
    pub socket: String,
    pub manufacturer: String,
    pub model: String,
    pub max_speed_mhz: u16,
    pub cores: u8,
    pub populated: bool,
}

/// Parse an SMBIOS type-4 (Processor Information) record.
pub fn parse_processor(rec: &[u8]) -> Option<Cpu> {
    let h = header(rec)?;
    if h.rtype != 4 {
        return None;
    }
    let len = h.length as usize;
    let status = *rec.get(0x18)?; // processor status (bit6 = CPU socket populated)
    Some(Cpu {
        socket: string(rec, len, *rec.get(0x04)?),
        manufacturer: string(rec, len, *rec.get(0x07)?),
        model: string(rec, len, *rec.get(0x10)?), // version string = model name
        max_speed_mhz: u16at(rec, 0x16),
        cores: *rec.get(0x23).unwrap_or(&0),
        populated: status & 0x40 != 0,
    })
}

/// A memory device (DIMM slot), from SMBIOS type 17.
pub struct Dimm {
    pub locator: String,
    pub bank: String,
    pub size_mb: u32,
    pub speed_mhz: u16,
    pub mem_type: u8,
    pub populated: bool,
}

/// Parse an SMBIOS type-17 (Memory Device) record.
pub fn parse_memory(rec: &[u8]) -> Option<Dimm> {
    let h = header(rec)?;
    if h.rtype != 17 {
        return None;
    }
    let len = h.length as usize;
    let raw = u16at(rec, 0x0C); // size: 0 = empty, bit15 = value is in KB
    let (populated, size_mb) = match raw {
        0 => (false, 0),
        0xFFFF => (true, 0), // unknown size
        v if v & 0x8000 != 0 => (true, ((v & 0x7FFF) as u32) / 1024),
        v => (true, v as u32),
    };
    Some(Dimm {
        locator: string(rec, len, *rec.get(0x10)?),
        bank: string(rec, len, *rec.get(0x11)?),
        size_mb,
        speed_mhz: u16at(rec, 0x15),
        mem_type: *rec.get(0x12).unwrap_or(&0),
        populated,
    })
}

/// Build a ROM-CHIF "get record by type + handle" request (opcode 3, docs/22).
/// Send over the romchf CHIF channel; the reply is the SMBIOS record bytes.
pub fn request_by_type(rtype: u8, handle: u16) -> Vec<u8> {
    let mut r = Vec::with_capacity(4);
    r.push(3); // opcode 3 = get-record-by-type
    r.push(rtype);
    r.extend_from_slice(&handle.to_le_bytes());
    r
}
