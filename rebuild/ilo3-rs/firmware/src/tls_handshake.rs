//! TLS 1.3 server handshake state machine (Phase 4, RFC 8446).
//!
//! Minimal-but-correct server side for the TLS_CHACHA20_POLY1305_SHA256 suite
//! (0x1303), X25519 ECDHE, Ed25519 CertificateVerify, with optional mutual TLS
//! (client-cert chain verified against trust anchors). Sits on top of
//! `tls13` (key schedule + record protection), `crypto` (primitives), and
//! `x509` (Ed25519 cert/chain verify).
//!
//! Transport-agnostic: the caller feeds it the raw ClientHello handshake
//! message and the client's encrypted flight, and receives the ServerHello
//! message (plaintext) and the encrypted server flight. Application-data
//! protect/unprotect is exposed once the handshake completes.

#![allow(dead_code)]

use alloc::vec::Vec;

use ed25519_dalek::{Signer, SigningKey};
use x25519_dalek::StaticSecret;

use crate::crypto;
use crate::tls13::{self, KeySchedule, TrafficKeys};
use crate::x509;

// Record content types (RFC 8446 §5.1).
const RT_HANDSHAKE: u8 = 22;
const RT_APPLICATION_DATA: u8 = 23;

// Handshake message types (RFC 8446 §4).
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_ENCRYPTED_EXTENSIONS: u8 = 8;
const HS_CERTIFICATE: u8 = 11;
const HS_CERTIFICATE_REQUEST: u8 = 13;
const HS_CERTIFICATE_VERIFY: u8 = 15;
const HS_FINISHED: u8 = 20;

const SUITE_CHACHA: [u8; 2] = [0x13, 0x03];
const LEGACY_VERSION: [u8; 2] = [0x03, 0x03];
const TLS13_VERSION: [u8; 2] = [0x03, 0x04];

// Extension / group / signature-scheme wire codes.
const EXT_SUPPORTED_VERSIONS: u16 = 0x002b;
const EXT_SIGNATURE_ALGORITHMS: u16 = 0x000d;
const EXT_KEY_SHARE: u16 = 0x0033;
const GROUP_X25519: u16 = 0x001d;
const SIG_ED25519: u16 = 0x0807;

fn u16be(b: [u8; 2]) -> u16 {
    u16::from_be_bytes(b)
}

/// Which client authentication message we are waiting for next.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RxPhase {
    Cert,
    Cv,
    Fin,
    Done,
}

/// A parsed ClientHello (fields we act on).
struct ClientHello {
    random: [u8; 32],
    session_id: Vec<u8>,
    key_share: [u8; 32], // client X25519 public key
}

/// Parse a ClientHello body (RFC 8446 §4.1.2); extracts the X25519 key_share.
fn parse_client_hello(body: &[u8]) -> Option<ClientHello> {
    let mut r = body;
    // legacy_version (2)
    if r.len() < 2 {
        return None;
    }
    r = &r[2..];
    // random (32)
    if r.len() < 32 {
        return None;
    }
    let random: [u8; 32] = r[..32].try_into().ok()?;
    r = &r[32..];
    // legacy_session_id
    let sid_len = *r.first()? as usize;
    r = &r[1..];
    if r.len() < sid_len {
        return None;
    }
    let session_id = r[..sid_len].to_vec();
    r = &r[sid_len..];
    // legacy_cipher_suites (2-byte length, list of 2-byte suites)
    if r.len() < 2 {
        return None;
    }
    let cs_len = u16be([r[0], r[1]]) as usize;
    r = &r[2..];
    if r.len() < cs_len {
        return None;
    }
    r = &r[cs_len..];
    // legacy_compression_methods (1-byte length)
    if r.len() < 1 {
        return None;
    }
    let cm_len = *r.first()? as usize;
    r = &r[1..];
    if r.len() < cm_len {
        return None;
    }
    r = &r[cm_len..];
    // extensions (2-byte length, then Extension list)
    if r.len() < 2 {
        return None;
    }
    let ext_total = u16be([r[0], r[1]]) as usize;
    r = &r[2..];
    if r.len() < ext_total {
        return None;
    }
    let mut key_share = None;
    let mut ext = &r[..ext_total];
    while !ext.is_empty() {
        if ext.len() < 4 {
            return None;
        }
        let etype = u16be([ext[0], ext[1]]);
        let elen = u16be([ext[2], ext[3]]) as usize;
        ext = &ext[4..];
        if ext.len() < elen {
            return None;
        }
        let (edata, erest) = ext.split_at(elen);
        ext = erest;
        if etype == EXT_KEY_SHARE {
            key_share = parse_key_share_client(edata);
        }
    }
    let key_share = key_share?;
    Some(ClientHello { random, session_id, key_share })
}

