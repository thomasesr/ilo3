//! DVC video capture HAL — Phase 7 KVM (docs/14).
//!
//! Two ASIC register blocks drive host-video capture:
//! * the **host-video source / snoop** `0x01F06000` (digitizes the host VGA,
//!   exposes mode/timing, feeds the capture engine);
//! * the **DVC capture control** `0x01F100C0` (+status regs), which DMAs snooped
//!   frames into the `0xB0000000`–`0xB3FFFFFF` capture-buffer region.
//!
//! **Register semantics are PARTIAL** (docs/14): block bases + the status path are
//! confirmed; per-index field meaning (`+0x28`/`+0x2A` indexed port) needs hardware
//! correlation. Every field here is CONFIRM-ON-HW — this is a bring-up scaffold,
//! not a validated datapath. Provenance: `docs/14-kvm-video.md`. No HP code copied.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

/// Host-video source / snoop block (docs/14 pass 3).
pub const VIDEO_SRC: usize = 0x01F0_6000;
/// DVC capture control block.
pub const DVC_CTRL: usize = 0x01F1_00C0;
/// Capture status/frame register.
pub const DVC_STATUS: usize = 0x01F1_0088;
/// Capture status/control (read) registers.
pub const DVC_STATUS2: usize = 0x01F1_0070;
pub const DVC_STATUS3: usize = 0x01F1_0098;

/// Capture-buffer region (framebuffers DMA'd here; docs/14).
pub const BUF_BASE: usize = 0xB000_0000;
pub const BUF_SIZE: usize = 0x0400_0000; // 64 MB

// Host-video source block offsets (16-bit regs; docs/14).
const VSRC_CTRL: usize = 0x00; // control/status (r/w)
const VSRC_MODE: usize = 0x02; // mode/command config (w)
const VSRC_TIMING: usize = 0x04; // timing config (r/w)
const VSRC_MODE_STATUS: usize = 0x20; // sync/mode status (r)
const VSRC_IDX: usize = 0x28; // indexed data port (r/w, 16/32b)
const VSRC_DATA: usize = 0x2C; // data / counter (r, 32b)

/// DVC control struct +0x4C poll target: capture engine ready when ≤ 0xF.
const DVC_READY_MAX: u32 = 0xF;

#[inline]
fn src16(off: usize) -> *mut u16 {
    (VIDEO_SRC + off) as *mut u16
}
#[inline]
fn dvc32(off: usize) -> *mut u32 {
    off as *mut u32
}

/// Read a 16-bit register in the host-video source block.
pub fn vsrc_read(off: usize) -> u16 {
    // SAFETY: fixed MMIO; identity-mapped (docs/10).
    unsafe { read_volatile(src16(off)) }
}

/// Write a 16-bit register in the host-video source block.
pub fn vsrc_write(off: usize, v: u16) {
    // SAFETY: fixed MMIO; identity-mapped.
    unsafe { write_volatile(src16(off), v) };
}

/// Read a 32-bit register at an absolute MMIO address.
fn mmio32(addr: usize) -> u32 {
    // SAFETY: fixed MMIO; identity-mapped.
    unsafe { read_volatile(dvc32(addr)) }
}

/// Arm the capture engine: kick the DVC control block (docs/14: dvc_main writes
/// the control struct +0x4c then polls it until the ASIC capture state ≤ 0xF).
/// The exact arm sequence is CONFIRM-ON-HW.
pub fn arm() {
    // Placeholder: the original writes `1` to the capture control struct +0x4c.
    // Field/bit encoding of DVC_CTRL is not statically recoverable (docs/14).
    vsrc_write(VSRC_CTRL, 1);
}

/// Poll the capture engine until ready (state ≤ 0xF) or `max` iterations.
pub fn poll_ready(max: u32) -> bool {
    for _ in 0..max {
        if mmio32(DVC_STATUS) & 0xFFFF <= DVC_READY_MAX {
            return true;
        }
    }
    false
}

/// Detected host-video mode/sync status (docs/14: read at +0x20).
pub fn mode_status() -> u16 {
    vsrc_read(VSRC_MODE_STATUS)
}

/// Capture-buffer base physical address (primary buffer 0 in the 64 MB window).
pub fn buffer_base() -> usize {
    BUF_BASE
}
