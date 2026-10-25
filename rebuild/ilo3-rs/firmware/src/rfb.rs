//! RFB 3.8 (VNC) server — Phase 7 KVM (docs/46).
//!
//! Clean-room from RFC 6143. Runs over any async byte stream (the WebSocket
//! `WsStream` in firmware, raw TCP in the host harness). Auth is *None* at the
//! RFB layer — the WebSocket/TLS + web session token already gates access
//! (docs/46), and RFB's own security types are all legacy.
//!
//! Pixel format: 16 bpp RGB565 little-endian (matches `framebuf::Framebuffer`).
//! Encodings: Hextile (preferred) or Raw. Capture is a tile-aligned framebuffer
//! for now (see framebuf.rs); non-aligned modes are a capture-bring-up concern.

#![allow(dead_code)]

use core::fmt;

use alloc::vec;
use alloc::vec::Vec;
use embedded_io_async::{ErrorType, Read, Write};

use crate::framebuf::Framebuffer;
use crate::hextile;

/// Client → server message types (RFC 6143 §6.4).
const MSG_SET_PIXEL_FORMAT: u8 = 0;
const MSG_SET_ENCODINGS: u8 = 2;
const MSG_UPDATE_REQUEST: u8 = 3;
const MSG_KEY_EVENT: u8 = 4;
const MSG_POINTER_EVENT: u8 = 5;
const MSG_CUT_TEXT: u8 = 6;

/// Encodings (RFC 6143 §7.6).
const ENC_RAW: i32 = 0;
const ENC_COPYRECT: i32 = 1;
const ENC_HEXTILE: i32 = 5;

/// Protocol version string (both directions).
const VERSION: &[u8; 12] = b"RFB 003.008\n";

#[derive(Debug)]
pub enum RfbError {
    Io,
    Protocol,
}

impl fmt::Display for RfbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rfb error")
    }
}
impl core::error::Error for RfbError {}
impl embedded_io::Error for RfbError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

/// Input sink: RFB Key/Pointer/CutText events hand off here. The KVM task routes
/// these to the virtual HID (docs/29) once built; a recorder in the harness.
pub trait Input {
    fn key(&mut self, down: bool, keysym: u32);
    fn pointer(&mut self, buttons: u8, x: u16, y: u16);
    fn cut_text(&mut self, _text: &[u8]) {}
}

/// A no-op input sink (until the HID back-channel exists).
pub struct NullInput;
impl Input for NullInput {
    fn key(&mut self, _down: bool, _keysym: u32) {}
    fn pointer(&mut self, _buttons: u8, _x: u16, _y: u16) {}
}

async fn read_exact<S: Read>(s: &mut S, n: usize) -> Result<Vec<u8>, RfbError> {
    let mut buf = vec![0u8; n];
    let mut done = 0;
    while done < n {
        let r = s.read(&mut buf[done..]).await.map_err(|_| RfbError::Io)?;
        if r == 0 {
            return Err(RfbError::Protocol);
        }
        done += r;
    }
    Ok(buf)
}

async fn write_all<S: Write>(s: &mut S, buf: &[u8]) -> Result<(), RfbError> {
    s.write_all(buf).await.map_err(|_| RfbError::Io)
}

/// An RFB server session over an async byte stream.
pub struct RfbSession<'a, S: Read + Write, I: Input> {
    stream: S,
    fb: &'a mut Framebuffer,
    input: I,
    hextile: bool,
    raw: bool,
}

impl<'a, S: Read + Write, I: Input> RfbSession<'a, S, I> {
    /// Version + security + ServerInit. Returns the session ready to `run`.
    pub async fn handshake(
        mut stream: S,
        fb: &'a mut Framebuffer,
        input: I,
    ) -> Result<Self, RfbError> {
        write_all(&mut stream, VERSION).await?;
        // Client version (12 bytes) — accepted, not parsed.
        let _ = read_exact(&mut stream, 12).await?;

        // Security: 1 type, None (TLS + session is the auth boundary).
        write_all(&mut stream, &[1, 1]).await?;
        let _sel = read_exact(&mut stream, 1).await?;
        // SecurityResult OK (0).
        write_all(&mut stream, &0u32.to_be_bytes()).await?;
        // ClientInit: shared-flag byte.
        let _ = read_exact(&mut stream, 1).await?;

        // ServerInit.
        let (w, h) = (fb.width() as u16, fb.height() as u16);
        let mut init = Vec::with_capacity(24 + 11);
        init.extend_from_slice(&w.to_be_bytes());
        init.extend_from_slice(&h.to_be_bytes());
        // PixelFormat (16 bytes): 16bpp RGB565 LE true colour.
        init.extend_from_slice(&[
            16, 16, 0, 1, // bpp, depth, big-endian, true-colour
            0, 31, 0, 63, // red-max, green-max
            0, 31, 11, 5, // blue-max, red-shift, green-shift
            0, 0, 0, 0, // blue-shift, padding
        ]);
        let name = b"ilo3-rs KVM";
        init.extend_from_slice(&(name.len() as u32).to_be_bytes());
        init.extend_from_slice(name);
        write_all(&mut stream, &init).await?;

        Ok(RfbSession { stream, fb, input, hextile: false, raw: true })
    }

