//! Host-side harness for the pure Phase 7 modules.
//!
//! `#[path]`-includes the dependency-light firmware modules (they only use
//! `alloc` + `embedded-io-async` + `sha1`) so their `#[cfg(test)]` suites and the
//! handshake paths run on x86_64 — the same pattern used to validate the TLS
//! layer against a real client. The cross-module `crate::` references resolve
//! against this crate's module tree.

#![allow(dead_code)]

extern crate alloc;

#[path = "../../ilo3-rs/firmware/src/hextile.rs"]
mod hextile;
#[path = "../../ilo3-rs/firmware/src/framebuf.rs"]
mod framebuf;
#[path = "../../ilo3-rs/firmware/src/usb_msc.rs"]
mod usb_msc;
#[path = "../../ilo3-rs/firmware/src/hid.rs"]
mod hid;
#[path = "../../ilo3-rs/firmware/src/ws.rs"]
mod ws;
#[path = "../../ilo3-rs/firmware/src/rfb.rs"]
mod rfb;

/// An in-memory duplex: the test pre-fills the client→server bytes, drives the
/// async handshake to completion, then inspects the server→client bytes.
use std::cell::RefCell;
use std::rc::Rc;

use embedded_io_async::{ErrorType, Read, Write};

#[derive(Debug)]
pub struct DuplexError;

impl core::fmt::Display for DuplexError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "duplex error")
    }
}
impl std::error::Error for DuplexError {}
impl embedded_io::Error for DuplexError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

#[derive(Clone)]
pub struct Duplex(Rc<RefCell<Inner>>);

struct Inner {
    rx: Vec<u8>,
    rx_pos: usize,
    tx: Vec<u8>,
}

impl Duplex {
    pub fn new(client_bytes: Vec<u8>) -> Self {
        Duplex(Rc::new(RefCell::new(Inner { rx: client_bytes, rx_pos: 0, tx: Vec::new() })))
    }
    /// The bytes the server wrote (the response, inspected after the test).
    pub fn server_output(&self) -> Vec<u8> {
        self.0.borrow().tx.clone()
    }
}

impl ErrorType for Duplex {
    type Error = DuplexError;
}

impl Read for Duplex {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut inner = self.0.borrow_mut();
        let avail = &inner.rx[inner.rx_pos..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        inner.rx_pos += n;
        Ok(n)
    }
}

impl Write for Duplex {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().tx.extend_from_slice(buf);
        Ok(buf.len())
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_lite::future::block_on;

    /// RFC 6455 §1.3 example: the canonical accept-key test vector.
    #[test]
    fn ws_handshake_accept_key() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let req = format!(
            "GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\nSec-WebSocket-Protocol: binary\r\n\r\n",
            key
        );
        let dup = Duplex::new(req.into_bytes());
        let _ = block_on(async { ws::WsStream::<Duplex>::accept(dup.clone()).await.unwrap() });
        let out = dup.server_output();
        let s = String::from_utf8_lossy(&out);
        // RFC 6455 test vector: SHA1(key+GUID) base64.
        assert!(s.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"), "resp: {}", s);
        assert!(s.contains("Sec-WebSocket-Protocol: binary\r\n"));
        assert!(s.starts_with("HTTP/1.1 101"));
    }

    /// RFB 3.8 server handshake: version → security → result → ServerInit.
    #[test]
    fn rfb_handshake_server_init() {
        // Client: version (12) + security-select (1) + ClientInit shared (1).
        let mut client = b"RFB 003.008\n".to_vec();
        client.push(1); // security type None
        client.push(1); // shared-flag

        let mut fb = framebuf::Framebuffer::new(64, 48);
        let dup = Duplex::new(client);
        let _sess = block_on(async {
            rfb::RfbSession::<Duplex, rfb::NullInput>::handshake(dup.clone(), &mut fb, rfb::NullInput)
                .await
                .unwrap()
        });
        let out = dup.server_output();
        assert_eq!(&out[..12], b"RFB 003.008\n");
        assert_eq!(&out[12..14], &[1, 1]); // 1 security type, None
        assert_eq!(&out[14..18], &[0, 0, 0, 0]); // SecurityResult OK
        let w = u16::from_be_bytes([out[18], out[19]]);
        let h = u16::from_be_bytes([out[20], out[21]]);
        assert_eq!((w, h), (64, 48));
        // PixelFormat 16bpp true-colour RGB565.
        assert_eq!(&out[22..26], &[16, 16, 0, 1]);
        let namelen = u32::from_be_bytes([out[38], out[39], out[40], out[41]]) as usize;
        assert_eq!(&out[42..42 + namelen], b"ilo3-rs KVM");
    }

    /// Hextile round-trip: encode a tile, decode it back to pixels, compare.
    #[test]
    fn hextile_roundtrip() {
        use hextile::*;
        // Two-colour tile: black background, a white 4x3 block at (5,6).
        let mut pix = [0u16; PIXELS];
        for y in 0..3 {
            for x in 0..4 {
                pix[(6 + y) * TILE + (5 + x)] = 0xFFFF;
            }
        }
        let mut enc = Vec::new();
        encode_tile(&pix, &mut enc);

        // Decode.
        let mut dec = [0u16; PIXELS];
        decode(&enc, &mut dec);
        assert_eq!(dec, pix);
    }

    /// Minimal Hextile decoder (Background/Foreground/AnySubrects/Raw).
    fn decode(enc: &[u8], out: &mut [u16; hextile::PIXELS]) {
        use hextile::{mask, PIXELS, TILE};
        let mut o = 0;
        let m = enc[o];
        o += 1;
        if m & mask::RAW != 0 {
            for p in out.iter_mut() {
                *p = u16::from_be_bytes([enc[o], enc[o + 1]]);
                o += 2;
            }
            return;
        }
        let mut bg = 0;
        let mut fg = 0;
        if m & mask::BACKGROUND != 0 {
            bg = u16::from_be_bytes([enc[o], enc[o + 1]]);
            o += 2;
        }
        if m & mask::FOREGROUND != 0 {
            fg = u16::from_be_bytes([enc[o], enc[o + 1]]);
            o += 2;
        }
        for p in out.iter_mut() {
            *p = bg;
        }
        if m & mask::ANY_SUBRECTS != 0 {
            let n = enc[o];
            o += 1;
            for _ in 0..n {
                let xy = enc[o];
                let wh = enc[o + 1];
                o += 2;
                let col = if m & mask::SUBRECTS_COLOURED != 0 {
                    let c = u16::from_be_bytes([enc[o], enc[o + 1]]);
                    o += 2;
                    c
                } else {
                    fg
                };
                let (sx, sy) = ((xy >> 4) as usize, (xy & 0x0F) as usize);
                let (w, h) = (((wh >> 4) + 1) as usize, ((wh & 0x0F) + 1) as usize);
                for py in 0..h {
                    for px in 0..w {
                        out[(sy + py) * TILE + (sx + px)] = col;
                    }
                }
            }
        }
        let _ = PIXELS;
    }
}
