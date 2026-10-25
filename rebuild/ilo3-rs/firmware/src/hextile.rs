//! Hextile rectangle encoder (RFC 6143 §7.7.7) — Phase 7 KVM.
//!
//! Hextile is the cheapest real VNC encoding: a 16x16 tile encoded as a solid
//! background + run-length subrectangles of a foreground colour (or raw pixels
//! for noisy tiles). No zlib — ideal for a software-only ARMv5 encoder (docs/46).
//!
//! Clean-room from the public RFC; provenance: `docs/46-remote-console-rewrite.md`
//! (decision), RFC 6143 (spec). No HP code copied.

#![allow(dead_code)]

use alloc::vec::Vec;

/// Hextile tile edge length (pixels).
pub const TILE: usize = 16;
/// Pixels per tile (16x16 = 256).
pub const PIXELS: usize = TILE * TILE;

/// Subencoding mask bits (RFC 6143 §7.7.7).
pub mod mask {
    pub const RAW: u8 = 1;
    pub const BACKGROUND: u8 = 2;
    pub const FOREGROUND: u8 = 4;
    pub const ANY_SUBRECTS: u8 = 8;
    pub const SUBRECTS_COLOURED: u8 = 16;
}

/// Encode one 16x16 tile (256 u16 pixels, row-major) into `out`.
///
/// Strategy (cheap, console-oriented):
/// * one colour     → BackgroundSpecified only.
/// * two colours    → Background + Foreground + AnySubrects (row runs of the fg).
/// * three or more  → Raw (fallback; correct regardless).
pub fn encode_tile(pix: &[u16], out: &mut Vec<u8>) {
    debug_assert_eq!(pix.len(), PIXELS);

    // Dominant colour + tile kind.
    let bg = dominant(pix);

    match classify(pix, bg) {
        Kind::Solid => {
            // Solid tile: BackgroundSpecified only.
            out.push(mask::BACKGROUND);
            out.extend_from_slice(&bg.to_be_bytes());
        }
        Kind::Two(fg) => {
            // Two colours. Gather horizontal runs of `fg` per row; bail to Raw if
            // the run list would overflow the 1-byte count.
            let mut runs = [0u8; PIXELS]; // x,y,w,h per run (one per px worst case)
            let mut n = 0usize;
            for y in 0..TILE {
                let mut x = 0;
                while x < TILE {
                    if pix[y * TILE + x] != fg {
                        x += 1;
                        continue;
                    }
                    let start = x;
                    while x < TILE && pix[y * TILE + x] == fg {
                        x += 1;
                    }
                    let w = x - start;
                    if n * 4 + 4 <= runs.len() {
                        runs[n * 4] = start as u8;
                        runs[n * 4 + 1] = y as u8;
                        runs[n * 4 + 2] = w as u8;
                        runs[n * 4 + 3] = 1;
                        n += 1;
                    }
                }
            }

            if n > 253 {
                raw(pix, out);
            } else {
                out.push(mask::BACKGROUND | mask::FOREGROUND | mask::ANY_SUBRECTS);
                out.extend_from_slice(&bg.to_be_bytes());
                out.extend_from_slice(&fg.to_be_bytes());
                out.push(n as u8);
                for i in 0..n {
                    let (sx, sy, w, h) =
                        (runs[i * 4], runs[i * 4 + 1], runs[i * 4 + 2], runs[i * 4 + 3]);
                    out.push((sx & 0x0F) << 4 | (sy & 0x0F));
                    out.push(((w - 1) & 0x0F) << 4 | ((h - 1) & 0x0F));
                }
            }
        }
        Kind::Multi => raw(pix, out),
    }
}

fn raw(pix: &[u16], out: &mut Vec<u8>) {
    out.push(mask::RAW);
    for &p in pix {
        out.extend_from_slice(&p.to_be_bytes());
    }
}

/// Most frequent colour in the tile (ties → first encountered).
fn dominant(pix: &[u16]) -> u16 {
    let mut best = pix[0];
    let mut best_n = 0usize;
    for &c in pix {
        let mut n = 0usize;
        for &d in pix {
            if d == c {
                n += 1;
            }
        }
        if n > best_n {
            best_n = n;
            best = c;
        }
    }
    best
}

/// Tile colour classification: solid, exactly two colours, or more.
enum Kind {
    Solid,
    Two(u16),
    Multi,
}

fn classify(pix: &[u16], bg: u16) -> Kind {
    let mut other: Option<u16> = None;
    for &c in pix {
        if c == bg {
            continue;
        }
        match other {
            None => other = Some(c),
            Some(o) if o != c => return Kind::Multi,
            Some(_) => {}
        }
    }
    match other {
        None => Kind::Solid,
        Some(o) => Kind::Two(o),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_tile() {
        let pix = [0x07E0u16; PIXELS]; // green
        let mut out = Vec::new();
        encode_tile(&pix, &mut out);
        assert_eq!(out, vec![mask::BACKGROUND, 0x07, 0xE0]);
    }

    #[test]
    fn two_colour_tile() {
        let mut pix = [0x0000u16; PIXELS]; // black bg
        pix[0] = 0xFFFF; // one white fg pixel
        let mut out = Vec::new();
        encode_tile(&pix, &mut out);
        assert_eq!(out[0], mask::BACKGROUND | mask::FOREGROUND | mask::ANY_SUBRECTS);
        assert_eq!(&out[1..5], &[0x00, 0x00, 0xFF, 0xFF]); // bg=0x0000, fg=0xFFFF
        assert_eq!(out[5], 1); // one subrect
        assert_eq!(out[6], 0x00); // x=0,y=0
        assert_eq!(out[7], 0x00); // w=1,h=1
    }

    #[test]
    fn noisy_tile_falls_back_raw() {
        let mut pix = [0u16; PIXELS];
        for i in 0..PIXELS {
            pix[i] = i as u16; // all distinct (well, >2)
        }
        let mut out = Vec::new();
        encode_tile(&pix, &mut out);
        assert_eq!(out[0], mask::RAW);
        assert_eq!(out.len(), 1 + PIXELS * 2);
    }
}