    /// Process client messages until the stream ends.
    pub async fn run(mut self) -> Result<(), RfbError> {
        loop {
            let msg = read_exact(&mut self.stream, 1).await?[0];
            match msg {
                MSG_SET_PIXEL_FORMAT => {
                    // 3 bytes padding + 16-byte PixelFormat. We advertise 16bpp
                    // and stay with it regardless (no pixel-format remap yet).
                    let _ = read_exact(&mut self.stream, 19).await?;
                }
                MSG_SET_ENCODINGS => self.set_encodings().await?,
                MSG_UPDATE_REQUEST => self.update_request().await?,
                MSG_KEY_EVENT => {
                    let b = read_exact(&mut self.stream, 7).await?;
                    let down = b[0] != 0;
                    let keysym = u32::from_be_bytes([b[3], b[4], b[5], b[6]]);
                    self.input.key(down, keysym);
                }
                MSG_POINTER_EVENT => {
                    let b = read_exact(&mut self.stream, 5).await?;
                    let buttons = b[0];
                    let x = u16::from_be_bytes([b[1], b[2]]);
                    let y = u16::from_be_bytes([b[3], b[4]]);
                    self.input.pointer(buttons, x, y);
                }
                MSG_CUT_TEXT => {
                    let b = read_exact(&mut self.stream, 7).await?;
                    let len = u32::from_be_bytes([b[3], b[4], b[5], b[6]]) as usize;
                    if len > 0 && len <= 1024 * 1024 {
                        let text = read_exact(&mut self.stream, len).await?;
                        self.input.cut_text(&text);
                    }
                }
                _ => return Err(RfbError::Protocol),
            }
        }
    }

    async fn set_encodings(&mut self) -> Result<(), RfbError> {
        let b = read_exact(&mut self.stream, 3).await?;
        let n = u16::from_be_bytes([b[1], b[2]]) as usize;
        let mut hextile = false;
        let mut raw = false;
        for _ in 0..n {
            let e = read_exact(&mut self.stream, 4).await?;
            let v = i32::from_be_bytes([e[0], e[1], e[2], e[3]]);
            match v {
                ENC_HEXTILE => hextile = true,
                ENC_RAW | ENC_COPYRECT => raw = true,
                _ => {}
            }
        }
        self.hextile = hextile;
        self.raw = raw;
        Ok(())
    }

    async fn update_request(&mut self) -> Result<(), RfbError> {
        let b = read_exact(&mut self.stream, 9).await?;
        let incremental = b[0];
        let x = u16::from_be_bytes([b[1], b[2]]) as usize;
        let y = u16::from_be_bytes([b[3], b[4]]) as usize;
        let w = u16::from_be_bytes([b[5], b[6]]) as usize;
        let h = u16::from_be_bytes([b[7], b[8]]) as usize;

        if incremental == 0 {
            self.fb.mark_region_dirty(x, y, w, h);
        }

        let tx0 = x / hextile::TILE;
        let ty0 = y / hextile::TILE;
        let tx1 = (x + w).div_ceil(hextile::TILE).min(self.fb.tiles_x());
        let ty1 = (y + h).div_ceil(hextile::TILE).min(self.fb.tiles_y());

        let mut body = Vec::new();
        let mut n_rects = 0u16;
        let mut tile = [0u16; hextile::PIXELS];
        for ty in ty0..ty1 {
            for tx in tx0..tx1 {
                if !self.fb.is_dirty(tx, ty) {
                    continue;
                }
                self.fb.copy_tile(tx, ty, &mut tile);
                let (enc, bytes) = if self.hextile {
                    let mut t = Vec::with_capacity(64);
                    hextile::encode_tile(&tile, &mut t);
                    (ENC_HEXTILE, t)
                } else {
                    let mut t = Vec::with_capacity(1 + hextile::PIXELS * 2);
                    t.push(hextile::mask::RAW);
                    for &p in &tile {
                        t.extend_from_slice(&p.to_be_bytes());
                    }
                    (ENC_RAW, t)
                };
                let (rx, ry) = (tx * hextile::TILE, ty * hextile::TILE);
                body.extend_from_slice(&(rx as u16).to_be_bytes());
                body.extend_from_slice(&(ry as u16).to_be_bytes());
                body.extend_from_slice(&(hextile::TILE as u16).to_be_bytes());
                body.extend_from_slice(&(hextile::TILE as u16).to_be_bytes());
                body.extend_from_slice(&enc.to_be_bytes());
                body.extend_from_slice(&bytes);
                n_rects += 1;
                self.fb.clear_dirty_tile(tx, ty);
            }
        }

        let mut msg = Vec::with_capacity(3 + body.len());
        msg.push(0); // FramebufferUpdate
        msg.push((n_rects >> 8) as u8);
        msg.push(n_rects as u8);
        msg.extend_from_slice(&body);
        write_all(&mut self.stream, &msg).await
    }
}

impl<S: Read + Write, I: Input> ErrorType for RfbSession<'_, S, I> {
    type Error = RfbError;
}

impl<S: Read + Write, I: Input> Read for RfbSession<'_, S, I> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.stream.read(buf).await.map_err(|_| RfbError::Io)
    }
}
impl<S: Read + Write, I: Input> Write for RfbSession<'_, S, I> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.stream.write(buf).await.map_err(|_| RfbError::Io)
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.stream.flush().await.map_err(|_| RfbError::Io)
    }
}
