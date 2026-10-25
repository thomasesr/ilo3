//! Minimal X.509 DER parsing + Ed25519 signature verification (Phase 4, mTLS).
//!
//! Hand-rolled on purpose: the standard `x509-parser` crate drags in `nom` with
//! `std` (via der-parser/asn1-rs), which cannot build on the armv5te no_std
//! target. We parse only what we need: the Ed25519 subject public key, the
//! signature + algorithm, the issuer/subject DN bytes for chain matching, the
//! **basicConstraints CA flag** (so a leaf cert cannot forge an issuer), and the
//! **validity period** (notBefore/notAfter). Zero-copy, no alloc — all borrows.
//!
//! Supported algorithm: Ed25519 only (OID 1.3.101.112).

#![allow(dead_code)]

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// DER OID value for Ed25519 (1.3.101.112).
pub const ED25519_OID: &[u8] = &[0x2b, 0x65, 0x70];
/// DER OID value for basicConstraints (2.5.29.19).
const BASIC_CONSTRAINTS_OID: &[u8] = &[0x55, 0x1d, 0x13];

/// A signature algorithm we can verify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigAlgo {
    Ed25519,
    Other,
}

/// A parsed X.509 certificate (borrows the DER it was parsed from).
pub struct Cert<'a> {
    pub tbs_der: &'a [u8], // raw DER of TBSCertificate (signed bytes)
    pub sig_algo: SigAlgo,
    pub sig: &'a [u8],     // raw signature bytes (64 for Ed25519)
    pub issuer: &'a [u8],  // raw DER of issuer Name (RDNSequence)
    pub subject: &'a [u8], // raw DER of subject Name
    pub spki: &'a [u8],    // raw 32-byte Ed25519 public key
    pub is_ca: bool,       // basicConstraints CA:TRUE
    pub not_before: i64,   // unix seconds (i64::MIN if unparsable)
    pub not_after: i64,    // unix seconds (i64::MAX if unparsable)
}

