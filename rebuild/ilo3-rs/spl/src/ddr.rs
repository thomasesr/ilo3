//! Gromit-XE DDR3 bring-up — verbatim port of boot-ROM fn 0x7f198c (docs/30).
//!
//! Source: boot ROM init fn 0x7f198c + (reg,val) table 0x7f1a0c, CONFIRMED by
//! disassembly (capstone) — the PHASE1/PHASE2 arrays below are that table in its
//! exact ROM order and values. These are the FW-1.94 values for the G7's fitted
//! DDR3; reuse for the same part, recompute from the datasheet for different RAM.
//!
//! Controller block base 0xC0000000. Sequence (0x7f198c):
//!   1. warm check: 0xC0000020 bit7 set → DRAM already up, skip.
//!   2. power-stable delay: wait until timer 0xC0000098 counts >= 500 (0x1F4).
//!   3. phase 1: walk the table (timing/mode regs, ending with 0xC0000020 = 0x3).
//!   4. calibration wait: poll 0xC0000020 until bit7 (PHY cal / DLL lock) set.
//!   5. phase 2: 0xC0000020 = 0x2, then 0x80200040 = 0x400.
//!   6. MEMCFG: 0xC0000064 = strap(0x801F00A4) >> 1. DRAM usable.
//!
//! Clock: this operative path does NOT reprogram the clock/PLL. The boot ROM
//! only runs the strap->PLL decode (0x80200000+0x88) on the clock-mode==0 branch
//! (0xD1000000 bits[1:0] == 0); since the real DDR values live in the static
//! table above, the fitted board uses a non-zero clock mode and trains DDR on
//! the strap-default reset clock. clock_mode() below checks that at runtime.

use core::ptr::{read_volatile, write_volatile};

const MEMCTL: *mut u32 = 0xC000_0020 as *mut u32; // start/status: wr 0x3 start, rd bit7 ready, wr 0x2
const MEMCFG: *mut u32 = 0xC000_0064 as *mut u32; // final config, from strap >> 1
const STRAP: *const u32 = 0x801F_00A4 as *const u32; // MEMCFG source strap
const TIMER_LO: *const u32 = 0xC000_0098 as *const u32; // 64-bit timebase low word
const CLK_POST: *mut u32 = 0x8020_0040 as *mut u32; // clock block post-init write (0x400)
const CLKSEL: *const u32 = 0xD100_0000 as *const u32; // clock-select strap (bits[1:0] = mode)

const READY: u32 = 0x80; // MEMCTL bit7 = DDR ready / PHY calibrated
/// Calibration-poll budget in timer ticks (~500-tick power delay is the scale;
/// this is far larger so a healthy PHY always wins, a dead one gives up). HW rate
/// is strap-derived (~MHz), so this bounds the wait without hanging forever.
const CAL_BUDGET: u32 = 0x0100_0000;

/// Boot-ROM table 0x7f1a0c, phase 1 — exact ROM order. Lives in .rodata (flash
/// XIP), readable pre-DDR. Terminates with the MEMCTL=0x3 start kick.
const PHASE1: &[(u32, u32)] = &[
    (0xC000_0030, 0x0000_1520),
    (0xC000_0034, 0x0000_0005),
    (0xC000_0038, 0x0000_0000),
    (0xC000_003C, 0x0000_0000),
    (0xC000_0028, 0x552C_9180),
    (0xC000_002C, 0x0050_C34F),
    (0xC000_0040, 0xAA96_58E3),
    (0xC000_0048, 0x0000_007D),
    (0xC000_0024, 0x0000_0022),
    (0xC000_0018, 0xFDF0_FFF0),
    (0xC000_0020, 0x0000_0003), // start
];

/// Outcome of DDR bring-up.
#[derive(PartialEq, Eq)]
pub enum Status {
    Warm,       // DRAM already up (warm boot)
    Cold,       // cold init completed, PHY calibrated
    CalTimeout, // PHY calibration never signalled ready — DRAM unusable
}

/// Clock-select mode = 0xD1000000 bits[1:0]. 0 = the strap->PLL decode path the
/// SPL does not implement; non-zero = strap-default clock (this port's path).
pub fn clock_mode() -> u32 {
    // SAFETY: fixed, valid MMIO read.
    unsafe { read_volatile(CLKSEL) & 0x3 }
}

/// Bring up DDR3. Uses only registers + locals — safe to run XIP before DRAM.
pub fn init() -> Status {
    // SAFETY: all addresses are fixed, valid Gromit-XE MMIO; single-threaded at
    // boot with interrupts masked.
    unsafe {
        // 1. Warm-boot check.
        if read_volatile(MEMCTL as *const u32) & READY != 0 {
            return Status::Warm;
        }

        // 2. Power-stable delay: spin until the free-running timer passes 500.
        while read_volatile(TIMER_LO) < 0x1F4 {}

        // 3. Phase 1: load timing/mode regs; the final entry kicks the controller.
        for &(reg, val) in PHASE1 {
            write_volatile(reg as *mut u32, val);
        }

        // 4. Calibration wait — bounded so a dead PHY gives up instead of hanging.
        let start = read_volatile(TIMER_LO);
        while read_volatile(MEMCTL as *const u32) & READY == 0 {
            if read_volatile(TIMER_LO).wrapping_sub(start) > CAL_BUDGET {
                return Status::CalTimeout;
            }
        }

        // 5. Phase 2: post-calibration writes.
        write_volatile(MEMCTL, 0x2);
        write_volatile(CLK_POST, 0x400);

        // 6. Final MEMCFG from the board strap (geometry / data-rate).
        write_volatile(MEMCFG, read_volatile(STRAP) >> 1);
    }
    Status::Cold
}
