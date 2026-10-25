//! User/account database + privilege model + password hashing (Phase 5, docs/38).
//!
//! Up to 12 local accounts, each with an Argon2id password hash (PHC string), a
//! privilege bitmap, and an optional SSH public key. Login tracks consecutive
//! failures for lockout/delay. Every management surface (web/CLI/IPMI/SNMP)
//! authenticates + authorizes through this; per-config-key privilege lives here.
//!
//! Password hashing is Argon2id tuned DOWN to 4 MB / 3 passes (the 19 MB default
//! would exceed the 14.8 MB heap). On 250 MHz ARMv5 a hash is slow (seconds) and
//! runs synchronously — acceptable for rare logins; serialize it (one at a time)
//! so concurrent hashes can't exhaust the heap. Tune on hardware.

#![allow(dead_code)]

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use bitflags::bitflags;

use crate::rng::SystemRng;

pub const MAX_USERS: usize = 12;
/// Consecutive failed logins before the account is locked.
pub const LOCKOUT_THRESHOLD: u32 = 5;

bitflags! {
    /// The iLO privilege box (docs/38). Core 6 + extended role bits.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Privileges: u32 {
        const LOGIN          = 1 << 0;  // log in to iLO
        const REMOTE_CONSOLE = 1 << 1;  // remote console (KVM)
        const VIRTUAL_MEDIA  = 1 << 2;  // virtual media
        const RESET_SERVER   = 1 << 3;  // virtual power + reset
        const CONFIG_ILO     = 1 << 4;  // configure iLO settings
        const ADMIN          = 1 << 5;  // administer user accounts
        // extended / role
        const VIEW_LOGS      = 1 << 6;
        const CLEAR_LOGS     = 1 << 7;
        const DIAG           = 1 << 8;
        const EMS            = 1 << 9;
        const CONFIG_RACK    = 1 << 10;
        const DIR_GRPACCT    = 1 << 11;
        const UPDATE_ILO     = 1 << 12;
    }
}

/// Argon2id tuned for this platform (see module docs).
fn argon2id() -> Argon2<'static> {
    let params = Params::new(4096, 3, 1, None).expect("argon2 params");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash a password to an Argon2id PHC string (salt from the entropy pool).
pub fn hash_password(pw: &[u8]) -> Option<String> {
    let salt = SaltString::generate(SystemRng);
    argon2id().hash_password(pw, &salt).ok().map(|h| h.to_string())
}

/// Verify a password against a stored PHC string (uses the hash's own params).
pub fn verify_password(pw: &[u8], phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else { return false };
    argon2id().verify_password(pw, &parsed).is_ok()
}

/// One local account.
pub struct Account {
    pub name: String,     // login name
    pub username: String, // display name
    phc: String,          // Argon2id PHC hash
    pub privileges: Privileges,
    pub ssh_pubkey: Option<Vec<u8>>,
    fails: u32, // consecutive login failures
}

impl Account {
    pub fn is_locked(&self) -> bool {
        self.fails >= LOCKOUT_THRESHOLD
    }
}

/// The local account database (≤ MAX_USERS).
pub struct UserDb {
    accounts: Vec<Account>,
}

impl UserDb {
    pub const fn new() -> Self {
        UserDb { accounts: Vec::new() }
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.accounts.iter().position(|a| a.name == name)
    }

    pub fn get(&self, name: &str) -> Option<&Account> {
        self.index_of(name).map(|i| &self.accounts[i])
    }

    /// Add an account. Err on a full DB or a duplicate name.
    pub fn add(&mut self, name: &str, username: &str, pw: &[u8], privileges: Privileges) -> Result<(), ()> {
        if self.accounts.len() >= MAX_USERS || self.index_of(name).is_some() {
            return Err(());
        }
        let phc = hash_password(pw).ok_or(())?;
        self.accounts.push(Account {
            name: name.to_string(),
            username: username.to_string(),
            phc,
            privileges,
            ssh_pubkey: None,
            fails: 0,
        });
        Ok(())
    }

    /// Remove an account by login name.
    pub fn remove(&mut self, name: &str) -> bool {
        match self.index_of(name) {
            Some(i) => {
                self.accounts.remove(i);
                true
            }
            None => false,
        }
    }

    /// Replace an account's password.
    pub fn set_password(&mut self, name: &str, pw: &[u8]) -> Result<(), ()> {
        let i = self.index_of(name).ok_or(())?;
        self.accounts[i].phc = hash_password(pw).ok_or(())?;
        Ok(())
    }

    /// Set/replace an account's SSH public key.
    pub fn set_ssh_key(&mut self, name: &str, key: Vec<u8>) -> Result<(), ()> {
        let i = self.index_of(name).ok_or(())?;
        self.accounts[i].ssh_pubkey = Some(key);
        Ok(())
    }

    /// Authenticate. Returns the account's privileges on success. Enforces
    /// lockout after `LOCKOUT_THRESHOLD` consecutive failures; a correct password
    /// on a locked account still fails until `reset_fails` is called.
    pub fn authenticate(&mut self, name: &str, pw: &[u8]) -> Option<Privileges> {
        let i = self.index_of(name)?;
        if self.accounts[i].is_locked() {
            return None;
        }
        if verify_password(pw, &self.accounts[i].phc) {
            self.accounts[i].fails = 0;
            Some(self.accounts[i].privileges)
        } else {
            self.accounts[i].fails = self.accounts[i].fails.saturating_add(1);
            None
        }
    }

    /// Clear the failure counter / unlock (admin action or timed unlock).
    pub fn reset_fails(&mut self, name: &str) {
        if let Some(i) = self.index_of(name) {
            self.accounts[i].fails = 0;
        }
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }
}
