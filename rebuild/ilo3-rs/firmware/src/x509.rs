//! Minimal X.509 DER parsing + Ed25519 signature verification (Phase 4, mTLS).
//!
//! Hand-rolled on purpose: the standard `x509-parser` crate drags in `nom` with
//! `std` (via der-parser/asn1-rs), which cannot build on the armv5te no_std
//! target. We only need enough DER to (a) pull the Ed25519 subject public key,
//! (b) pull the signature + signature algorithm, (c) locate issuer/subject DN
//! bytes for chain matching, and (d) verify a cert's signature over its own
//! TBSCertificate with ed25519-dalek. Zero-copy, no alloc — all borrows.
//!
//! Supported algorithm: Ed25519 only (OID 1.3.101.112). This is the only cert
//! type the rewrite issues/verifies; RSA cert verification is a later addition.

#![allow(dead_code)]

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// DER-encoded OID value for Ed25519 (1.3.101.112): 43 * 40 + 3 = ... -> {0x2b, 0x65, 0x70}.
pub const ED25519_OID: &[u8] = &[0x2b, 0x65, 0x70];

/// A signature algorithm we can verify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SigAlgo {
    Ed25519,
    Other,
}

/// A parsed X.509 certificate (borrows the DER it was parsed from).
pub struct Cert<'a> {
    pub tbs_der: &'a [u8],  // raw DER of TBSCertificate (signed bytes)
    pub sig_algo: SigAlgo,
    pub sig: &'a [u8],      // raw signature bytes (64 for Ed25519)
    pub issuer: &'a [u8],   // raw DER of issuer Name (RDNSequence)
    pub subject: &'a [u8],  // raw DER of subject Name
    pub spki: &'a [u8],     // raw 32-byte Ed25519 public key
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
/// *content* (the bytes inside the SPKI SEQUENCE: AlgorithmIdentifier + BIT STRING).
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

/// Walk the fixed prefix of a TBSCertificate to locate issuer, subject, SPKI.
fn parse_tbs(tbs_der: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let mut rest = tbs_der;
    // optional [0] EXPLICIT version
    if rest.first() == Some(&0xA0) {
        let (_, _, r) = read_tlv(rest)?;
        rest = r;
    }
    // serialNumber INTEGER
    let (t, _, r) = read_tlv(rest)?;
    if t != 0x02 {
        return None;
    }
    rest = r;
    // signature AlgorithmIdentifier
    let (t, _, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    rest = r;
    // issuer Name
    let (t, issuer, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    rest = r;
    // validity
    let (t, _, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    rest = r;
    // subject Name
    let (t, subject, r) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    rest = r;
    // subjectPublicKeyInfo
    let (t, spki_der, _) = read_tlv(rest)?;
    if t != 0x30 {
        return None;
    }
    let spki = parse_spki(spki_der)?;
    Some((issuer, subject, spki))
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
    // The signature covers the *complete* TBSCertificate DER (SEQUENCE header +
    // content), so capture the full element bytes, not just the content.
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
    let (issuer, subject, spki) = parse_tbs(tbs_content)?;
    Some(Cert { tbs_der: tbs_full, sig_algo, sig, issuer, subject, spki })
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
/// Accepts both the direct seed (32-byte OCTET STRING) and the nested
/// `04 20 <seed>` CurvePrivateKey form that OpenSSL emits.
pub fn ed25519_seed_from_pkcs8(der: &[u8]) -> Option<[u8; 32]> {
    // PrivateKeyInfo ::= SEQUENCE { version INTEGER, alg AlgorithmIdentifier,
    //                              privateKey OCTET STRING }
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

/// Verify a certificate chain (leaf first) against a set of trust-anchor certs.
///
/// Each cert's signature is verified with the next cert's key (DN-matched), and
/// the final cert must chain to a trust anchor whose subject DN equals the
/// cert's issuer DN. Returns true only if every link verifies.
pub fn verify_chain(chain: &[&[u8]], anchors: &[&[u8]]) -> bool {
    if chain.is_empty() {
        return false;
    }
    for i in 0..chain.len() {
        let Some(cert) = parse_cert(chain[i]) else { return false };
        let issuer_pub: &[u8] = if i + 1 < chain.len() {
            let Some(issuer) = parse_cert(chain[i + 1]) else { return false };
            if issuer.subject != cert.issuer {
                return false; // issuer DN must match the next cert's subject DN
            }
            issuer.spki
        } else {
            let mut found = None;
            for a in anchors {
                let Some(anchor) = parse_cert(a) else { continue };
                if anchor.subject == cert.issuer {
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
