//! Framebuffer + dirty-tile tracking (Phase 7 KVM seam).
//!
//! A 16bpp (RGB565) framebuffer with a per-tile dirty bitmap. The RFB task
//! (kvm.rs) polls dirty tiles and feeds them to the Hextile encoder. Two sources
//! plug in here: a software renderer now (text console / test pattern), and the
//! real DVC capture (`capture.rs`, docs/14) once brought up on hardware.
//!
//! Provenance: `docs/14-kvm-video.md` (buffer region / capture model), RFC 6143
//! (framebuffer update). No HP code copied.

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;

use crate::hextile;

/// A 16bpp RGB565 framebuffer with dirty-tile tracking.
pub struct Framebuffer {
    w: usize,
    h: usize,
    pixels: Vec<u16>,
    dirty: Vec<u8>, // 1 byte per tile: 0 = clean, 1 = dirty
}

impl Framebuffer {
    pub fn new(w: usize, h: usize) -> Self {
        let tiles_x = w.div_ceil(hextile::TILE);
        let tiles_y = h.div_ceil(hextile::TILE);
        Framebuffer {
            w,
            h,
            pixels: vec![0u16; w * h],
            dirty: vec![0u8; tiles_x * tiles_y],
        }
    }

    pub fn width(&self) -> usize {
        self.w
    }
    pub fn height(&self) -> usize {
        self.h
    }
    pub fn tiles_x(&self) -> usize {
        self.w.div_ceil(hextile::TILE)
    }
    pub fn tiles_y(&self) -> usize {
        self.h.div_ceil(hextile::TILE)
    }

    #[inline]
    pub fn set_pixel(&mut self, x: usize, y: usize, p: u16) {
        if x < self.w && y < self.h {
            self.pixels[y * self.w + x] = p;
        }
    }

    /// Fill a rectangle (clipped) and mark its tiles dirty.
    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, p: u16) {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        for yy in y..y1 {
            let row = yy * self.w;
            for xx in x..x1 {
                self.pixels[row + xx] = p;
            }
        }
        self.mark_region_dirty(x, y, w, h);
    }

    pub fn mark_all_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = 1;
        }
    }

    pub fn mark_region_dirty(&mut self, x: usize, y: usize, w: usize, h: usize) {
        let tiles_x = self.tiles_x();
        let (tx0, ty0) = (x / hextile::TILE, y / hextile::TILE);
        let (tx1, ty1) = (x.wrapping_add(w).div_ceil(hextile::TILE), y.wrapping_add(h).div_ceil(hextile::TILE));
        for ty in ty0..ty1.min(self.tiles_y()) {
            for tx in tx0..tx1.min(tiles_x) {
                self.dirty[ty * tiles_x + tx] = 1;
            }
        }
    }

    pub fn is_dirty(&self, tx: usize, ty: usize) -> bool {
        self.dirty[ty * self.tiles_x() + tx] != 0
    }

    /// Clear the whole dirty bitmap (call after a FramebufferUpdate sweep).
    pub fn clear_dirty(&mut self) {
        for d in &mut self.dirty {
            *d = 0;
        }
    }

    /// Clear the dirty flag for a single tile.
    pub fn clear_dirty_tile(&mut self, tx: usize, ty: usize) {
        let tiles_x = self.tiles_x();
        self.dirty[ty * tiles_x + tx] = 0;
    }

    /// Copy a 16x16 tile into `buf` (right/bottom tiles are edge-padded with the
    /// last valid pixel — keeps the encoder 256-px-aligned for non-multiple dims).
    pub fn copy_tile(&self, tx: usize, ty: usize, buf: &mut [u16; hextile::PIXELS]) {
        let x0 = tx * hextile::TILE;
        let y0 = ty * hextile::TILE;
        for py in 0..hextile::TILE {
            let y = (y0 + py).min(self.h - 1);
            let row = y * self.w;
            for px in 0..hextile::TILE {
                let x = (x0 + px).min(self.w - 1);
                buf[py * hextile::TILE + px] = self.pixels[row + x];
            }
        }
    }
}
