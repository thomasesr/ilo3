//! TLS 1.3 crypto primitives + power-on self-test (Phase 4, docs/32).
//!
//! Vetted RustCrypto impls (build clean no_std on ARMv5): ChaCha20-Poly1305
//! (AEAD), SHA-256, HKDF, and X25519 ECDHE — the TLS_CHACHA20_POLY1305_SHA256
//! suite (ChaCha beats AES in software with no AES instructions, docs/32). Keys
//! come from our software entropy pool (crate::rng::SystemRng).
//!
//! `selftest()` is a FIPS-style power-on self-test: known-answer tests for SHA-256
//! and X25519 (RFC vectors) + an AEAD round-trip with tamper detection. Run it at
//! boot before trusting the crypto; a FAIL means a miscompile/bug, not a config.

#![allow(dead_code)]

use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce, Tag};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{x25519, PublicKey, StaticSecret};

use crate::rng::SystemRng;

/// SHA-256 digest.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// HKDF-SHA256 extract+expand into `okm`.
pub fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], okm: &mut [u8]) -> bool {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    hk.expand(info, okm).is_ok()
}

/// AEAD seal in place: encrypt `buf` and return the 16-byte tag (detached).
pub fn aead_seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], buf: &mut [u8]) -> [u8; 16] {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(nonce), aad, buf)
        .expect("aead seal");
    tag.into()
}

/// AEAD open in place: verify `tag` + decrypt `buf`. False = auth failure.
pub fn aead_open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], buf: &mut [u8], tag: &[u8; 16]) -> bool {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt_in_place_detached(Nonce::from_slice(nonce), aad, buf, Tag::from_slice(tag))
        .is_ok()
}

/// Generate an ephemeral X25519 keypair from the system entropy pool.
pub fn x25519_keypair() -> (StaticSecret, [u8; 32]) {
    let secret = StaticSecret::random_from_rng(SystemRng);
    let public = PublicKey::from(&secret);
    (secret, *public.as_bytes())
}

/// X25519 shared secret from our secret + the peer's public key.
pub fn x25519_shared(secret: &StaticSecret, peer_public: &[u8; 32]) -> [u8; 32] {
    secret.diffie_hellman(&PublicKey::from(*peer_public)).to_bytes()
}

// ---- power-on self-test ----

/// SHA-256("abc") (FIPS 180-4).
const SHA_ABC: [u8; 32] = [
    0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
    0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
];
// X25519 RFC 7748 §5.2 first test vector.
const X_SCALAR: [u8; 32] = [
    0xa5, 0x46, 0xe3, 0x6b, 0xf0, 0x52, 0x7c, 0x9d, 0x3b, 0x16, 0x15, 0x4b, 0x82, 0x46, 0x5e, 0xdd,
    0x62, 0x14, 0x4c, 0x0a, 0xc1, 0xfc, 0x5a, 0x18, 0x50, 0x6a, 0x22, 0x44, 0xba, 0x44, 0x9a, 0xc4,
];
const X_U: [u8; 32] = [
    0xe6, 0xdb, 0x68, 0x67, 0x58, 0x30, 0x30, 0xdb, 0x35, 0x94, 0xc1, 0xa4, 0x24, 0xb1, 0x5f, 0x7c,
    0x72, 0x66, 0x24, 0xec, 0x26, 0xb3, 0x35, 0x3b, 0x10, 0xa9, 0x03, 0xa6, 0xd0, 0xab, 0x1c, 0x4c,
];
const X_OUT: [u8; 32] = [
    0xc3, 0xda, 0x55, 0x37, 0x9d, 0xe9, 0xc6, 0x90, 0x8e, 0x94, 0xea, 0x4d, 0xf2, 0x8d, 0x08, 0x4f,
    0x32, 0xec, 0xcf, 0x03, 0x49, 0x1c, 0x71, 0xf7, 0x54, 0xb4, 0x07, 0x55, 0x77, 0xa2, 0x85, 0x52,
];

/// Known-answer + round-trip self-test. Returns true iff every primitive passes.
pub fn selftest() -> bool {
    // SHA-256 KAT
    if sha256(b"abc") != SHA_ABC {
        return false;
    }
    // X25519 KAT (RFC 7748)
    if x25519(X_SCALAR, X_U) != X_OUT {
        return false;
    }
    // AEAD round-trip + tamper detection
    let key = [0x42u8; 32];
    let nonce = [0x24u8; 12];
    let aad = b"ilo3";
    let mut buf = *b"power-on self test payload";
    let tag = aead_seal(&key, &nonce, aad, &mut buf);
    let mut tampered = buf;
    tampered[0] ^= 1;
    if aead_open(&key, &nonce, aad, &mut tampered, &tag) {
        return false; // tamper must fail
    }
    if !aead_open(&key, &nonce, aad, &mut buf, &tag) || &buf != b"power-on self test payload" {
        return false;
    }
    true
}
