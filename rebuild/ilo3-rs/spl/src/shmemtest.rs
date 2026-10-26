//! Pre-recovery shared-memory self-test: is `0x40000000` (the CHIF host-window
//! base) backed by writable RAM in OUR firmware's memory map?
//!
//! HP's INTEGRITY allocated backed RAM in the `0x40000000` region; our DRAM is
//! decoded at `0x20000000` and static RE did not confirm a `0x40000000` alias
//! (docs/28 §10). If it is NOT backed, the whole host interface (CHIF + the SPL
//! recovery mailbox) is dead no matter how the BAR is programmed — so we check it
//! here, on the real hardware, before trusting any host channel.
//!
//! This is a BMC-side test: it reads/writes `0x40000000` directly (independent of
//! the host BAR register, which only affects host-side translation). The caller
//! reports PASS/FAIL on the UID LED (main.rs), so it is visible on the first boot
//! after a flash with no host channel involved.
//!
//! The probe defeats the two ways an unbacked aperture can fake a PASS:
//!   * a dead/zero aperture (readback 0 / all-ones) — caught by the 0 and ~0 rounds;
//!   * a single-value latch (one register echoing the last write) — caught by
//!     writing DIFFERENT values to two far-apart addresses and requiring both to
//!     read back their own value.
//!
//! WARNING: runs with the MMU off. If `0x40000000` is entirely unmapped (no bus
//! slave), the access may hang rather than fault — which shows up as the boot
//! stalling at the "DRAM up" LED code (3x), itself a usable signal.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

const SHMEM: u32 = 0x4000_0000;
const A0: u32 = SHMEM + 0x0000; // start of the window / recovery mailbox
const A1: u32 = SHMEM + 0x0FFC; // last word of the first 4 KB (far from A0)

#[inline]
fn drain_wb() {
    // Drain the write buffer so a store lands before the following load (caches
    // off, but the write buffer can still reorder store/load to MMIO-like space).
    unsafe { core::arch::asm!("mcr p15, 0, {z}, c7, c10, 4", z = in(reg) 0u32) };
}

#[inline]
unsafe fn wr(a: u32, v: u32) {
    write_volatile(a as *mut u32, v);
}
#[inline]
unsafe fn rd(a: u32) -> u32 {
    read_volatile(a as *const u32)
}

/// Returns true iff `0x40000000` behaves as writable, per-address RAM.
pub fn probe() -> bool {
    unsafe {
        // Distinct values to two far-apart addresses -> defeats a single latch.
        wr(A0, 0xA5A5_5A5A);
        wr(A1, 0x5A5A_A5A5);
        drain_wb();
        let distinct = rd(A0) == 0xA5A5_5A5A && rd(A1) == 0x5A5A_A5A5;

        // All-zero and all-ones round-trips at A0 -> defeats a stuck 0 / stuck ~0.
        wr(A0, 0x0000_0000);
        drain_wb();
        let zero = rd(A0) == 0x0000_0000;
        wr(A0, 0xFFFF_FFFF);
        drain_wb();
        let ones = rd(A0) == 0xFFFF_FFFF;

        // Leave the probed words clean for main's CHIF init.
        wr(A0, 0);
        wr(A1, 0);
        drain_wb();

        distinct && zero && ones
    }
}
