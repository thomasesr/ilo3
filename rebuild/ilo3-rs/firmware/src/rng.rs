//! Software entropy pool + ChaCha20 CSPRNG (Phase 4, docs/32).
//!
//! iLO 3 has NO hardware TRNG (docs/32) — the Rust rewrite must build its own
//! entropy. This is a FIRST-CLASS concern: weak randomness = broken TLS keys.
//!
//! Design: a sponge entropy pool (ChaCha permutation over a 16-word state) that
//! absorbs timing-jitter samples, feeding a reseedable ChaCha20 DRBG. Jitter
//! comes from the free-running counter (0xC0000098) sampled around a small
//! variable-timing workload, plus continuous `feed_timing()` from IRQ arrival.
//!
//! QUALITY CAVEAT (CONFIRM-ON-HW, NIST SP 800-90B): the counter is ~1 MHz
//! (placeholder rate) — coarse for per-instruction jitter — so entropy leans on
//! IRQ preemption + DRAM-refresh stalls accumulated over many samples. The pool
//! MUST be entropy-assessed on real hardware before its output is trusted for
//! key generation. Do NOT ship TLS keys from this until validated.

#![allow(dead_code)]

use core::cell::RefCell;
use core::ptr::addr_of_mut;

use critical_section::Mutex;
use rand_core::{CryptoRng, RngCore};

use crate::{chacha, timer};

const INIT_SAMPLES: usize = 4096; // jitter samples folded into the pool at init
const RESEED_BYTES: u64 = 1 << 16; // reseed the DRBG after this much output

/// Scratch touched by the jitter workload (memory-timing noise source).
static mut SCRATCH: [u32; 32] = [0; 32];

/// One timing-jitter sample: counter delta around a variable workload, X1'd with
/// the evolving memory state. Low bits carry preemption / cache / refresh noise.
fn jitter_sample() -> u32 {
    let t0 = timer::now_ticks() as u32;
    let mut x = t0 ^ 0x9E37_79B9;
    // SAFETY: single-threaded harvest; SCRATCH is private noise state.
    let s = unsafe { &mut *addr_of_mut!(SCRATCH) };
    for i in 0..64usize {
        let idx = (x as usize ^ i) & 31;
        x = x.wrapping_mul(1_664_525).wrapping_add(s[idx]).rotate_left(7);
        s[idx] ^= x;
    }
    let t1 = timer::now_ticks() as u32;
    t1.wrapping_sub(t0) ^ x
}

/// Sponge entropy pool: absorb samples into a 16-word state, permute to mix.
struct Pool {
    state: [u32; 16],
    pos: usize,
}

impl Pool {
    const fn new() -> Self {
        Pool { state: [0; 16], pos: 0 }
    }
    fn absorb(&mut self, sample: u32) {
        self.state[self.pos] ^= sample;
        self.pos += 1;
        if self.pos == 16 {
            chacha::permute(&mut self.state);
            self.pos = 0;
        }
    }
    /// Squeeze a 32-byte key + 3-word nonce out of the pool.
    fn squeeze(&mut self) -> ([u32; 8], [u32; 3]) {
        chacha::permute(&mut self.state);
        let mut key = [0u32; 8];
        key.copy_from_slice(&self.state[0..8]);
        let nonce = [self.state[8], self.state[9], self.state[10]];
        chacha::permute(&mut self.state); // ratchet so the squeeze isn't exposed
        (key, nonce)
    }
}

/// ChaCha20 DRBG seeded/reseeded from the pool.
struct Drbg {
    key: [u32; 8],
    nonce: [u32; 3],
    ctr: u32,
    out: [u8; 64],
    used: usize, // bytes of `out` already consumed (64 = empty)
    since_reseed: u64,
}

impl Drbg {
    const fn new() -> Self {
        Drbg { key: [0; 8], nonce: [0; 3], ctr: 0, out: [0; 64], used: 64, since_reseed: 0 }
    }
    fn reseed(&mut self, key: [u32; 8], nonce: [u32; 3]) {
        for i in 0..8 {
            self.key[i] ^= key[i];
        }
        for i in 0..3 {
            self.nonce[i] ^= nonce[i];
        }
        self.ctr = self.ctr.wrapping_add(1);
        self.used = 64;
        self.since_reseed = 0;
    }
    fn refill(&mut self) {
        chacha::block(&self.key, self.ctr, &self.nonce, &mut self.out);
        self.ctr = self.ctr.wrapping_add(1);
        self.used = 0;
    }
}

/// The combined entropy pool + DRBG.
pub struct Rng {
    pool: Pool,
    drbg: Drbg,
}

impl Rng {
    const fn new() -> Self {
        Rng { pool: Pool::new(), drbg: Drbg::new() }
    }
    fn seed(&mut self) {
        for _ in 0..INIT_SAMPLES {
            self.pool.absorb(jitter_sample());
        }
        let (k, n) = self.pool.squeeze();
        self.drbg.reseed(k, n);
    }
    fn fill(&mut self, dst: &mut [u8]) {
        for b in dst.iter_mut() {
            if self.drbg.used == 64 {
                if self.drbg.since_reseed >= RESEED_BYTES {
                    self.pool.absorb(jitter_sample());
                    let (k, nc) = self.pool.squeeze();
                    self.drbg.reseed(k, nc);
                }
                self.drbg.refill();
            }
            *b = self.drbg.out[self.drbg.used];
            self.drbg.used += 1;
            self.drbg.since_reseed += 1;
        }
    }
}

static RNG: Mutex<RefCell<Rng>> = Mutex::new(RefCell::new(Rng::new()));

/// Harvest the initial seed. Call once at boot, after the timer is up. Blocks
/// briefly while it collects jitter.
pub fn init() {
    critical_section::with(|cs| RNG.borrow(cs).borrow_mut().seed());
}

/// Add a timing-entropy sample (e.g. the counter value at IRQ arrival). Cheap,
/// non-blocking; call from interrupt handlers to keep reseeding the pool.
pub fn feed_timing(sample: u32) {
    critical_section::with(|cs| RNG.borrow(cs).borrow_mut().pool.absorb(sample));
}

/// Fill `dst` with CSPRNG output.
pub fn fill_bytes(dst: &mut [u8]) {
    critical_section::with(|cs| RNG.borrow(cs).borrow_mut().fill(dst));
}

/// rand_core handle routing to the global pool (for rustls / the rand ecosystem).
#[derive(Clone, Copy, Default)]
pub struct SystemRng;

impl RngCore for SystemRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }
    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        fill_bytes(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        fill_bytes(dest);
        Ok(())
    }
}

impl CryptoRng for SystemRng {}