/// Read one DER TLV element: returns (tag, value, rest-of-buffer).
fn read_tlv(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = buf.split_first()?;
    let (&l0, rest) = rest.split_first()?;
    let (len, rest) = if l0 & 0x80 == 0 {
        (l0 as usize, rest)
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let mut len = 0usize;
        for &b in &rest[..n] {
            len = (len << 8) | b as usize;
        }
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

/// The OID value of an AlgorithmIdentifier *content* (first element is the OID).
fn alg_oid(alg_body: &[u8]) -> Option<&[u8]> {
    let (to, oid_val, _) = read_tlv(alg_body)?;
    if to != 0x06 {
        return None;
    }
    Some(oid_val)
}

/// Extract the raw Ed25519 public key (32 bytes) from a SubjectPublicKeyInfo
/// *content* (AlgorithmIdentifier + BIT STRING).
fn parse_spki(spki_body: &[u8]) -> Option<&[u8]> {
    let (ta, alg_body, r) = read_tlv(spki_body)?;
    if ta != 0x30 {
        return None;
    }
    let (to, oid_val, _) = read_tlv(alg_body)?;
    if to != 0x06 || oid_val != ED25519_OID {
        return None;
    }
    let (tb, bits, _) = read_tlv(r)?;
    if tb != 0x03 {
        return None;
    }
    let (_, key) = bits.split_first()?; // skip the unused-bits count byte
    if key.len() != 32 {
        return None;
    }
    Some(key)
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse an ASCII integer field of fixed width from `b` at `off`.
fn num(b: &[u8], off: usize, w: usize) -> Option<i64> {
    let mut v = 0i64;
    for &c in b.get(off..off + w)? {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as i64;
    }
    Some(v)
}

/// Convert an X.509 Time (UTCTime 0x17 / GeneralizedTime 0x18, "...Z") to unix
/// seconds. Only the UTC "Z" forms are accepted (RFC 5280 profile).
fn der_time_to_unix(tag: u8, v: &[u8]) -> Option<i64> {
    // UTCTime: YYMMDDHHMMSSZ (13); GeneralizedTime: YYYYMMDDHHMMSSZ (15).
    let (year, o) = match tag {
        0x17 if v.len() == 13 && v[12] == b'Z' => {
            let yy = num(v, 0, 2)?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, 2)
        }
        0x18 if v.len() == 15 && v[14] == b'Z' => (num(v, 0, 4)?, 4),
        _ => return None,
    };
    let mon = num(v, o, 2)?;
    let day = num(v, o + 2, 2)?;
    let hh = num(v, o + 4, 2)?;
    let mm = num(v, o + 6, 2)?;
    let ss = num(v, o + 8, 2)?;
    if !(1..=12).contains(&mon) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, mon, day) * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Parse (notBefore, notAfter) from a Validity SEQUENCE content.
fn parse_validity(body: &[u8]) -> (i64, i64) {
    let parse_two = || -> Option<(i64, i64)> {
        let (t1, nb, r) = read_tlv(body)?;
        let (t2, na, _) = read_tlv(r)?;
        Some((der_time_to_unix(t1, nb)?, der_time_to_unix(t2, na)?))
    };
    parse_two().unwrap_or((i64::MIN, i64::MAX))
}

/// Determine basicConstraints CA:TRUE from an Extensions [3] EXPLICIT content
/// (which wraps a SEQUENCE OF Extension). Absent/unparsable ⇒ not a CA.
fn parse_is_ca(ext_explicit: &[u8]) -> bool {
    let inner = match read_tlv(ext_explicit) {
        Some((0x30, seq, _)) => seq, // SEQUENCE OF Extension
        _ => return false,
    };
    let mut rest = inner;
    while let Some((t, ext, r)) = read_tlv(rest) {
        rest = r;
        if t != 0x30 {
            continue;
        }
        // Extension ::= SEQUENCE { extnID OID, critical BOOL DEFAULT FALSE, extnValue OCTET STRING }
        let Some((0x06, oid, mut er)) = read_tlv(ext) else { continue };
        if oid != BASIC_CONSTRAINTS_OID {
            continue;
        }
        // optional critical BOOLEAN
        if let Some((0x01, _crit, r2)) = read_tlv(er) {
            er = r2;
        }
        // extnValue OCTET STRING wrapping BasicConstraints SEQUENCE
        let Some((0x04, val, _)) = read_tlv(er) else { return false };
        let Some((0x30, bc, _)) = read_tlv(val) else { return false };
        // BasicConstraints ::= SEQUENCE { cA BOOLEAN DEFAULT FALSE, ... }
        return matches!(read_tlv(bc), Some((0x01, b, _)) if b.first().copied().unwrap_or(0) != 0);
    }
    false
}

/// Walk the TBSCertificate to locate the inner signature-alg OID, issuer,
/// subject, SPKI, CA flag, and validity.
fn parse_tbs(tbs_der: &[u8]) -> Option<(&[u8], &[u8], &[u8], &[u8], bool, i64, i64)> {
    let mut rest = tbs_der;
    // optional [0] EXPLICIT version
    if rest.first() == Some(&0xA0) {
        rest = read_tlv(rest)?.2;
    }
    // serialNumber INTEGER
    let (t, _, r) = read_tlv(rest)?;
    if t != 0x02 {
        return None;
    }
    rest = r;
    // signature AlgorithmIdentifier — capture its OID for the outer/inner match.
    let (t, inner_alg, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    let inner_sig_oid = alg_oid(inner_alg)?;
    rest = r;
    // issuer Name
    let (t, issuer, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    rest = r;
    // validity SEQUENCE { notBefore, notAfter }
    let (t, validity, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    let (not_before, not_after) = parse_validity(validity);
    rest = r;
    // subject Name
    let (t, subject, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    rest = r;
    // subjectPublicKeyInfo
    let (t, spki_der, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    let spki = parse_spki(spki_der)?;
    rest = r;
    // optional [1] issuerUniqueID / [2] subjectUniqueID, then [3] extensions
    let mut is_ca = false;
    while let Some((t, _v, r)) = read_tlv(rest) {
        match t {
            0x81 | 0x82 => rest = r, // unique IDs — skip
            0xA3 => {
                is_ca = parse_is_ca(_v);
                break;
            }
            _ => break,
        }
    }
    Some((inner_sig_oid, issuer, subject, spki, is_ca, not_before, not_after))
}

/// Parse a full DER Certificate into a borrowed `Cert`.
pub fn parse_cert(der: &[u8]) -> Option<Cert<'_>> {
    let (t0, cert_body, _) = read_tlv(der)?;
    if t0 != 0x30 {
        return None;
    }
    let (t1, tbs_content, r1) = read_tlv(cert_body)?;
    if t1 != 0x30 {
        return None;
    }
    // The signature covers the complete TBSCertificate DER element.
    let tbs_full = &cert_body[..cert_body.len() - r1.len()];
    let (t2, sig_algo_body, r2) = read_tlv(r1)?;
    if t2 != 0x30 {
        return None;
    }
    let (t3, sig_bits, _) = read_tlv(r2)?;
    if t3 != 0x03 {
        return None;
    }
    let sig_algo = match alg_oid(sig_algo_body) {
        Some(oid) if oid == ED25519_OID => SigAlgo::Ed25519,
        _ => SigAlgo::Other,
    };
    let (_, sig) = sig_bits.split_first()?; // skip unused-bits count byte
    let (inner_oid, issuer, subject, spki, is_ca, not_before, not_after) = parse_tbs(tbs_content)?;
    // RFC 5280 §4.1.1.2: the inner TBS signature alg MUST equal the outer
    // signatureAlgorithm (reject algorithm-substitution mismatches).
    if alg_oid(sig_algo_body) != Some(inner_oid) {
        return None;
    }
    Some(Cert { tbs_der: tbs_full, sig_algo, sig, issuer, subject, spki, is_ca, not_before, not_after })
}

/// Verify a cert's signature over its TBSCertificate using the issuer's Ed25519
/// public key (32 bytes).
pub fn verify_cert_sig(cert: &Cert, issuer_pub: &[u8]) -> bool {
    if cert.sig_algo != SigAlgo::Ed25519 || cert.sig.len() != 64 || issuer_pub.len() != 32 {
        return false;
    }
    let key: [u8; 32] = match issuer_pub.try_into() {
        Ok(k) => k,
        Err(_) => return false,
    };
    let Ok(vk) = VerifyingKey::from_bytes(&key) else { return false };
    let Ok(sig) = Signature::from_slice(cert.sig) else { return false };
    vk.verify(cert.tbs_der, &sig).is_ok()
}

/// Extract the 32-byte Ed25519 seed from a PKCS8 (RFC 8410) private-key DER.
pub fn ed25519_seed_from_pkcs8(der: &[u8]) -> Option<[u8; 32]> {
    let (t0, body, _) = read_tlv(der)?;
    if t0 != 0x30 {
        return None;
    }
    let (t1, _ver, r1) = read_tlv(body)?;
    if t1 != 0x02 {
        return None;
    }
    let (t2, _alg, r2) = read_tlv(r1)?;
    if t2 != 0x30 {
        return None;
    }
    let (t3, priv_, _) = read_tlv(r2)?;
    if t3 != 0x04 {
        return None;
    }
    let seed: &[u8] = if priv_.len() == 32 {
        priv_
    } else {
        let (t4, inner, _) = read_tlv(priv_)?;
        if t4 != 0x04 || inner.len() != 32 {
            return None;
        }
        inner
    };
    seed.try_into().ok()
}

/// True if `now` (unix seconds) is outside the cert's validity window.
fn expired(cert: &Cert, now: i64) -> bool {
    now < cert.not_before || now > cert.not_after
}

/// Verify a certificate chain (leaf first) against a set of trust-anchor certs.
///
/// Each cert's signature is verified with the next cert's key (DN-matched);
/// **every cert used as an issuer (intermediates + the matched anchor) must be a
/// CA** (basicConstraints CA:TRUE) — so a plain leaf cannot forge an issuer. The
/// final cert must chain to a CA trust anchor whose subject DN equals its issuer
/// DN. When `now_unix` is `Some`, every cert's validity period is enforced; pass
/// `None` only before a trusted wall-clock is available (no time source yet).
pub fn verify_chain(chain: &[&[u8]], anchors: &[&[u8]], now_unix: Option<u64>) -> bool {
    if chain.is_empty() {
        return false;
    }
    let now = now_unix.map(|t| t as i64);
    for i in 0..chain.len() {
        let Some(cert) = parse_cert(chain[i]) else { return false };
        if let Some(n) = now {
            if expired(&cert, n) {
                return false;
            }
        }
        let issuer_pub: &[u8] = if i + 1 < chain.len() {
            let Some(issuer) = parse_cert(chain[i + 1]) else { return false };
            if !issuer.is_ca || issuer.subject != cert.issuer {
                return false; // issuer must be a CA whose subject DN matches
            }
            issuer.spki
        } else {
            let mut found = None;
            for a in anchors {
                let Some(anchor) = parse_cert(a) else { continue };
                if anchor.is_ca && anchor.subject == cert.issuer {
                    if let Some(n) = now {
                        if expired(&anchor, n) {
                            continue;
                        }
                    }
                    found = Some(anchor.spki);
                    break;
                }
            }
            let Some(pk) = found else { return false };
            pk
        };
        if !verify_cert_sig(&cert, issuer_pub) {
            return false;
        }
    }
    true
}