/// Parse the client's KeyShareClientHello extension data → first X25519 key.
fn parse_key_share_client(data: &[u8]) -> Option<[u8; 32]> {
    if data.len() < 2 {
        return None;
    }
    let list_len = u16be([data[0], data[1]]) as usize;
    let list = &data[2..];
    if list.len() < list_len {
        return None;
    }
    let mut r = &list[..list_len];
    while !r.is_empty() {
        if r.len() < 4 {
            return None;
        }
        let group = u16be([r[0], r[1]]);
        let klen = u16be([r[2], r[3]]) as usize;
        r = &r[4..];
        if r.len() < klen {
            return None;
        }
        let (key, rest) = r.split_at(klen);
        r = rest;
        if group == GROUP_X25519 && key.len() == 32 {
            return key.try_into().ok();
        }
    }
    None
}

/// Build a ServerHello body (RFC 8446 §4.1.3) echoing the client's session id.
fn build_server_hello(client: &ClientHello, eph_pub: &[u8; 32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(64);
    b.extend_from_slice(&LEGACY_VERSION);
    b.extend_from_slice(&client.random); // echo client random
    b.push(client.session_id.len() as u8);
    b.extend_from_slice(&client.session_id);
    b.extend_from_slice(&SUITE_CHACHA);
    b.push(0); // legacy_compression_method
    // extensions
    let mut ext = Vec::new();
    // supported_versions = 0x0304
    ext.extend_from_slice(&EXT_SUPPORTED_VERSIONS.to_be_bytes());
    ext.extend_from_slice(&2u16.to_be_bytes());
    ext.extend_from_slice(&TLS13_VERSION);
    // key_share = x25519(0x001d) + 32-byte key
    ext.extend_from_slice(&EXT_KEY_SHARE.to_be_bytes());
    let klen = 2 + 2 + 32; // group + keylen + key
    ext.extend_from_slice(&(klen as u16).to_be_bytes());
    ext.extend_from_slice(&GROUP_X25519.to_be_bytes());
    ext.extend_from_slice(&32u16.to_be_bytes());
    ext.extend_from_slice(eph_pub);
    b.extend_from_slice(&(ext.len() as u16).to_be_bytes());
    b.extend_from_slice(&ext);
    b
}

/// A full handshake message (type + 3-byte length + body).
fn handshake_msg(msg_type: u8, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    m.push(msg_type);
    m.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]); // 3 bytes
    m.extend_from_slice(body);
    m
}

/// The CertificateVerify signature input (RFC 8446 §4.4.3): 64 octets of
/// 0x20, then the raw context string, then a 0x00 separator, then the
/// transcript hash.
fn cv_input(role: &[u8], transcript: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(64 + 33 + 1 + 32);
    v.resize(64, b' '); // fixed 64-byte pad (not padded context string)
    v.extend_from_slice(b"TLS 1.3, ");
    v.extend_from_slice(role);
    v.extend_from_slice(b" CertificateVerify");
    v.push(0);
    v.extend_from_slice(transcript);
    v
}

