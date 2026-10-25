//! ChaCha20 block function — self-contained (Phase 4 crypto foundation).
//!
//! Used as the core of the CSPRNG (src/rng.rs). Hand-rolled to avoid pulling
//! ppv-lite86/SIMD crates that don't fit ARMv5; this is the plain portable
//! reference permutation (RFC 8439), constant-time in the sense that it has no
//! data-dependent branches or memory accesses.

/// "expand 32-byte k"
const CONSTS: [u32; 4] = [0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574];

#[inline(always)]
fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// Compute one 64-byte ChaCha20 block for (`key`, `counter`, `nonce`) into `out`.
pub fn block(key: &[u32; 8], counter: u32, nonce: &[u32; 3], out: &mut [u8; 64]) {
    let init = [
        CONSTS[0], CONSTS[1], CONSTS[2], CONSTS[3],
        key[0], key[1], key[2], key[3],
        key[4], key[5], key[6], key[7],
        counter, nonce[0], nonce[1], nonce[2],
    ];
    let mut s = init;
    for _ in 0..10 {
        // column rounds
        quarter_round(&mut s, 0, 4, 8, 12);
        quarter_round(&mut s, 1, 5, 9, 13);
        quarter_round(&mut s, 2, 6, 10, 14);
        quarter_round(&mut s, 3, 7, 11, 15);
        // diagonal rounds
        quarter_round(&mut s, 0, 5, 10, 15);
        quarter_round(&mut s, 1, 6, 11, 12);
        quarter_round(&mut s, 2, 7, 8, 13);
        quarter_round(&mut s, 3, 4, 9, 14);
    }
    for i in 0..16 {
        let w = s[i].wrapping_add(init[i]);
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
}

/// Permute a 16-word state in place (20 rounds, no feed-forward) — the mixing
/// primitive the entropy pool uses to absorb jitter samples (sponge-style).
pub fn permute(s: &mut [u32; 16]) {
    for _ in 0..10 {
        quarter_round(s, 0, 4, 8, 12);
        quarter_round(s, 1, 5, 9, 13);
        quarter_round(s, 2, 6, 10, 14);
        quarter_round(s, 3, 7, 11, 15);
        quarter_round(s, 0, 5, 10, 15);
        quarter_round(s, 1, 6, 11, 12);
        quarter_round(s, 2, 7, 8, 13);
        quarter_round(s, 3, 4, 9, 14);
    }
}
