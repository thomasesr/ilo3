//! Minimal ASN.1 BER codec — the SNMP subset (Phase 5, docs/35).
//!
//! Definite-length TLV encode/decode for exactly the types SNMP v1/v2c uses:
//! INTEGER, OCTET STRING, NULL, OID, SEQUENCE, the SNMP application types
//! (IpAddress/Counter32/Gauge32/TimeTicks), and the context PDU tags. Hand-rolled
//! because no no_std SNMP/ASN.1 crate fits; small + allocating (we have a heap).

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;

// Universal tags.
pub const T_INT: u8 = 0x02;
pub const T_OCTET: u8 = 0x04;
pub const T_NULL: u8 = 0x05;
pub const T_OID: u8 = 0x06;
pub const T_SEQ: u8 = 0x30;
// SNMP application tags.
pub const T_IPADDR: u8 = 0x40;
pub const T_COUNTER32: u8 = 0x41;
pub const T_GAUGE32: u8 = 0x42;
pub const T_TIMETICKS: u8 = 0x43;
// SNMP PDU (context-specific, constructed).
pub const T_GET: u8 = 0xA0;
pub const T_GETNEXT: u8 = 0xA1;
pub const T_RESPONSE: u8 = 0xA2;
pub const T_TRAP_V2: u8 = 0xA7;

/// Append a definite length (short or long form).
fn push_len(out: &mut Vec<u8>, len: usize) {
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = (len.max(1).ilog2() / 8 + 1) as usize;
        out.push(0x80 | bytes as u8);
        for i in (0..bytes).rev() {
            out.push((len >> (i * 8)) as u8);
        }
    }
}

/// Build a TLV from a tag + content.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(content.len() + 4);
    v.push(tag);
    push_len(&mut v, content.len());
    v.extend_from_slice(content);
    v
}

/// SEQUENCE wrapper.
pub fn seq(content: &[u8]) -> Vec<u8> {
    tlv(T_SEQ, content)
}

/// Encode a signed INTEGER (minimal two's-complement).
pub fn int(mut n: i64) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        body.insert(0, (n & 0xff) as u8);
        n >>= 8;
        // stop when sign-extended fully: n is 0/-1 and the top bit agrees
        let top = body[0] & 0x80;
        if (n == 0 && top == 0) || (n == -1 && top != 0) {
            break;
        }
    }
    tlv(T_INT, &body)
}

/// Encode an unsigned value under an application tag (Counter32/Gauge32/TimeTicks).
pub fn uint(tag: u8, n: u32) -> Vec<u8> {
    let mut body = n.to_be_bytes().to_vec();
    while body.len() > 1 && body[0] == 0 {
        body.remove(0); // trim leading zeros
    }
    if body[0] & 0x80 != 0 {
        body.insert(0, 0); // keep it non-negative
    }
    tlv(tag, &body)
}

/// OCTET STRING.
pub fn octet(s: &[u8]) -> Vec<u8> {
    tlv(T_OCTET, s)
}

/// NULL.
pub fn null() -> Vec<u8> {
    vec![T_NULL, 0x00]
}

/// Encode an OBJECT IDENTIFIER from its arcs (>= 2 arcs).
pub fn oid(arcs: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    if arcs.len() >= 2 {
        body.push((arcs[0] * 40 + arcs[1]) as u8);
        for &a in &arcs[2..] {
            // base-128, big-endian, MSB set on all but the last byte
            let mut tmp = [0u8; 5];
            let mut i = 5;
            let mut v = a;
            loop {
                i -= 1;
                tmp[i] = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    break;
                }
            }
            for j in i..4 {
                body.push(tmp[j] | 0x80);
            }
            body.push(tmp[4]);
        }
    }
    tlv(T_OID, &body)
}

/// Read one TLV: returns (tag, content, rest). Definite length only.
pub fn read_tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, r) = b.split_first()?;
    let (&l0, r) = r.split_first()?;
    let (len, r) = if l0 & 0x80 == 0 {
        (l0 as usize, r)
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 4 || r.len() < n {
            return None;
        }
        let mut len = 0usize;
        for &x in &r[..n] {
            len = (len << 8) | x as usize;
        }
        (len, &r[n..])
    };
    if r.len() < len {
        return None;
    }
    Some((tag, &r[..len], &r[len..]))
}

/// Parse an unsigned integer value (INTEGER/Counter/Gauge/TimeTicks content).
pub fn parse_uint(content: &[u8]) -> Option<u64> {
    if content.is_empty() || content.len() > 8 {
        return None;
    }
    let mut v = 0u64;
    for &b in content {
        v = (v << 8) | b as u64;
    }
    Some(v)
}

/// Parse an OBJECT IDENTIFIER content into its arcs.
pub fn parse_oid(content: &[u8]) -> Option<Vec<u32>> {
    let (&first, rest) = content.split_first()?;
    let mut arcs = vec![(first / 40) as u32, (first % 40) as u32];
    let mut acc = 0u32;
    for &b in rest {
        acc = (acc << 7) | (b & 0x7f) as u32;
        if b & 0x80 == 0 {
            arcs.push(acc);
            acc = 0;
        }
    }
    Some(arcs)
}
