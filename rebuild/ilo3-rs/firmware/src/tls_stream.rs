//! TLS 1.3 stream: wraps an async byte transport with our handshake +
//! record protection, exposing the decrypted plaintext as async Read+Write
//! (Phase 5 web). Feeds picoserve the plaintext HTTP stream.
//!
//! `handshake()` drives the server handshake (ClientHello → ServerHello +
//! encrypted flight → client flight), then the stream encrypts/decrypts
//! application records on read/write.

#![allow(dead_code)]

use core::fmt;

use alloc::vec::Vec;
use embedded_io_async::{ErrorType, Read, Write};

use crate::tls_handshake::ServerHandshake;

/// Maximum plaintext per application-data record (16 KB record cap minus
/// ciphertext/tag/inner-type overhead).
const MAX_PLAINTEXT: usize = 16 * 1024 - 16 - 1;

/// TLS 1.3 max ciphertext record length (RFC 8446 §5.2): 2^14 + 256. Reject
/// anything larger instead of allocating up to the 16-bit length field (64 KB).
const MAX_RECORD: usize = (1 << 14) + 256;

#[derive(Debug)]
pub enum TlsStreamError {
    Io,
    Tls,
}

impl fmt::Display for TlsStreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tls stream error")
    }
}

impl core::error::Error for TlsStreamError {}

impl embedded_io::Error for TlsStreamError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

/// Fill `buf` exactly; returns Ok(false) on EOF before the buffer is full.
async fn read_exact<S: Read>(s: &mut S, buf: &mut [u8]) -> Result<bool, S::Error> {
    let mut n = 0;
    while n < buf.len() {
        let r = s.read(&mut buf[n..]).await?;
        if r == 0 {
            return Ok(false);
        }
        n += r;
    }
    Ok(true)
}

/// Read one TLS record (header + fragment); returns None on clean EOF, Err on an
/// I/O error or an over-length record (> MAX_RECORD).
async fn read_record<S: Read>(s: &mut S) -> Result<Option<(u8, Vec<u8>)>, TlsStreamError> {
    let mut hdr = [0u8; 5];
    if !read_exact(s, &mut hdr).await.map_err(|_| TlsStreamError::Io)? {
        return Ok(None);
    }
    let rtype = hdr[0];
    let len = ((hdr[3] as usize) << 8) | hdr[4] as usize;
    if len > MAX_RECORD {
        return Err(TlsStreamError::Tls);
    }
    let mut full = Vec::with_capacity(5 + len);
    full.extend_from_slice(&hdr);
    full.resize(5 + len, 0);
    if !read_exact(s, &mut full[5..]).await.map_err(|_| TlsStreamError::Io)? {
        return Ok(None);
    }
    Ok(Some((rtype, full)))
}

/// Write one TLS record (header + fragment).
async fn write_record<S: Write>(s: &mut S, rtype: u8, frag: &[u8]) -> Result<(), S::Error> {
    let mut out = Vec::with_capacity(5 + frag.len());
    out.push(rtype);
    out.push(0x03);
    out.push(0x03);
    out.push((frag.len() >> 8) as u8);
    out.push(frag.len() as u8);
    out.extend_from_slice(frag);
    s.write_all(&out).await
}

/// A TLS 1.3 server stream over an async byte transport.
pub struct TlsStream<'a, S: Read + Write> {
    inner: S,
    hs: ServerHandshake<'a>,
    rx_buf: Vec<u8>, // decrypted plaintext not yet returned to the reader
    rx_pos: usize,
}

impl<'a, S: Read + Write> TlsStream<'a, S> {
    /// Perform the server handshake over `inner`, returning the plaintext stream.
    pub async fn handshake(
        mut inner: S,
        mut hs: ServerHandshake<'a>,
    ) -> Result<Self, TlsStreamError> {
        // 1. ClientHello (plaintext handshake record).
        let (rtype, rec) = read_record(&mut inner).await?.ok_or(TlsStreamError::Tls)?;
        if rtype != 22 {
            return Err(TlsStreamError::Tls);
        }
        let resp = hs.client_hello(&rec[5..]).map_err(|_| TlsStreamError::Tls)?;

        // 2. ServerHello + (middlebox) CCS + encrypted flight.
        write_record(&mut inner, 22, &resp.server_hello)
            .await
            .map_err(|_| TlsStreamError::Io)?;
        write_record(&mut inner, 20, &[1])
            .await
            .map_err(|_| TlsStreamError::Io)?;
        inner
            .write_all(&resp.flight)
            .await
            .map_err(|_| TlsStreamError::Io)?;

        // 3. Client flight (may span multiple records).
        loop {
            let (rtype, rec) = read_record(&mut inner)
                .await
                .map_err(|_| TlsStreamError::Io)?
                .ok_or(TlsStreamError::Tls)?;
            if rtype == 20 {
                continue; // skip CCS
            }
            if rtype == 23 {
                if hs.client_record(&rec).map_err(|_| TlsStreamError::Tls)? {
                    break;
                }
            }
        }

        Ok(TlsStream { inner, hs, rx_buf: Vec::new(), rx_pos: 0 })
    }
}

impl<S: Read + Write> ErrorType for TlsStream<'_, S> {
    type Error = TlsStreamError;
}

impl<S: Read + Write> Read for TlsStream<'_, S> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if self.rx_pos >= self.rx_buf.len() {
            loop {
                let rec = read_record(&mut self.inner).await?;
                let Some((rtype, rec)) = rec else {
                    return Ok(0); // EOF
                };
                if rtype != 23 {
                    continue;
                }
                let plain = self.hs.decrypt(&rec).ok_or(TlsStreamError::Tls)?;
                self.rx_buf = plain;
                self.rx_pos = 0;
                break;
            }
        }
        let avail = &self.rx_buf[self.rx_pos..];
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        self.rx_pos += n;
        Ok(n)
    }
}

impl<S: Read + Write> Write for TlsStream<'_, S> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        for chunk in buf.chunks(MAX_PLAINTEXT) {
            let record = self.hs.encrypt(chunk).map_err(|_| TlsStreamError::Tls)?;
            self.inner
                .write_all(&record)
                .await
                .map_err(|_| TlsStreamError::Io)?;
        }
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().await.map_err(|_| TlsStreamError::Io)
    }
}
