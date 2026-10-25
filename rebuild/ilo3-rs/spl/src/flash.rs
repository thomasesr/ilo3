//! Load the main firmware from flash into DDR and jump to it.
//!
//! Flash is memory-mapped XIP at 0xFF800000-0xFFFFFFFF (docs/03: the boot ROM's
//! own "Copying image" reads flash as memory), so the load is a plain copy from
//! the mapped window — no SMIF FIFO (docs/17) needed for reads.
//!
//! Payload layout (our own, written by tools/pack-flash.py) at flash base
//! 0xFF800000: a 16-byte header then the raw main-firmware .bin:
//!   +0x00 u32 magic "GXSP"   +0x04 u32 length   +0x08 u32 load   +0x0C u32 entry
//!
//! Caches/MMU are off throughout the SPL, so the copied code needs no cache
//! maintenance beyond draining the write buffer before the jump.

use core::ptr::{read_volatile, write_volatile};

use crate::uart;

/// Flash base, memory-mapped (flash offset 0 = 0xFF800000).
pub const PAYLOAD_FLASH: usize = 0xFF80_0000;
const HDR_BYTES: usize = 0x10;
const MAGIC: u32 = 0x5053_5847; // "GXSP" little-endian

/// Read the payload header, copy the image into DDR, and jump to its entry.
/// Never returns. Must run with the stack already raised into high DRAM (the
/// copy target overlaps the SPL's pre-DDR scratch stack) — see src/main.rs.
///
/// # Safety
/// Call once, after DDR init, with a valid high stack. Jumps to foreign code.
pub unsafe fn copy_and_jump() -> ! {
    let h = PAYLOAD_FLASH as *const u32;
    let magic = read_volatile(h);
    let len = read_volatile(h.add(1)) as usize;
    let load = read_volatile(h.add(2));
    let entry = read_volatile(h.add(3));

    if magic != MAGIC {
        uart::puts("[spl] bad payload magic ");
        uart::puthex(magic);
        uart::puts(" — halt\n");
        hang();
    }

    uart::puts("[spl] load ");
    uart::puthex(len as u32);
    uart::puts(" B -> ");
    uart::puthex(load);
    uart::puts("\n");

    // Word-copy ceil(len/4) words from flash+0x10 into DDR at `load`. Reading a
    // few trailing bytes past `len` from flash is harmless (still valid flash).
    let src = (PAYLOAD_FLASH + HDR_BYTES) as *const u32;
    let dst = load as *mut u32;
    let words = (len + 3) / 4;
    let mut i = 0;
    while i < words {
        write_volatile(dst.add(i), read_volatile(src.add(i)));
        i += 1;
    }

    // Drain the write buffer so every byte has landed in DRAM before we fetch
    // the copied code (caches are off, so no I-cache invalidate is required).
    core::arch::asm!("mcr p15, 0, {z}, c7, c10, 4", z = in(reg) 0u32, options(nostack, preserves_flags));

    uart::puts("[spl] boot -> ");
    uart::puthex(entry);
    uart::puts("\n");

    // Jump to the loaded firmware's entry (its _start at 0x20000000).
    core::arch::asm!("bx {e}", e = in(reg) entry, options(noreturn));
}

/// Fatal-error park: spin forever (ARMv5 CP15 wait-for-interrupt).
pub fn hang() -> ! {
    loop {
        // SAFETY: CP15 WFI, source value ignored.
        unsafe { core::arch::asm!("mcr p15, 0, {z}, c7, c0, 4", z = in(reg) 0u32) };
    }
}
