//! SNMP v1/v2c agent — GET/GETNEXT + traps (Phase 5, docs/35).
//!
//! Community-based (v1/v2c; v3/USM is a later security add). Serves a small MIB:
//! the standard system group + an HP/Compaq (OID base 1.3.6.1.4.1.232) health
//! value, bound to the HAL (sensors). Transport-agnostic: `handle()` takes a
//! request packet and returns the response packet; the UDP listener (smoltcp)
//! wires it to the network. Traps are built from events (eventlog, docs/39).
//!
//! CPQ OIDs/varbinds come from HP's public CPQ MIB files (clean-room: build to
//! the published MIB, not HP code). The one CPQ OID below is a placeholder shape.

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;

use crate::ber;
use crate::{sensors, timer};

/// Default read community (override from NVRAM config, docs/38).
pub const DEFAULT_RO_COMMUNITY: &[u8] = b"public";

/// A MIB value.
pub enum Val {
    Str(Vec<u8>),
    Oid(Vec<u32>),
    Gauge(u32),
    TimeTicks(u32),
    NoSuchObject,
}

fn encode_val(v: &Val) -> Vec<u8> {
    match v {
        Val::Str(s) => ber::octet(s),
        Val::Oid(a) => ber::oid(a),
        Val::Gauge(n) => ber::uint(ber::T_GAUGE32, *n),
        Val::TimeTicks(n) => ber::uint(ber::T_TIMETICKS, *n),
        Val::NoSuchObject => vec![0x80, 0x00], // v2c noSuchObject exception
    }
}

/// System uptime in centiseconds (TimeTicks). 1 MHz counter → /10000 (rate TBD).
fn uptime() -> u32 {
    (timer::now_ticks() / 10_000) as u32
}

// The served MIB, in lexicographic OID order (required for GETNEXT).
const MIB: &[&[u32]] = &[
    &[1, 3, 6, 1, 2, 1, 1, 1, 0], // sysDescr.0
    &[1, 3, 6, 1, 2, 1, 1, 2, 0], // sysObjectID.0
    &[1, 3, 6, 1, 2, 1, 1, 3, 0], // sysUpTime.0
    &[1, 3, 6, 1, 2, 1, 1, 5, 0], // sysName.0
    &[1, 3, 6, 1, 4, 1, 232, 6, 2, 6, 8, 1, 1, 4, 1], // cpq temperature (sensor 0) — placeholder
];

/// Resolve an exact OID to its value, or NoSuchObject.
fn resolve(oid: &[u32]) -> Val {
    match oid {
        [1, 3, 6, 1, 2, 1, 1, 1, 0] => Val::Str(b"iLO3-rs clean-room BMC".to_vec()),
        [1, 3, 6, 1, 2, 1, 1, 2, 0] => Val::Oid(vec![1, 3, 6, 1, 4, 1, 232]),
        [1, 3, 6, 1, 2, 1, 1, 3, 0] => Val::TimeTicks(uptime()),
        [1, 3, 6, 1, 2, 1, 1, 5, 0] => Val::Str(b"ilo3-rs".to_vec()),
        [1, 3, 6, 1, 4, 1, 232, 6, 2, 6, 8, 1, 1, 4, 1] => match sensors::TABLE.first() {
            Some(s) => match s.read() {
                Ok(t) => Val::Gauge(t.max(0) as u32),
                Err(_) => Val::NoSuchObject,
            },
            None => Val::NoSuchObject,
        },
        _ => Val::NoSuchObject,
    }
}

/// Lexicographic OID compare.
fn oid_cmp(a: &[u32], b: &[u32]) -> core::cmp::Ordering {
    a.iter().cmp(b.iter())
}

/// GETNEXT: the first served OID strictly greater than `oid`, with its value.
fn next(oid: &[u32]) -> Option<(Vec<u32>, Val)> {
    for &m in MIB {
        if oid_cmp(m, oid) == core::cmp::Ordering::Greater {
            return Some((m.to_vec(), resolve(m)));
        }
    }
    None
}

/// One varbind: SEQUENCE { name OID, value }.
fn varbind(oid: &[u32], val: &Val) -> Vec<u8> {
    let mut c = ber::oid(oid);
    c.extend_from_slice(&encode_val(val));
    ber::seq(&c)
}

