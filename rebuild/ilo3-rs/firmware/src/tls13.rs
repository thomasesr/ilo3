//! TLS 1.3 key schedule + record protection (Phase 4, RFC 8446 §7.1/§5.2).
//!
//! The cryptographic core of TLS 1.3 for the TLS_CHACHA20_POLY1305_SHA256 suite,
//! built on crate::crypto primitives + the heap (`alloc`). This is correct and
//! RFC-verifiable; the handshake *message codec* + X.509 chain verification (the
//! large, negotiation-heavy part) sit on top of this and are the remaining work.
//!
//! Key schedule: Extract/Expand-Label/Derive-Secret → early/handshake/master
//! secret tree → per-direction traffic secrets → {key, iv, finished_key}.
//! Record layer: per-record nonce = iv XOR seq; AEAD over the record with the
//! TLS record header as additional data.

#![allow(dead_code)]

use alloc::vec::Vec;

use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::crypto::{aead_open, aead_seal};

const HASH_LEN: usize = 32; // SHA-256
pub const KEY_LEN: usize = 32; // ChaCha20 key
pub const IV_LEN: usize = 12; // AEAD nonce

type HmacSha256 = Hmac<Sha256>;

/// HKDF-Extract (= HMAC-Hash(salt, ikm)); empty salt → all-zero key.
fn extract(salt: &[u8], ikm: &[u8]) -> [u8; HASH_LEN] {
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
    prk.into()
}

/// HKDF-Expand-Label(secret, label, context, len) — RFC 8446 §7.1.
pub fn expand_label(secret: &[u8; HASH_LEN], label: &[u8], context: &[u8], out: &mut [u8]) {
    // struct HkdfLabel { u16 length; opaque label<7..255>="tls13 "+label; opaque context<0..255> }
    let mut info: Vec<u8> = Vec::with_capacity(4 + 6 + label.len() + context.len());
    info.extend_from_slice(&(out.len() as u16).to_be_bytes());
    info.push((6 + label.len()) as u8);
    info.extend_from_slice(b"tls13 ");
    info.extend_from_slice(label);
    info.push(context.len() as u8);
    info.extend_from_slice(context);
    let hk = Hkdf::<Sha256>::from_prk(secret).expect("prk len");
    hk.expand(&info, out).expect("expand len");
}

/// Derive-Secret(secret, label, transcript_hash).
pub fn derive_secret(secret: &[u8; HASH_LEN], label: &[u8], transcript: &[u8; HASH_LEN]) -> [u8; HASH_LEN] {
    let mut out = [0u8; HASH_LEN];
    expand_label(secret, label, transcript, &mut out);
    out
}

/// Traffic-key material for one direction/secret.
#[derive(Clone, Copy)]
pub struct TrafficKeys {
    pub key: [u8; KEY_LEN],
    pub iv: [u8; IV_LEN],
    pub finished_key: [u8; HASH_LEN],
}

/// Derive {key, iv, finished_key} from a traffic secret (RFC 8446 §7.3).
pub fn traffic_keys(secret: &[u8; HASH_LEN]) -> TrafficKeys {
    let mut k = TrafficKeys { key: [0; KEY_LEN], iv: [0; IV_LEN], finished_key: [0; HASH_LEN] };
    expand_label(secret, b"key", &[], &mut k.key);
    expand_label(secret, b"iv", &[], &mut k.iv);
    expand_label(secret, b"finished", &[], &mut k.finished_key);
    k
}

/// The TLS 1.3 key schedule, advanced as the handshake progresses.
pub struct KeySchedule {
    transcript: Sha256,
    secret: [u8; HASH_LEN], // current stage secret
}

impl KeySchedule {
    /// Start with early_secret = Extract(0, PSK-or-0).
    pub fn new() -> Self {
        KeySchedule { transcript: Sha256::new(), secret: extract(&[0u8; HASH_LEN], &[0u8; HASH_LEN]) }
    }

