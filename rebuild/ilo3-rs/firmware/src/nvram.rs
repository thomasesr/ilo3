//! NVRAM configuration store — named key-value, flash-backed (Phase 5, docs/38).
//!
//! A ping-pong wear-leveled K-V store: two flash blocks (A/B); each save writes
//! the whole store to the *inactive* block with an incremented sequence number,
//! so a power-fail mid-write leaves the previous block intact (atomic). Load
//! picks the valid block with the higher sequence. Each key carries a privilege
//! mask (docs/38) so callers can gate reads/writes against the user bitmap.
//!
//! Keeps the original UPPERCASE key names (SNMP_*, HTTP_PORT, HOST_KEY, …) for
//! config compatibility. Flash I/O rides on the SMIF driver — CONFIRM-ON-HW.

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::mac::ether_crc; // CRC-32 used purely as an integrity check
use crate::spinor;

const BLOCK_A: u32 = 0x0068_0000;
const BLOCK_B: u32 = 0x0069_0000;
const BLOCK_SIZE: usize = 0x1_0000; // 64 KB erase block
const MAGIC: u32 = 0x474E_5847; // "GXNV"

struct Entry {
    priv_mask: u32,
    val: Vec<u8>,
}

/// The in-RAM config store; mirror of the active flash block.
pub struct Nvram {
    map: BTreeMap<String, Entry>,
    active: u8, // 0 = A, 1 = B (block currently holding the live copy)
    seq: u32,
}

fn block_addr(which: u8) -> u32 {
    if which == 0 {
        BLOCK_A
    } else {
        BLOCK_B
    }
}

/// Parse a serialized block image → (seq, map). None if magic/CRC is bad.
fn parse_block(buf: &[u8]) -> Option<(u32, BTreeMap<String, Entry>)> {
    if buf.len() < 12 || u32::from_le_bytes(buf[0..4].try_into().ok()?) != MAGIC {
        return None;
    }
    let seq = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    let count = u32::from_le_bytes(buf[8..12].try_into().ok()?) as usize;
    let mut p = 12usize;
    let mut map = BTreeMap::new();
    for _ in 0..count {
        let klen = *buf.get(p)? as usize;
        p += 1;
        let key = core::str::from_utf8(buf.get(p..p + klen)?).ok()?.to_string();
        p += klen;
        let priv_mask = u32::from_le_bytes(buf.get(p..p + 4)?.try_into().ok()?);
        p += 4;
        let vlen = u16::from_le_bytes(buf.get(p..p + 2)?.try_into().ok()?) as usize;
        p += 2;
        let val = buf.get(p..p + vlen)?.to_vec();
        p += vlen;
        map.insert(key, Entry { priv_mask, val });
    }
    // trailing CRC-32 over bytes [0..p]
    let crc = u32::from_le_bytes(buf.get(p..p + 4)?.try_into().ok()?);
    if crc != ether_crc(&buf[..p]) {
        return None;
    }
    Some((seq, map))
}

impl Nvram {
    /// Load the store from flash, choosing the valid block with the higher seq.
    /// An empty/corrupt pair yields a fresh empty store targeting block A.
    pub fn load() -> Self {
        let mut best: Option<(u8, u32, BTreeMap<String, Entry>)> = None;
        for which in 0..2u8 {
            let mut buf = vec![0u8; BLOCK_SIZE];
            if spinor::read(block_addr(which), &mut buf).is_err() {
                continue;
            }
            if let Some((seq, map)) = parse_block(&buf) {
                if best.as_ref().map_or(true, |(_, s, _)| seq > *s) {
                    best = Some((which, seq, map));
                }
            }
        }
        match best {
            // The loaded block is active; its seq is current.
            Some((which, seq, map)) => Nvram { map, active: which, seq },
            None => Nvram { map: BTreeMap::new(), active: 0, seq: 0 },
        }
    }

    /// Read a value (and its privilege mask) by key.
    pub fn get(&self, key: &str) -> Option<(&[u8], u32)> {
        self.map.get(key).map(|e| (e.val.as_slice(), e.priv_mask))
    }

    /// Set a key in RAM (call `commit` to persist). `priv_mask` is the privilege
    /// required to modify it (enforced by callers against the user bitmap).
    pub fn set(&mut self, key: &str, priv_mask: u32, val: &[u8]) {
        self.map.insert(key.to_string(), Entry { priv_mask, val: val.to_vec() });
    }

    /// Delete a key in RAM (call `commit` to persist).
    pub fn delete(&mut self, key: &str) -> bool {
        self.map.remove(key).is_some()
    }

    /// Serialize the store to a flash image (magic, seq, records, CRC).
    fn serialize(&self, seq: u32) -> Vec<u8> {
        let mut b = Vec::with_capacity(256);
        b.extend_from_slice(&MAGIC.to_le_bytes());
        b.extend_from_slice(&seq.to_le_bytes());
        b.extend_from_slice(&(self.map.len() as u32).to_le_bytes());
        for (k, e) in &self.map {
            b.push(k.len() as u8);
            b.extend_from_slice(k.as_bytes());
            b.extend_from_slice(&e.priv_mask.to_le_bytes());
            b.extend_from_slice(&(e.val.len() as u16).to_le_bytes());
            b.extend_from_slice(&e.val);
        }
        let crc = ether_crc(&b);
        b.extend_from_slice(&crc.to_le_bytes());
        b
    }

    /// Persist the store atomically to the inactive block (write new, bump seq),
    /// then switch to it. The previous block stays intact until overwritten, so a
    /// mid-write power loss falls back to it on the next load.
    pub fn commit(&mut self) -> Result<(), spinor::Error> {
        let target = self.active ^ 1;
        let next_seq = self.seq.wrapping_add(1);
        let image = self.serialize(next_seq);
        if image.len() > BLOCK_SIZE {
            return Err(spinor::Error::TooLong);
        }
        let addr = block_addr(target);
        spinor::erase_64k(addr)?;
        spinor::page_program(addr, &image)?;
        self.active = target;
        self.seq = next_seq;
        Ok(())
    }
}