/// Seal one encrypted handshake record (type 23, inner type = Handshake).
/// Appends the 0x16 inner content-type byte per RFC 8446 §5.4.
fn seal_hs_record(keys: &TrafficKeys, seq: &mut u64, hs_bytes: &[u8]) -> Vec<u8> {
    let mut plain = Vec::with_capacity(hs_bytes.len() + 1);
    plain.extend_from_slice(hs_bytes);
    plain.push(RT_HANDSHAKE); // inner content type
    let total = plain.len() + 16;
    let aad = [RT_APPLICATION_DATA, 0x03, 0x03, (total >> 8) as u8, total as u8];
    let mut out = Vec::with_capacity(5 + total);
    out.extend_from_slice(&aad);
    let start = out.len();
    out.extend_from_slice(&plain);
    let tag = tls13::record_seal(keys, *seq, &aad, &mut out[start..]);
    out.extend_from_slice(&tag);
    *seq += 1;
    out
}

/// Strip the trailing padding + inner content-type byte from decrypted record
/// plaintext, returning the inner content type (RFC 8446 §5.4 — the receiver
/// scans back from the end for the last non-zero octet).
fn strip_inner_type(plain: &mut Vec<u8>) -> Option<u8> {
    let mut split = plain.len();
    while split > 0 && plain[split - 1] == 0 {
        split -= 1;
    }
    if split == 0 {
        return None; // no content type octet
    }
    let inner_type = plain[split - 1];
    plain.truncate(split - 1);
    Some(inner_type)
}

/// Open one encrypted handshake record, returning the inner handshake bytes.
fn open_hs_record(keys: &TrafficKeys, seq: &mut u64, record: &[u8]) -> Option<Vec<u8>> {
    if record.len() < 5 + 16 || record[0] != RT_APPLICATION_DATA {
        return None;
    }
    let aad = &record[..5];
    let body = &record[5..];
    let (ctxt, tag) = body.split_at(body.len() - 16);
    let mut plain = ctxt.to_vec();
    let tag: [u8; 16] = tag.try_into().ok()?;
    if !tls13::record_open(keys, *seq, aad, &mut plain, &tag) {
        return None;
    }
    *seq += 1;
    if strip_inner_type(&mut plain)? != RT_HANDSHAKE {
        return None;
    }
    Some(plain)
}

/// The TLS 1.3 server handshake state machine.
pub struct ServerHandshake<'a> {
    ks: KeySchedule,
    eph_secret: StaticSecret,
    eph_pub: [u8; 32],
    server_cert: &'a [u8],
    signing_key: SigningKey,
    anchors: &'a [&'a [u8]],

    server_hs_keys: Option<TrafficKeys>,
    client_hs_keys: Option<TrafficKeys>,
    server_ap_keys: Option<TrafficKeys>,
    client_ap_keys: Option<TrafficKeys>,
    read_seq: u64,
    write_seq: u64,

    rx_phase: RxPhase, // which client auth message we are waiting for
    hs_rx: Vec<u8>,    // decrypted client handshake bytes awaiting parse
    hs_rx_pos: usize,  // consumed offset into hs_rx
    pending_chain: Vec<Vec<u8>>, // client cert chain (leaf first)
    pending_leaf: Vec<u8>,       // client leaf cert DER
}

/// The server's response to a ClientHello.
pub struct ServerHelloResponse {
    /// The plaintext ServerHello handshake message (send as a Handshake record).
    pub server_hello: Vec<u8>,
    /// The encrypted server flight (EE + CertificateRequest + Certificate +
    /// CertificateVerify + Finished) as one ApplicationData record.
    pub flight: Vec<u8>,
}

