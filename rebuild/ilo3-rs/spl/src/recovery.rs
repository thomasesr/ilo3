//! SPL recovery window — host-pull RAM-boot over CHIF (feature `recovery`).
//!
//! Closes the one gap in host-reflash (docs/48): a payload that crashes *before*
//! the main CHIF debug task spawns leaves no channel, so the host can't re-flash
//! it -> back to the SPI programmer. This window runs in the SPL (which the host
//! can't brick: the SPL region is write-protected and only the programmer writes
//! it), AFTER DDR init and BEFORE jumping to main. If a host recovery tool is
//! present, it streams a known-good/new main image straight into DRAM and we jump
//! to it -- a VOLATILE RAM-boot. Persisting the fix is then the already-built main
//! `FWRITE` path (dbgchif). So: SPL recovery gets *any* working main running; that
//! main persists itself. Nothing here writes flash, so recovery itself can't brick.
//!
//! Transport is a dead-simple polling mailbox in the shared-mem window -- NO
//! doorbell and NO CCB ring, so it does not depend on the two CONFIRM-ON-HW CHIF
//! params. Host and SPL both poll; a (req_seq != resp_seq) handshake carries one
//! request at a time. Layout at window+0x8000 (host tool: ilo3-chif-console.py):
//!   +0x00 u32 magic "RCVR"   +0x04 req_seq   +0x08 cmd   +0x0C addr
//!   +0x10 len                +0x14 resp_seq  +0x18 status +0x40.. data
//!
//! The spin budget guarantees the loop terminates, so a normal boot (no host)
//! never hangs -- it just costs a few seconds polling, then boots flash normally.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

#[cfg(feature = "recovery")]
use crate::uart;

// Host-interface enable (mirror of firmware/src/chif.rs init).
const BAR_REG: usize = 0x01F0_2050;
const CTRL0: usize = 0x01F0_20D0;
const CTRL1: usize = 0x01F0_20D4;
const CTRL1_ENABLE: u32 = 0x0400_0000;
const SHMEM: usize = 0x4000_0000;
// Window size the SPL enumerates for BAR2. The size FIELD in 0x01F02050 is
// bit_length(size/4096) (HP encoding, docs/28 §10); 1 MB → field 9. Programming
// it here (before host PCI enumeration at POST) is what makes the host see the
// full window. (The old code wrote 20 → 2 GB → invalid → 4 KB default.)
const WINDOW_SIZE: u32 = 0x10_0000; // 1 MB

// Recovery mailbox at the START of the window (first 4 KB), NOT window+0x8000.
// Defense-in-depth: even if the BAR size encoding is somehow still wrong and the
// host only sees the default 4 KB BAR2, the whole mailbox (header 0x00..0x40 +
// data up to 0xF00) stays within that 4 KB and remains reachable. During SPL
// recovery main is not running, so overlapping main's CCB region (0x000..0x1000)
// is harmless — a successful recovery RAM-boots a fresh main anyway.
const REC: usize = SHMEM + 0x0000;
const REC_MAGIC: usize = REC + 0x00;
const REC_REQ: usize = REC + 0x04;
const REC_CMD: usize = REC + 0x08;
const REC_ADDR: usize = REC + 0x0C;
const REC_LEN: usize = REC + 0x10;
const REC_RESP: usize = REC + 0x14;
const REC_STATUS: usize = REC + 0x18;
const REC_DATA: usize = REC + 0x40;

const MAGIC: u32 = 0x5256_4352; // "RCVR" little-endian
const CMD_WRITE: u32 = 1; // copy len bytes from data -> addr (DRAM)
const CMD_BOOT: u32 = 2; // jump to addr
const CMD_PING: u32 = 3;
const MAX_CHUNK: u32 = 0xF00; // data area; keeps REC_DATA(0x40)+chunk < 0x1000

const DRAM_BASE: u32 = 0x2000_0000;
const DRAM_END: u32 = 0x2800_0000; // 128 MB (docs/13)

// Poll iterations before giving up and booting flash normally. Bounded so a
// normal, host-less boot can never hang — it just costs this long polling, then
// boots flash. Sized LONG on purpose: the only iLO reset available on the target
// is a full AC cycle (soft host-off leaves iLO on standby), after which the host
// takes MINUTES to POST + boot its OS before it can drive recovery. The SPL must
// still be waiting then. ~2e9 iters ≈ several minutes at 250 MHz (20e6 ≈ a few
// seconds). Any host activity (REC_MAGIC seen) resets it, so a live session never
// times out. Drop the `recovery` feature for production builds (spl/Cargo.toml).
const BUDGET: u32 = 2_000_000_000;

#[inline]
fn rd32(a: usize) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
#[inline]
fn wr32(a: usize, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) };
}