    /// Feed a handshake message into the transcript hash.
    pub fn update_transcript(&mut self, msg: &[u8]) {
        self.transcript.update(msg);
    }

    /// Current transcript hash (clone-and-finalize, non-destructive).
    pub fn transcript_hash(&self) -> [u8; HASH_LEN] {
        self.transcript.clone().finalize().into()
    }

    /// Advance to the handshake secret from the ECDHE shared secret, returning
    /// the (client, server) handshake-traffic secrets.
    pub fn handshake(&mut self, ecdhe: &[u8; 32]) -> ([u8; HASH_LEN], [u8; HASH_LEN]) {
        let derived = derive_secret(&self.secret, b"derived", &sha256_empty());
        self.secret = extract(&derived, ecdhe);
        let th = self.transcript_hash();
        let c = derive_secret(&self.secret, b"c hs traffic", &th);
        let s = derive_secret(&self.secret, b"s hs traffic", &th);
        (c, s)
    }

    /// Advance to the master secret, returning the (client, server) application
    /// traffic secrets (call after the full handshake transcript is in).
    pub fn application(&mut self) -> ([u8; HASH_LEN], [u8; HASH_LEN]) {
        let derived = derive_secret(&self.secret, b"derived", &sha256_empty());
        self.secret = extract(&derived, &[0u8; HASH_LEN]);
        let th = self.transcript_hash();
        let c = derive_secret(&self.secret, b"c ap traffic", &th);
        let s = derive_secret(&self.secret, b"s ap traffic", &th);
        (c, s)
    }

    /// Advance to the master secret only (no traffic secrets derived). Call after
    /// the handshake traffic secrets are derived, before deriving the per-direction
    /// application secrets (which use *different* transcript hashes — server app
    /// secret includes the server Finished, client app secret the client Finished).
    pub fn master(&mut self) {
        let derived = derive_secret(&self.secret, b"derived", &sha256_empty());
        self.secret = extract(&derived, &[0u8; HASH_LEN]);
    }

    /// Derive one application traffic secret from the master secret using an
    /// explicit transcript hash (`"c ap traffic"` / `"s ap traffic"`).
    pub fn app_secret(&self, label: &[u8], th: &[u8; HASH_LEN]) -> [u8; HASH_LEN] {
        derive_secret(&self.secret, label, th)
    }
}

fn sha256_empty() -> [u8; HASH_LEN] {
    Sha256::digest([]).into()
}

/// Verify-data (Finished): HMAC(finished_key, transcript_hash) — RFC 8446 §4.4.4.
pub fn finished_mac(finished_key: &[u8; HASH_LEN], transcript: &[u8; HASH_LEN]) -> [u8; HASH_LEN] {
    let mut m = HmacSha256::new_from_slice(finished_key).expect("hmac key");
    m.update(transcript);
    m.finalize().into_bytes().into()
}

// ---- record protection (RFC 8446 §5.2) ----

/// Per-record nonce: the write IV XOR the 64-bit sequence number, right-aligned.
fn record_nonce(iv: &[u8; IV_LEN], seq: u64) -> [u8; IV_LEN] {
    let mut n = *iv;
    let s = seq.to_be_bytes();
    for i in 0..8 {
        n[IV_LEN - 8 + i] ^= s[i];
    }
    n
}

/// Seal a record in place; returns the 16-byte auth tag. `aad` is the 5-byte TLS
/// record header (type=23, legacy_version, length).
pub fn record_seal(keys: &TrafficKeys, seq: u64, aad: &[u8], buf: &mut [u8]) -> [u8; 16] {
    aead_seal(&keys.key, &record_nonce(&keys.iv, seq), aad, buf)
}

/// Open a record in place; false on auth failure.
pub fn record_open(keys: &TrafficKeys, seq: u64, aad: &[u8], buf: &mut [u8], tag: &[u8; 16]) -> bool {
    aead_open(&keys.key, &record_nonce(&keys.iv, seq), aad, buf, tag)
}