impl<'a> ServerHandshake<'a> {
    /// Build the server side. `eph_secret` is the ephemeral X25519 secret (the
    /// caller generates it from the entropy pool; passed in so this module stays
    /// pure + host-testable). `signing_seed` is the Ed25519 private seed for the
    /// server CertificateVerify. `anchors` are trust-anchor cert DERs for mTLS.
    pub fn new(
        server_cert: &'a [u8],
        signing_seed: &[u8; 32],
        anchors: &'a [&'a [u8]],
        eph_secret: StaticSecret,
    ) -> Self {
        let eph_pub = {
            let p = x25519_dalek::PublicKey::from(&eph_secret);
            *p.as_bytes()
        };
        ServerHandshake {
            ks: KeySchedule::new(),
            eph_secret,
            eph_pub,
            server_cert,
            signing_key: SigningKey::from_bytes(signing_seed),
            anchors,
            server_hs_keys: None,
            client_hs_keys: None,
            server_ap_keys: None,
            client_ap_keys: None,
            read_seq: 0,
            write_seq: 0,
            // Server-auth-only when no trust anchor is configured (browsers
            // don't present a client cert); mTLS kicks in when anchors exist.
            rx_phase: if anchors.is_empty() { RxPhase::Fin } else { RxPhase::Cert },
            hs_rx: Vec::new(),
            hs_rx_pos: 0,
            pending_chain: Vec::new(),
            pending_leaf: Vec::new(),
        }
    }

    /// Process the ClientHello handshake message (full, incl. 4-byte header) and
    /// produce the ServerHello + encrypted flight. Returns Err on parse/negotiate
    /// failure (no matching key_share / version).
    pub fn client_hello(&mut self, ch_msg: &[u8]) -> Result<ServerHelloResponse, ()> {
        if ch_msg.first() != Some(&HS_CLIENT_HELLO) {
            return Err(());
        }
        let body = ch_msg.get(4..).ok_or(())?;
        let client = parse_client_hello(body).ok_or(())?;

        // Feed ClientHello to the transcript (exact wire bytes).
        self.ks.update_transcript(ch_msg);

        // Build + feed ServerHello.
        let sh_body = build_server_hello(&client, &self.eph_pub);
        let sh_msg = handshake_msg(HS_SERVER_HELLO, &sh_body);
        self.ks.update_transcript(&sh_msg);

        // ECDHE shared secret → handshake traffic secrets.
        let shared = crypto::x25519_shared(&self.eph_secret, &client.key_share);
        let (c_hs, s_hs) = self.ks.handshake(&shared);
        self.client_hs_keys = Some(tls13::traffic_keys(&c_hs));
        self.server_hs_keys = Some(tls13::traffic_keys(&s_hs));
        // Advance to the master secret (app secrets derived later, per-direction).
        self.ks.master();

        // Build the encrypted flight.
        let flight = self.server_flight()?;

        Ok(ServerHelloResponse { server_hello: sh_msg, flight })
    }

    /// Build EE + CertificateRequest + Certificate + CertificateVerify + Finished,
    /// sealed as one encrypted handshake record. Derives the server application
    /// secret after the server Finished is in the transcript.
    fn server_flight(&mut self) -> Result<Vec<u8>, ()> {
        let hs_keys = *self.server_hs_keys.as_ref().ok_or(())?;
        let mut msgs = Vec::new();

        // EncryptedExtensions: empty extensions.
        let ee = handshake_msg(HS_ENCRYPTED_EXTENSIONS, &[0, 0]);
        self.ks.update_transcript(&ee);
        msgs.extend_from_slice(&ee);

        // CertificateRequest: only when we require client auth (mTLS).
        if !self.anchors.is_empty() {
            let cr = handshake_msg(HS_CERTIFICATE_REQUEST, &self.build_cert_request());
            self.ks.update_transcript(&cr);
            msgs.extend_from_slice(&cr);
        }

        // Certificate: our server cert (single entry, no extensions).
        let cert = handshake_msg(HS_CERTIFICATE, &self.build_certificate(&[self.server_cert]));
        self.ks.update_transcript(&cert);
        msgs.extend_from_slice(&cert);

        // CertificateVerify: sign context || transcript_hash.
        let th = self.ks.transcript_hash();
        let input = cv_input(b"server", &th);
        let sig = self.signing_key.sign(&input);
        let cv = handshake_msg(HS_CERTIFICATE_VERIFY, &self.build_cert_verify(&sig));
        self.ks.update_transcript(&cv);
        msgs.extend_from_slice(&cv);

        // Finished: HMAC(finished_key, transcript_hash).
        let th = self.ks.transcript_hash();
        let fin = handshake_msg(HS_FINISHED, &tls13::finished_mac(&hs_keys.finished_key, &th));
        self.ks.update_transcript(&fin);
        msgs.extend_from_slice(&fin);

        // Both application traffic secrets derive from the transcript through
        // the *server* Finished (RFC 8446 §7.1) — the client secret is NOT gated
        // on the client Finished (only resumption_master_secret is).
        let th = self.ks.transcript_hash();
        let c_ap = self.ks.app_secret(b"c ap traffic", &th);
        let s_ap = self.ks.app_secret(b"s ap traffic", &th);
        self.client_ap_keys = Some(tls13::traffic_keys(&c_ap));
        self.server_ap_keys = Some(tls13::traffic_keys(&s_ap));

        // Seal the handshake flight (write_seq still 0 for this handshake record).
        let flight = seal_hs_record(&hs_keys, &mut self.write_seq, &msgs);

        // Application-data records restart their sequence at 0 (per-key epoch).
        self.write_seq = 0;

        Ok(flight)
    }

    /// Process one client TLS record during the handshake. Returns `Ok(true)`
    /// when the client's flight is complete (Certificate + CertificateVerify +
    /// Finished verified + client application secret derived), `Ok(false)` when
    /// more records are needed, `Err(())` on any failure. The client flight may
    /// span multiple records, so call this once per received record.
    pub fn client_record(&mut self, record: &[u8]) -> Result<bool, ()> {
        if record.len() < 5 || record[0] != RT_APPLICATION_DATA {
            return Ok(false); // skip CCS / non-handshake records
        }
        let hs_keys = *self.client_hs_keys.as_ref().ok_or(())?;
        let plain = open_hs_record(&hs_keys, &mut self.read_seq, record).ok_or(())?;
        self.hs_rx.extend_from_slice(&plain);
        self.try_complete_client()
    }

    /// Attempt to parse + verify the client authentication messages from the
    /// accumulated decrypted handshake bytes.
    fn try_complete_client(&mut self) -> Result<bool, ()> {
        let hs_keys = *self.client_hs_keys.as_ref().ok_or(())?;
        loop {
            match self.rx_phase {
                RxPhase::Cert => {
                    let Some(cert_msg) = self.take_buffered(HS_CERTIFICATE) else { return Ok(false) };
                    let chain = self.parse_certificate(&cert_msg).ok_or(())?;
                    let leaf = chain.first().ok_or(())?.clone();
                    self.ks.update_transcript(&cert_msg);
                    self.pending_leaf = leaf;
                    self.pending_chain = chain;
                    self.rx_phase = RxPhase::Cv;
                }
                RxPhase::Cv => {
                    let Some(cv_msg) = self.take_buffered(HS_CERTIFICATE_VERIFY) else { return Ok(false) };
                    let cv_sig = self.parse_cert_verify(&cv_msg).ok_or(())?;
                    // Verify over the transcript *excluding* the CV message.
                    let leaf_cert = x509::parse_cert(&self.pending_leaf).ok_or(())?;
                    let input = cv_input(b"client", &self.ks.transcript_hash());
                    let vk = ed25519_dalek::VerifyingKey::from_bytes(
                        leaf_cert.spki.try_into().map_err(|_| ())?,
                    )
                    .map_err(|_| ())?;
                    let sig = ed25519_dalek::Signature::from_slice(&cv_sig).map_err(|_| ())?;
                    use ed25519_dalek::Verifier;
                    vk.verify(&input, &sig).map_err(|_| ())?;
                    self.ks.update_transcript(&cv_msg);
                    self.rx_phase = RxPhase::Fin;
                }
                RxPhase::Fin => {
                    let Some(fin_msg) = self.take_buffered(HS_FINISHED) else { return Ok(false) };
                    // Finished MAC is over the transcript *excluding* Finished.
                    let expected = tls13::finished_mac(&hs_keys.finished_key, &self.ks.transcript_hash());
                    if fin_msg.get(4..) != Some(&expected[..]) {
                        return Err(());
                    }
                    self.ks.update_transcript(&fin_msg);
                    // Client cert chain must chain to a trust anchor (mTLS only).
                    if !self.anchors.is_empty() {
                        let der_refs: Vec<&[u8]> =
                            self.pending_chain.iter().map(|c| c.as_slice()).collect();
                        // now_unix = None: no trusted wall-clock yet (RTC is a
                        // software clock seeded from NTP, docs/18). Validity is
                        // enforced once a time source is wired; the CA-flag +
                        // signature checks are always enforced.
                        if !x509::verify_chain(&der_refs, self.anchors, None) {
                            return Err(());
                        }
                    }
                    // Application-data records restart their sequence at 0 (the
                    // client application secret was already derived in server_flight).
                    self.read_seq = 0;
                    self.rx_phase = RxPhase::Done;
                    return Ok(true);
                }
                RxPhase::Done => return Ok(true),
            }
        }
    }

    /// Take the next buffered handshake message of `expected` type, if present.
    fn take_buffered(&mut self, expected: u8) -> Option<Vec<u8>> {
        let buf = &self.hs_rx[self.hs_rx_pos..];
        if buf.len() < 4 {
            return None;
        }
        let mtype = buf[0];
        let mlen = u32::from_be_bytes([0, buf[1], buf[2], buf[3]]) as usize;
        if mtype != expected || buf.len() < 4 + mlen {
            return None;
        }
        let msg = buf[..4 + mlen].to_vec();
        self.hs_rx_pos += 4 + mlen;
        Some(msg)
    }

    /// Encrypt application data (returns a type-23 record).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ()> {
        let keys = *self.server_ap_keys.as_ref().ok_or(())?;
        let mut plain = Vec::with_capacity(plaintext.len() + 1);
        plain.extend_from_slice(plaintext);
        plain.push(RT_APPLICATION_DATA); // inner content type
        let total = plain.len() + 16;
        let aad = [RT_APPLICATION_DATA, 0x03, 0x03, (total >> 8) as u8, total as u8];
        let mut out = Vec::with_capacity(5 + total);
        out.extend_from_slice(&aad);
        let start = out.len();
        out.extend_from_slice(&plain);
        let tag = tls13::record_seal(&keys, self.write_seq, &aad, &mut out[start..]);
        out.extend_from_slice(&tag);
        self.write_seq += 1;
        Ok(out)
    }

    /// Decrypt application data (returns the plaintext). None on auth failure.
    pub fn decrypt(&mut self, record: &[u8]) -> Option<Vec<u8>> {
        let keys = *self.client_ap_keys.as_ref()?;
        if record.len() < 5 + 16 || record[0] != RT_APPLICATION_DATA {
            return None;
        }
        let aad = &record[..5];
        let body = &record[5..];
        let (ctxt, tag) = body.split_at(body.len() - 16);
        let mut plain = ctxt.to_vec();
        let tag: [u8; 16] = tag.try_into().ok()?;
        if !tls13::record_open(&keys, self.read_seq, aad, &mut plain, &tag) {
            return None;
        }
        self.read_seq += 1;
        if strip_inner_type(&mut plain)? != RT_APPLICATION_DATA {
            return None;
        }
        Some(plain)
    }

    // ---- builders ----

    fn build_cert_request(&self) -> Vec<u8> {
        // CertificateRequest { context<1> (0), extensions<2> }
        // signature_algorithms extension data = SignatureSchemeList:
        //   list length (2) + schemes (2 each) = ed25519 (0x0807)
        let mut ext = Vec::new();
        ext.extend_from_slice(&EXT_SIGNATURE_ALGORITHMS.to_be_bytes());
        ext.extend_from_slice(&4u16.to_be_bytes()); // data length = 2 (list len) + 2 (scheme)
        ext.extend_from_slice(&2u16.to_be_bytes()); // one 2-byte scheme follows
        ext.extend_from_slice(&SIG_ED25519.to_be_bytes());
        let mut b = Vec::with_capacity(3 + ext.len());
        b.push(0); // certificate_request_context
        b.extend_from_slice(&(ext.len() as u16).to_be_bytes());
        b.extend_from_slice(&ext);
        b
    }

    fn build_certificate(&self, chain: &[&[u8]]) -> Vec<u8> {
        // Certificate { context<1>, certificate_list<3> { entry } }
        let mut entries = Vec::new();
        for der in chain {
            entries.extend_from_slice(&(der.len() as u32).to_be_bytes()[1..]); // 3 bytes
            entries.extend_from_slice(der);
            entries.extend_from_slice(&0u16.to_be_bytes()); // extensions length 0
        }
        let mut b = Vec::with_capacity(4 + entries.len());
        b.push(0); // certificate_request_context
        b.extend_from_slice(&(entries.len() as u32).to_be_bytes()[1..]); // 3 bytes
        b.extend_from_slice(&entries);
        b
    }

    fn build_cert_verify(&self, sig: &ed25519_dalek::Signature) -> Vec<u8> {
        // CertificateVerify { algorithm<2>, signature<2> }
        let mut b = Vec::with_capacity(4 + sig.to_bytes().len());
        b.extend_from_slice(&SIG_ED25519.to_be_bytes());
        b.extend_from_slice(&(sig.to_bytes().len() as u16).to_be_bytes());
        b.extend_from_slice(&sig.to_bytes());
        b
    }

    // ---- parsers ----

    fn parse_certificate(&self, msg: &[u8]) -> Option<Vec<Vec<u8>>> {
        let body = msg.get(4..)?;
        let mut r = body;
        let ctx_len = *r.first()? as usize;
        r = &r[1..];
        if r.len() < ctx_len {
            return None;
        }
        r = &r[ctx_len..];
        if r.len() < 3 {
            return None;
        }
        let list_len = u32::from_be_bytes([0, r[0], r[1], r[2]]) as usize;
        r = &r[3..];
        if r.len() < list_len {
            return None;
        }
        let mut chain = Vec::new();
        let mut list = &r[..list_len];
        while !list.is_empty() {
            if list.len() < 3 {
                return None;
            }
            let clen = u32::from_be_bytes([0, list[0], list[1], list[2]]) as usize;
            list = &list[3..];
            if list.len() < clen {
                return None;
            }
            chain.push(list[..clen].to_vec());
            list = &list[clen..];
            if list.len() < 2 {
                return None;
            }
            let elen = u16be([list[0], list[1]]) as usize;
            list = &list[2..];
            if list.len() < elen {
                return None;
            }
            list = &list[elen..]; // skip entry extensions
        }
        if chain.is_empty() {
            return None;
        }
        Some(chain)
    }

    fn parse_cert_verify(&self, msg: &[u8]) -> Option<Vec<u8>> {
        let body = msg.get(4..)?;
        if body.len() < 4 {
            return None;
        }
        // algorithm (2), signature length (2), signature
        if u16be([body[0], body[1]]) != SIG_ED25519 {
            return None;
        }
        let slen = u16be([body[2], body[3]]) as usize;
        if body.len() != 4 + slen {
            return None;
        }
        Some(body[4..].to_vec())
    }
}
