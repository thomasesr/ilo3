//! Web authentication: session store + login + per-request auth (Phase 5, docs/38).
//!
//! Session model: POST /json/login with HTTP Basic credentials → one Argon2
//! verify → a random 256-bit bearer token. Later requests carry
//! `Authorization: Bearer <token>`; a fast session lookup yields the privileges,
//! so the slow hash runs once per login, not per request.
//!
//! Argon2 is slow (seconds) and must NEVER run inside a critical section (that
//! masks interrupts): all hashing happens OUTSIDE the lock, with only tiny
//! lookups/inserts under `critical_section::with` (see login/init).

#![allow(dead_code)]

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use critical_section::Mutex;
use picoserve::extract::FromRequestParts;
use picoserve::request::RequestParts;
use picoserve::response::StatusCode;

use crate::uart::Uart;
use crate::users::{self, Privileges, UserDb};
use crate::{rng, timer};

const SESSION_TTL_TICKS: u64 = 30 * 60 * 1_000_000; // 30 min @ 1 MHz (placeholder rate)
const MAX_SESSIONS: usize = 16;

struct Session {
    token: [u8; 32],
    privs: Privileges,
    created: u64,
}

struct Store {
    v: Vec<Session>,
}

impl Store {
    const fn new() -> Self {
        Store { v: Vec::new() }
    }
    /// Insert a pre-generated token (bytes generated OUTSIDE this lock).
    fn insert(&mut self, token: [u8; 32], privs: Privileges) {
        if self.v.len() >= MAX_SESSIONS {
            self.v.remove(0); // evict oldest
        }
        self.v.push(Session { token, privs, created: timer::now_ticks() });
    }
    fn lookup(&mut self, token: &[u8]) -> Option<Privileges> {
        let now = timer::now_ticks();
        self.v.retain(|s| now.wrapping_sub(s.created) < SESSION_TTL_TICKS);
        self.v.iter().find(|s| ct_eq(&s.token, token)).map(|s| s.privs)
    }
    fn remove(&mut self, token: &[u8]) {
        self.v.retain(|s| !ct_eq(&s.token, token));
    }
}

static USERS: Mutex<RefCell<UserDb>> = Mutex::new(RefCell::new(UserDb::new()));
static SESSIONS: Mutex<RefCell<Store>> = Mutex::new(RefCell::new(Store::new()));

/// Constant-time equality (token compare).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    d == 0
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        for nib in [b >> 4, b & 0xf] {
            s.push(if nib < 10 { (b'0' + nib) as char } else { (b'a' + nib - 10) as char });
        }
    }
    s
}

fn hex_decode_32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        let hi = (b[i * 2] as char).to_digit(16)?;
        let lo = (b[i * 2 + 1] as char).to_digit(16)?;
        *o = ((hi << 4) | lo) as u8;
    }
    Some(out)
}

