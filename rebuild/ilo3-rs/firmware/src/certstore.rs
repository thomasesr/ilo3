//! TLS certificate / private-key store (Phase 4, docs/32).
//!
//! Holds the server X.509 cert (DER) + private key (PKCS8 DER) for the HTTPS/TLS
//! server, parsed from PEM and persisted in boot flash via crate::spinor. The
//! iLO self-signed cert (or a user-imported one) lived in NVRAM/flash on the
//! original (docs/32); this is the clean-room equivalent.
//!
//! Flash layout at CERT_FLASH_ADDR (one 64 KB erase block): header
//! [magic "GXCS", cert_len u32, key_len u32] then cert DER then key DER.
//! Flash I/O rides on the SMIF controller — CONFIRM-ON-HW with the driver.

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;

use crate::spinor;

/// Flash region for the cert store (flash offset; SMIF 24-bit address).
pub const CERT_FLASH_ADDR: u32 = 0x0060_0000; // 6 MB, a dedicated 64 KB block
const MAGIC: u32 = 0x4743_5343; // "GXCS"
const HDR_LEN: usize = 12;

/// A parsed cert + key pair, held on the heap.
pub struct CertStore {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
}

/// Decode a base64 string (standard alphabet, padding + whitespace tolerant).
fn b64_decode(s: &[u8]) -> Option<Vec<u8>> {
    #[inline]
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s {
        if c == b'=' {
            break;
        }
        let Some(v) = val(c) else { continue }; // skip newlines/spaces
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Extract the DER body of the first PEM block with `-----BEGIN <label>-----`.
pub fn pem_to_der(pem: &[u8], label: &str) -> Option<Vec<u8>> {
    let begin = {
        let mut m = Vec::new();
        m.extend_from_slice(b"-----BEGIN ");
        m.extend_from_slice(label.as_bytes());
        m.extend_from_slice(b"-----");
        m
    };
    let text = core::str::from_utf8(pem).ok()?;
    let marker = core::str::from_utf8(&begin).ok()?;
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find("-----END")?;
    b64_decode(rest[..end].as_bytes())
}

impl CertStore {
    /// Build from PEM cert + PKCS8 PEM key.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Option<Self> {
        let cert_der = pem_to_der(cert_pem, "CERTIFICATE")?;
        let key_der = pem_to_der(key_pem, "PRIVATE KEY")?;
        Some(CertStore { cert_der, key_der })
    }

    /// Persist to flash: erase the block, then program header + cert + key.
    pub fn save_to_flash(&self) -> Result<(), spinor::Error> {
        let mut blob = Vec::with_capacity(HDR_LEN + self.cert_der.len() + self.key_der.len());
        blob.extend_from_slice(&MAGIC.to_le_bytes());
        blob.extend_from_slice(&(self.cert_der.len() as u32).to_le_bytes());
        blob.extend_from_slice(&(self.key_der.len() as u32).to_le_bytes());
        blob.extend_from_slice(&self.cert_der);
        blob.extend_from_slice(&self.key_der);
        spinor::erase_64k(CERT_FLASH_ADDR)?;
        spinor::page_program(CERT_FLASH_ADDR, &blob)
    }

    /// Load from flash. None if the magic is absent / lengths are implausible.
    pub fn load_from_flash() -> Option<Self> {
        let mut hdr = [0u8; HDR_LEN];
        spinor::read(CERT_FLASH_ADDR, &mut hdr).ok()?;
        if u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) != MAGIC {
            return None;
        }
        let cert_len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        let key_len = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]) as usize;
        if cert_len > 0x8000 || key_len > 0x2000 {
            return None; // sanity: fits a 64 KB block
        }
        let mut cert_der = vec![0u8; cert_len];
        let mut key_der = vec![0u8; key_len];
        spinor::read(CERT_FLASH_ADDR + HDR_LEN as u32, &mut cert_der).ok()?;
        spinor::read(CERT_FLASH_ADDR + HDR_LEN as u32 + cert_len as u32, &mut key_der).ok()?;
        Some(CertStore { cert_der, key_der })
    }
}
