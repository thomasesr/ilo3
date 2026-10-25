//! Debug log ring + tee writer — the host-pull early-boot buffer (bring-up).
//!
//! No JTAG/UART access on this board, so firmware debug output goes HERE: a RAM
//! ring buffer that fills from `main` onward (post-DDR). The host, which boots
//! AFTER iLO, connects over CHIF later and *drains* the ring — so nothing printed
//! before the host was up is lost (host-pull, not push). Pre-DDR/SPL phases use
//! the separate flash-breadcrumb; this covers main → runtime.
//!
//! `con()` returns a tee that writes each byte to both the console UART (harmless
//! if unrouted — the ASIC UART still ACKs) and the ring, so existing `writeln!`
//! banners are captured without rewriting every call site.

#![allow(dead_code)]

use core::cell::RefCell;
use core::fmt;

use critical_section::Mutex;

use crate::uart::Uart;

const RING: usize = 32 * 1024;

struct Ring {
    buf: [u8; RING],
    head: usize, // next write position
    avail: usize, // unread bytes held (<= RING; oldest dropped on overrun)
}

impl Ring {
    const fn new() -> Self {
        Ring { buf: [0; RING], head: 0, avail: 0 }
    }
    fn push(&mut self, b: u8) {
        self.buf[self.head] = b;
        self.head = (self.head + 1) % RING;
        if self.avail < RING {
            self.avail += 1;
        }
    }
    /// Copy up to `out.len()` oldest-unread bytes out; returns how many.
    fn drain(&mut self, out: &mut [u8]) -> usize {
        let n = self.avail.min(out.len());
        let start = (self.head + RING - self.avail) % RING;
        for (i, o) in out[..n].iter_mut().enumerate() {
            *o = self.buf[(start + i) % RING];
        }
        self.avail -= n;
        n
    }
}

static RING_LOG: Mutex<RefCell<Ring>> = Mutex::new(RefCell::new(Ring::new()));

/// Append bytes to the ring (and nothing else).
pub fn log_bytes(bytes: &[u8]) {
    critical_section::with(|cs| {
        let mut r = RING_LOG.borrow_ref_mut(cs);
        for &b in bytes {
            r.push(b);
        }
    });
}

/// Drain up to `out.len()` buffered bytes for the host (CHIF debug DRAIN).
pub fn drain(out: &mut [u8]) -> usize {
    critical_section::with(|cs| RING_LOG.borrow_ref_mut(cs).drain(out))
}

/// Bytes currently buffered.
pub fn available() -> usize {
    critical_section::with(|cs| RING_LOG.borrow_ref(cs).avail)
}

/// A tee writer: console UART + the ring. Use `writeln!(dbglog::con(), ...)`.
pub struct Tee(Uart);

pub fn con() -> Tee {
    Tee(Uart::console())
}

impl fmt::Write for Tee {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_bytes(s.as_bytes()); // harmless if the UART is unrouted
        log_bytes(s.as_bytes()); // captured for the host to drain
        Ok(())
    }
}
