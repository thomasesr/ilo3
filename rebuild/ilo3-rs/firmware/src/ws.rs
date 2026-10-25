//! WebSocket server framing (RFC 6455) — Phase 7 KVM transport.
//!
//! Wraps an async byte stream (our TLS 1.3 `TlsStream`, or a raw TCP stream in
//! the host harness) and exposes binary frames as a plain async byte stream.
//! noVNC speaks this directly (`wss://`), so no websockify proxy is needed
//! (docs/46). Server frames are unmasked; client frames are masked (validated).
//!
//! Clean-room from the public RFC. Provenance: `docs/46-remote-console-rewrite.md`
//! (decision), RFC 6455 (spec). No HP code copied.

#![allow(dead_code)]

use core::fmt;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use embedded_io_async::{ErrorType, Read, Write};

use sha1::{Digest, Sha1};

/// RFC 6455 §4.2.2 magic GUID for the accept-key.
const WS_GUID: &[u8; 36] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Cap a single frame (client→server messages are tiny; this guards the heap).
const MAX_FRAME: usize = 64 * 1024;

#[derive(Debug)]
pub enum WsError {
    Io,
    Protocol,
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "websocket error")
    }
}
impl core::error::Error for WsError {}
impl embedded_io::Error for WsError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

/// Standard-alphabet base64 (RFC 4648 §4) with padding.
fn b64_encode(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Fill `buf` exactly; Ok(false) on EOF before full.
async fn read_exact<S: Read>(s: &mut S, buf: &mut [u8]) -> Result<bool, WsError> {
    let mut n = 0;
    while n < buf.len() {
        let r = s.read(&mut buf[n..]).await.map_err(|_| WsError::Io)?;
        if r == 0 {
            return Ok(false);
        }
        n += r;
    }
    Ok(true)
}

/// A decoded WebSocket frame.
struct Frame {
    opcode: u8,
    payload: Vec<u8>,
}

/// Read one client frame (expects MASK set per RFC 6455 §5.1).
async fn read_frame<S: Read>(s: &mut S) -> Result<Frame, WsError> {
    let mut hdr = [0u8; 2];
    if !read_exact(s, &mut hdr).await? {
        return Err(WsError::Io);
    }
    let opcode = hdr[0] & 0x0F;
    let masked = hdr[1] & 0x80 != 0;
    if !masked {
        return Err(WsError::Protocol); // RFC 6455 §5.1: client frames MUST be masked
    }
    let mut len = (hdr[1] & 0x7F) as usize;
    if len == 126 {
        let mut ext = [0u8; 2];
        read_exact(s, &mut ext).await?;
        len = u16::from_be_bytes(ext) as usize;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        read_exact(s, &mut ext).await?;
        len = u64::from_be_bytes(ext) as usize;
    }
    if len > MAX_FRAME {
        return Err(WsError::Protocol);
    }
    let mut key = [0u8; 4];
    if masked {
        read_exact(s, &mut key).await?;
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        read_exact(s, &mut payload).await?;
    }
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= key[i & 3];
        }
    }
    Ok(Frame { opcode, payload })
}

/// Write one server frame (unmasked).
async fn write_frame<S: Write>(s: &mut S, opcode: u8, payload: &[u8]) -> Result<(), WsError> {
    let mut out = Vec::with_capacity(10 + payload.len());
    out.push(0x80 | opcode);
    let n = payload.len();
    if n < 126 {
        out.push(n as u8);
    } else if n < 65536 {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    s.write_all(&out).await.map_err(|_| WsError::Io)
}

/// A WebSocket server stream: binary frames ↔ byte stream.
pub struct WsStream<S: Read + Write> {
    inner: S,
    rx: Vec<u8>,
    rx_pos: usize,
    closed: bool,
    token: Option<String>, // session token from the request URL (?token=...)
}

/// Extract the `token=` query value from a request line's target (`GET /p?token=X ..`).
fn request_token(request_line: &str) -> Option<String> {
    let target = request_line.split(' ').nth(1)?; // the path?query
    let q = target.split('?').nth(1)?;
    for kv in q.split('&') {
        if let Some(v) = kv.strip_prefix("token=") {
            return Some(v.into());
        }
    }
    None
}

impl<S: Read + Write> WsStream<S> {
    /// Perform the server handshake: read the HTTP Upgrade, answer 101. Expects
    /// the request's `Sec-WebSocket-Key`; echoes `binary` subprotocol if offered.
    pub async fn accept(mut inner: S) -> Result<Self, WsError> {
        // Read up to the blank line ending the request headers.
        let mut buf = Vec::new();
        loop {
            let mut b = [0u8; 1];
            if !read_exact(&mut inner, &mut b).await? {
                return Err(WsError::Protocol);
            }
            buf.push(b[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
            if buf.len() > 4096 {
                return Err(WsError::Protocol);
            }
        }
        let text = core::str::from_utf8(&buf).map_err(|_| WsError::Protocol)?;
        let token = text.lines().next().and_then(request_token);
        let mut key: Option<&str> = None;
        let mut want_binary = false;
        for line in text.split("\r\n") {
            if let Some(v) = line.strip_prefix("sec-websocket-key:") {
                key = Some(v.trim());
            }
            if let Some(v) = line.strip_prefix("Sec-WebSocket-Key:") {
                key = Some(v.trim());
            }
            if let Some(v) = line.strip_prefix("Sec-WebSocket-Protocol:") {
                if v.to_ascii_lowercase().contains("binary") {
                    want_binary = true;
                }
            }
        }
        let key = key.ok_or(WsError::Protocol)?;

        let mut hasher = Sha1::new();
        hasher.update(key.as_bytes());
        hasher.update(WS_GUID);
        let accept = b64_encode(&hasher.finalize());

        let mut resp = String::from(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n",
        );
        resp.push_str(&alloc::format!("Sec-WebSocket-Accept: {}\r\n", accept));
        if want_binary {
            resp.push_str("Sec-WebSocket-Protocol: binary\r\n");
        }
        resp.push_str("\r\n");
        inner.write_all(resp.as_bytes()).await.map_err(|_| WsError::Io)?;

        Ok(WsStream { inner, rx: Vec::new(), rx_pos: 0, closed: false, token })
    }

    /// The session token from the request URL (`?token=`), if any — the KVM path
    /// authenticates the console connection with it.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Send a close frame (graceful shutdown).
    pub async fn close(&mut self) -> Result<(), WsError> {
        if !self.closed {
            write_frame(&mut self.inner, 8, &[]).await?;
            self.closed = true;
        }
        Ok(())
    }
}

impl<S: Read + Write> ErrorType for WsStream<S> {
    type Error = WsError;
}

impl<S: Read + Write> Read for WsStream<S> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.closed {
            return Ok(0);
        }
        if self.rx_pos >= self.rx.len() {
            loop {
                let frame = read_frame(&mut self.inner).await?;
                match frame.opcode {
                    0x1 | 0x2 => {
                        self.rx = frame.payload;
                        self.rx_pos = 0;
                        break;
                    }
                    0x8 => {
                        self.closed = true;
                        return Ok(0);
                    }
                    0x9 => write_frame(&mut self.inner, 0xA, &frame.payload).await?,
                    _ => {} // pong / others: ignore
                }
            }
        }
        let avail = &self.rx[self.rx_pos..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.rx_pos += n;
        Ok(n)
    }
}

impl<S: Read + Write> Write for WsStream<S> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if self.closed {
            return Err(WsError::Io);
        }
        write_frame(&mut self.inner, 0x2, buf).await?;
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await.map_err(|_| WsError::Io)
    }
}