fn b64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for &c in s {
        if c == b'=' {
            break;
        }
        let Some(v) = val(c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Create a default `Administrator` account with a random password (logged once)
/// if the user DB is empty. Hashing runs OUTSIDE the lock. Call once at boot.
pub fn init() {
    let empty = critical_section::with(|cs| USERS.borrow_ref_mut(cs).is_empty());
    if !empty {
        return;
    }
    // random 12-char password from the entropy pool (unambiguous alphabet)
    const CH: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut raw = [0u8; 12];
    rng::fill_bytes(&mut raw);
    let pw: String = raw.iter().map(|x| CH[(x & 31) as usize] as char).collect();
    let Some(phc) = users::hash_password(pw.as_bytes()) else { return };
    critical_section::with(|cs| {
        let _ = USERS
            .borrow_ref_mut(cs)
            .add_hashed("Administrator", "Administrator", phc, Privileges::all());
    });
    let con = Uart::console();
    con.write_bytes(b"[web] default account 'Administrator' password: ");
    con.write_bytes(pw.as_bytes());
    con.write_bytes(b"\n");
}

// ---- extractors ----

/// Authenticated request: carries the caller's privileges. Rejects (401) when no
/// valid `Authorization: Bearer <token>` is present.
pub struct Auth {
    pub privs: Privileges,
    token: [u8; 32],
}

impl<'r, S> FromRequestParts<'r, S> for Auth {
    type Rejection = StatusCode;
    async fn from_request_parts(_state: &'r S, parts: &RequestParts<'r>) -> Result<Self, StatusCode> {
        let hdr = parts.headers().get("authorization").ok_or(StatusCode::UNAUTHORIZED)?;
        let val = hdr.as_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        let tok = val.strip_prefix("Bearer ").ok_or(StatusCode::UNAUTHORIZED)?;
        let bytes = hex_decode_32(tok.trim()).ok_or(StatusCode::UNAUTHORIZED)?;
        let privs =
            critical_section::with(|cs| SESSIONS.borrow_ref_mut(cs).lookup(&bytes)).ok_or(StatusCode::UNAUTHORIZED)?;
        Ok(Auth { privs, token: bytes })
    }
}

impl Auth {
    /// Require a privilege bit; `Err(403)` if absent.
    pub fn require(&self, p: Privileges) -> Result<(), StatusCode> {
        if self.privs.contains(p) {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// HTTP Basic credentials, for the login endpoint only.
pub struct BasicCreds {
    pub user: String,
    pub pass: String,
}

impl<'r, S> FromRequestParts<'r, S> for BasicCreds {
    type Rejection = StatusCode;
    async fn from_request_parts(_state: &'r S, parts: &RequestParts<'r>) -> Result<Self, StatusCode> {
        let hdr = parts.headers().get("authorization").ok_or(StatusCode::UNAUTHORIZED)?;
        let val = hdr.as_str().map_err(|_| StatusCode::UNAUTHORIZED)?;
        let b64 = val.strip_prefix("Basic ").ok_or(StatusCode::UNAUTHORIZED)?;
        let raw = b64_decode(b64.trim().as_bytes()).ok_or(StatusCode::UNAUTHORIZED)?;
        let s = core::str::from_utf8(&raw).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let (user, pass) = s.split_once(':').ok_or(StatusCode::UNAUTHORIZED)?;
        Ok(BasicCreds { user: user.into(), pass: pass.into() })
    }
}

// ---- handlers ----

/// Authenticate a username/password against the account DB, returning the
/// privileges on success. The slow Argon2 verify runs OUTSIDE any lock; only the
/// hash fetch + failure/success bookkeeping are under tiny critical sections.
/// Shared by the web login and RIBCL (each RIBCL request carries a LOGIN).
pub fn authenticate(user: &str, pass: &str) -> Option<Privileges> {
    let phc = critical_section::with(|cs| USERS.borrow_ref_mut(cs).phc_for_login(user))?;
    if !users::verify_password(pass.as_bytes(), &phc) {
        critical_section::with(|cs| USERS.borrow_ref_mut(cs).note_failure(user));
        crate::eventlog::raise(
            crate::eventlog::LogKind::Ilo,
            crate::eventlog::Severity::Caution,
            crate::eventlog::class::SECURITY,
            crate::eventlog::code::LOGIN_FAIL,
            "failed login attempt",
            Some(user),
        );
        return None;
    }
    critical_section::with(|cs| USERS.borrow_ref_mut(cs).note_success(user))
}

/// POST /json/login — Basic creds in, bearer token out. One Argon2 verify.
pub async fn login(creds: BasicCreds) -> Result<String, StatusCode> {
    let privs = authenticate(&creds.user, &creds.pass).ok_or(StatusCode::UNAUTHORIZED)?;
    // token bytes generated OUTSIDE the SESSIONS lock
    let mut token = [0u8; 32];
    rng::fill_bytes(&mut token);
    critical_section::with(|cs| SESSIONS.borrow_ref_mut(cs).insert(token, privs));
    Ok(format!("{{\"token\":\"{}\"}}\n", hex(&token)))
}

/// POST /json/logout — invalidate the bearer token.
pub async fn logout(auth: Auth) -> &'static str {
    critical_section::with(|cs| SESSIONS.borrow_ref_mut(cs).remove(&auth.token));
    "{\"ok\":true}\n"
}
