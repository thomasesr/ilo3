//! ARM926EJ-S (ARMv5) MMU bring-up — identity map + high-vector alias.
//!
//! Why this exists: the CPU exception base is 0x00000000 (CP15 V=0) or
//! 0xFFFF0000 (V=1), never our DDR at 0x20000000, so with the MMU off there is
//! no writable RAM to hold the vector table and interrupts cannot be taken.
//! This brings up the one-level (section) page table, turns the MMU on, and
//! aliases the high-vector section to a dedicated DDR MB so the vectors become
//! installable (see vectors.rs) and live IRQ delivery is unblocked.
//!
//! Translation model (ARMv5 short-descriptor, 1 MB sections only):
//!   * identity-map all 4 GB (docs/10: the SoC's device regions are VA=PA) —
//!     MMIO sections strongly-ordered (C=0,B=0), DDR sections write-back cached;
//!   * L1[0] = fault, so a null-pointer access aborts instead of aliasing PA 0;
//!   * L1[0xFFF] (VA 0xFFF00000) -> `_vectors_phys` (reserved top DDR MB, see
//!     link.x), device memory, so the V=1 vector base 0xFFFF0000 is writable.
//!   DACR domain 0 = manager, so the AP bits are not enforced during bring-up.
//!
//! All CP15 ops below are ARM926-exact (TRM): there is no VBAR/ISB/TTBCR on
//! ARMv5 — vector base is the SCTLR V bit, barriers are CP15 c7 operations.

use core::ptr::addr_of_mut;

extern "C" {
    static _vectors_phys: u8; // reserved high-vector alias MB (link.x), 1 MB aligned
}

/// The CPU exception base once the MMU is on (SCTLR V=1, high vectors).
pub const VECTOR_BASE: usize = 0xFFFF_0000;

// ---- ARMv5 L1 section descriptor fields ----
const SECTION: u32 = 0b10; // bits[1:0] = 1 MB section
const SBO: u32 = 1 << 4; // bit[4] should-be-one on ARMv5
const AP_RW: u32 = 0b11 << 10; // bits[11:10] full access (moot under manager domain)
const CACHED: u32 = (1 << 3) | (1 << 2); // C | B = write-back, write-allocate
const DDR_SECTIONS: core::ops::Range<usize> = 0x200..0x210; // 0x20000000..0x21000000 (16 MB)

/// 16 KB-aligned level-1 translation table: 4096 one-MB section descriptors.
#[repr(C, align(16384))]
struct L1Table([u32; 4096]);
static mut L1: L1Table = L1Table([0; 4096]);

#[inline]
fn device(pa: u32) -> u32 {
    (pa & 0xFFF0_0000) | AP_RW | SBO | SECTION // C=0,B=0 strongly ordered (MMIO-safe)
}
#[inline]
fn cached(pa: u32) -> u32 {
    device(pa) | CACHED
}

/// Fill the L1 table, program CP15, and enable the MMU + I/D caches with high
/// vectors. Call once at boot, after .bss is zeroed and before unmasking IRQs.
pub fn init() {
    let table = addr_of_mut!(L1) as *mut u32;
    let valias = unsafe { &_vectors_phys as *const u8 as u32 }; // 0x20F00000

    // Build the table. D-cache is still off here (see _start), so these plain
    // writes land directly in DDR — coherent for the hardware table walker.
    for i in 0..4096usize {
        // SAFETY: `i` is in-bounds of the 4096-entry table.
        unsafe { table.add(i).write_volatile(device((i as u32) << 20)) };
    }
    for i in DDR_SECTIONS {
        unsafe { table.add(i).write_volatile(cached((i as u32) << 20)) };
    }
    // L1[0] = 0 (fault) → null-deref aborts. Reserved alias MB stays device
    // under its identity VA too, so the only cached view of DDR is the image's.
    unsafe {
        table.add(0).write_volatile(0);
        table.add(0x20F).write_volatile(device(valias)); // identity view = device
        table.add(0xFFF).write_volatile(device(valias)); // VA 0xFFF00000 → alias
    }

    // SAFETY: table is a valid 16 KB-aligned L1; we enable the MMU exactly once.
    unsafe { enable_mmu(table as u32) };
}

/// Program TTBR0/DACR, flush, and switch the MMU + caches + high vectors on.
///
/// # Safety
/// `ttbr` must point to a valid, 16 KB-aligned ARMv5 L1 table covering every VA
/// the CPU will touch (code, stacks, MMIO, vector base). Call once.
#[inline(never)]
unsafe fn enable_mmu(ttbr: u32) {
    core::arch::asm!(
        "mcr p15, 0, {ttbr}, c2, c0, 0",   // TTBR0 = L1 base
        "mcr p15, 0, {mgr}, c3, c0, 0",    // DACR: domain 0 = manager (0b11)
        "mcr p15, 0, {z}, c8, c7, 0",      // invalidate I+D TLB
        "mcr p15, 0, {z}, c7, c7, 0",      // invalidate I+D caches
        "mcr p15, 0, {z}, c7, c10, 4",     // drain write buffer
        "mrc p15, 0, {t}, c1, c0, 0",      // read SCTLR
        "orr {t}, {t}, #0x1",              // M  = MMU on
        "orr {t}, {t}, #0x4",              // C  = D-cache on
        "orr {t}, {t}, #0x1000",           // I  = I-cache on
        "orr {t}, {t}, #0x2000",           // V  = high vectors (0xFFFF0000)
        "mcr p15, 0, {t}, c1, c0, 0",      // write SCTLR (MMU live from here)
        "mcr p15, 0, {z}, c7, c5, 4",      // prefetch flush (ARMv5 ISB)
        ttbr = in(reg) ttbr,
        mgr  = in(reg) 0x3u32,
        z    = in(reg) 0u32,
        t    = out(reg) _,
        options(nostack, preserves_flags),
    );
}

/// Make freshly-written code at `addr..addr+len` visible to instruction fetch:
/// drain the write buffer, invalidate the I-cache, and flush the prefetch. Use
/// after copying the vector table to VECTOR_BASE (the alias MB is uncached, so
/// no D-cache clean is needed — the write buffer drain suffices).
pub fn sync_icache() {
    // SAFETY: CP15 cache-maintenance ops, no memory operands.
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {z}, c7, c10, 4", // drain write buffer
            "mcr p15, 0, {z}, c7, c5, 0",  // invalidate entire I-cache
            "mcr p15, 0, {z}, c7, c5, 4",  // prefetch flush
            z = in(reg) 0u32,
            options(nostack, preserves_flags),
        );
    }
}
