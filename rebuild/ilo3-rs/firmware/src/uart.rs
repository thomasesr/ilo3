//! Gromit-XE console UART — polled driver (HAL, Phase 1).
//!
//! Custom HP 8-bit UART, NOT a 16550 (docs/05, confirmed from decompiled
//! putchar/getchar). No baud/line-control registers in this block.
//!
//!   data   @ base+0x00   write = TX, read = RX   (8-bit)
//!   status @ base+0x05   bit5 (0x20) = TX ready,  bit0 (0x01) = RX ready
//!
//! Base = 0xC00000F0 in the 0xC0000000 core block (identity-mapped, VA=PA).

use core::ptr::{read_volatile, write_volatile};

const UART_BASE: usize = 0xC000_00F0;
const REG_DATA: usize = 0x00;
const REG_STATUS: usize = 0x05;
const TXRDY: u8 = 0x20; // status bit5 = TX holding empty
const RXRDY: u8 = 0x01; // status bit0 = RX data available

/// A polled handle to a Gromit-XE UART. Zero-sized; the MMIO base is fixed.
pub struct Uart {
    base: usize,
}

impl Uart {
    /// The system console UART at 0xC00000F0.
    #[inline]
    pub const fn console() -> Self {
        Uart { base: UART_BASE }
    }

    #[inline]
    fn status(&self) -> u8 {
        // SAFETY: MMIO read of a fixed, valid device register.
        unsafe { read_volatile((self.base + REG_STATUS) as *const u8) }
    }

    /// Block until the TX holding register is empty, then write one byte.
    #[inline]
    pub fn write_byte(&self, b: u8) {
        while self.status() & TXRDY == 0 {}
        // SAFETY: MMIO write of a fixed, valid device register.
        unsafe { write_volatile((self.base + REG_DATA) as *mut u8, b) };
    }

    /// Write a byte slice, expanding LF to CRLF for terminal sanity.
    pub fn write_bytes(&self, bytes: &[u8]) {
        for &b in bytes {
            if b == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(b);
        }
    }

    /// Non-blocking read of one byte if the RX register has data.
    #[allow(dead_code)] // RX path wired up in a later phase
    #[inline]
    pub fn read_byte(&self) -> Option<u8> {
        if self.status() & RXRDY != 0 {
            // SAFETY: MMIO read of a fixed, valid device register.
            Some(unsafe { read_volatile((self.base + REG_DATA) as *const u8) })
        } else {
            None
        }
    }
}
