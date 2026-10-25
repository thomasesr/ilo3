//! Minimal polled console UART for SPL banners (TX only).
//!
//! Gromit-XE custom 8-bit UART @ 0xC00000F0 (docs/05): data +0x00, status +0x05
//! (TXRDY = bit5). Same block the main firmware drives; usable early because the
//! core clock is strap-driven at reset. No .data/.bss — safe to call pre-DDR.

use core::ptr::{read_volatile, write_volatile};

const DATA: *mut u8 = 0xC000_00F0 as *mut u8;
const STATUS: *const u8 = 0xC000_00F5 as *const u8;
const TXRDY: u8 = 0x20; // status bit5 = TX holding empty

#[inline]
fn put_byte(b: u8) {
    // SAFETY: fixed, valid MMIO console registers.
    unsafe {
        while read_volatile(STATUS) & TXRDY == 0 {}
        write_volatile(DATA, b);
    }
}

/// Write a string to the console, expanding LF to CRLF.
pub fn puts(s: &str) {
    for &b in s.as_bytes() {
        if b == b'\n' {
            put_byte(b'\r');
        }
        put_byte(b);
    }
}

/// Write a u32 as 8 hex digits with a `0x` prefix (no allocations).
pub fn puthex(mut v: u32) {
    puts("0x");
    let mut buf = [0u8; 8];
    for i in (0..8).rev() {
        let nib = (v & 0xF) as u8;
        buf[i] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        v >>= 4;
    }
    // SAFETY: buf is ASCII hex.
    puts(unsafe { core::str::from_utf8_unchecked(&buf) });
}