#[inline]
fn in_dram(addr: u32, len: u32) -> bool {
    addr >= DRAM_BASE && (addr as u64 + len as u64) <= DRAM_END as u64
}

/// Copy `len` bytes from the mailbox data area to a DRAM address (byte-wise;
/// chunks are small). Both ends are plain memory, so no flash/XIP hazard.
unsafe fn copy_in(dst: u32, len: u32) {
    let mut i = 0u32;
    while i < len {
        let b = read_volatile((REC_DATA + i as usize) as *const u8);
        write_volatile((dst + i) as *mut u8, b);
        i += 1;
    }
}

#[inline]
fn drain_wb() {
    // Drain the write buffer so DRAM writes land before the jump (caches off).
    unsafe { core::arch::asm!("mcr p15, 0, {z}, c7, c10, 4", z = in(reg) 0u32) };
}

/// Poll the recovery mailbox. Returns Some(entry) if the host requested a RAM
/// boot (image already streamed into DRAM), else None after the budget expires.
unsafe fn poll() -> Option<u32> {
    // Enable the host interface + program the window at 0x40000000. BAR value =
    // (base-0x40000000) | bit_length(size/4096) (HP encoding, docs/28 §10); for a
    // 1 MB window at the aperture base this is 0 | 9. This runs BEFORE host PCI
    // enumeration, so the host sees the correct BAR size at POST.
    let field = 32 - (WINDOW_SIZE >> 12).leading_zeros(); // 1 MB -> 9
    wr32(BAR_REG, (SHMEM as u32).wrapping_sub(0x4000_0000) | field);
    wr32(CTRL0, 0);
    wr32(CTRL1, CTRL1_ENABLE);

    // Hold the UID button at boot -> wait INDEFINITELY for the host (feature
    // uid-boot-recovery, default-off; the UID bit is CONFIRM-ON-HW — docs/49).
    // Otherwise the bounded BUDGET guarantees a host-less boot never hangs.
    let force = cfg!(feature = "uid-boot-recovery") && uid_held();

    let mut budget = BUDGET;
    loop {
        if rd32(REC_MAGIC) == MAGIC {
            let req = rd32(REC_REQ);
            if req != rd32(REC_RESP) {
                budget = BUDGET; // live host activity -> keep the window open
                let cmd = rd32(REC_CMD);
                let addr = rd32(REC_ADDR);
                let len = rd32(REC_LEN).min(MAX_CHUNK);
                let mut st: u32 = 1; // default: error
                match cmd {
                    CMD_PING => st = 0,
                    CMD_WRITE => {
                        if in_dram(addr, len) {
                            copy_in(addr, len);
                            st = 0;
                        }
                    }
                    CMD_BOOT => {
                        if in_dram(addr, 4) {
                            wr32(REC_STATUS, 0);
                            wr32(REC_RESP, req);
                            drain_wb();
                            return Some(addr);
                        }
                    }
                    _ => {}
                }
                wr32(REC_STATUS, st);
                wr32(REC_RESP, req); // ack
            }
        }
        if !force {
            budget -= 1;
            if budget == 0 {
                return None;
            }
        }
    }
}

/// True if the UID button reads pressed at boot. Register/bit + polarity are
/// CONFIRM-ON-HW and MUST match firmware/src/cpld.rs (+0x09 bit2, active-high).
/// Only consulted under the default-off `uid-boot-recovery` feature.
#[cfg(feature = "uid-boot-recovery")]
fn uid_held() -> bool {
    const UID_INPUT: usize = 0x01F1_4009; // CONFIRM-ON-HW
    const UID_BIT: u8 = 2; // CONFIRM-ON-HW
    unsafe { read_volatile(UID_INPUT as *const u8) & (1 << UID_BIT) != 0 }
}

#[cfg(not(feature = "uid-boot-recovery"))]
fn uid_held() -> bool {
    false
}

/// Boot entry from the SPL: offer the recovery window, then boot normally. Called
/// with the high stack already set (src/main.rs), since a RAM-boot image lands at
/// DRAM_BASE where the pre-DDR scratch stack sat. Never returns.
///
/// # Safety
/// Call once, after DDR init, with the high stack. Jumps to foreign code.
pub unsafe fn boot() -> ! {
    #[cfg(feature = "recovery")]
    {
        uart::puts("[spl] recovery: window open (host-pull RAM-boot)\n");
        if let Some(entry) = poll() {
            uart::puts("[spl] recovery: RAM-boot -> ");
            uart::puthex(entry);
            uart::puts("\n");
            core::arch::asm!("bx {e}", e = in(reg) entry, options(noreturn));
        }
        uart::puts("[spl] recovery: no host -> flash boot\n");
    }
    crate::flash::copy_and_jump()
}