/// Handle a GET/GETNEXT request packet; returns the response packet, or None if
/// malformed or the community does not match (drop silently, SNMP convention).
pub fn handle(packet: &[u8], ro_community: &[u8]) -> Option<Vec<u8>> {
    let (_, msg, _) = ber::read_tlv(packet)?;
    let (_, ver, r) = ber::read_tlv(msg)?; // version INTEGER
    let version = ber::parse_uint(ver)?;
    let (_, comm, r) = ber::read_tlv(r)?; // community OCTET STRING
    if comm != ro_community {
        return None;
    }
    let (pdu_tag, pdu, _) = ber::read_tlv(r)?;
    let is_getnext = pdu_tag == ber::T_GETNEXT;
    if pdu_tag != ber::T_GET && !is_getnext {
        return None;
    }
    let (_, reqid, r) = ber::read_tlv(pdu)?;
    let (_, _errst, r) = ber::read_tlv(r)?;
    let (_, _erridx, r) = ber::read_tlv(r)?;
    let (_, vblist, _) = ber::read_tlv(r)?; // SEQUENCE OF varbind

    // Walk the request varbinds, resolve each.
    let mut out_vbs = Vec::new();
    let mut rest = vblist;
    while let Some((_, vb, next_rest)) = ber::read_tlv(rest) {
        rest = next_rest;
        let (_, name, _) = ber::read_tlv(vb)?; // OID (value is NULL in a request)
        let oid = ber::parse_oid(name)?;
        let (rname, val) = if is_getnext {
            match next(&oid) {
                Some((n, v)) => (n, v),
                None => (oid.clone(), Val::NoSuchObject), // endOfMibView ~ NoSuchObject here
            }
        } else {
            (oid.clone(), resolve(&oid))
        };
        out_vbs.extend_from_slice(&varbind(&rname, &val));
    }

    // Response PDU: same req-id, error-status 0, error-index 0, varbinds.
    let mut pdu_body = Vec::new();
    pdu_body.extend_from_slice(&ber::tlv(ber::T_INT, reqid));
    pdu_body.extend_from_slice(&ber::int(0)); // error-status
    pdu_body.extend_from_slice(&ber::int(0)); // error-index
    pdu_body.extend_from_slice(&ber::seq(&out_vbs));
    let pdu_resp = ber::tlv(ber::T_RESPONSE, &pdu_body);

    let mut msg_body = Vec::new();
    msg_body.extend_from_slice(&ber::int(version as i64));
    msg_body.extend_from_slice(&ber::octet(ro_community));
    msg_body.extend_from_slice(&pdu_resp);
    Some(ber::seq(&msg_body))
}

/// Build an SNMPv2-Trap packet: sysUpTime.0 + snmpTrapOID.0 + extra varbinds.
pub fn trap_v2(community: &[u8], trap_oid: &[u32], extra: &[(&[u32], Val)]) -> Vec<u8> {
    const SYS_UPTIME: &[u32] = &[1, 3, 6, 1, 2, 1, 1, 3, 0];
    const SNMP_TRAP_OID: &[u32] = &[1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0];

    let mut vbs = Vec::new();
    vbs.extend_from_slice(&varbind(SYS_UPTIME, &Val::TimeTicks(uptime())));
    vbs.extend_from_slice(&varbind(SNMP_TRAP_OID, &Val::Oid(trap_oid.to_vec())));
    for (oid, val) in extra {
        vbs.extend_from_slice(&varbind(oid, val));
    }

    let reqid = (timer::now_ticks() as u32) as i64;
    let mut pdu_body = Vec::new();
    pdu_body.extend_from_slice(&ber::int(reqid));
    pdu_body.extend_from_slice(&ber::int(0)); // error-status
    pdu_body.extend_from_slice(&ber::int(0)); // error-index
    pdu_body.extend_from_slice(&ber::seq(&vbs));
    let pdu = ber::tlv(ber::T_TRAP_V2, &pdu_body);

    let mut msg = Vec::new();
    msg.extend_from_slice(&ber::int(1)); // version 2c = 1
    msg.extend_from_slice(&ber::octet(community));
    msg.extend_from_slice(&pdu);
    ber::seq(&msg)
}
