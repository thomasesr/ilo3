//! USB Mass Storage: Bulk-Only Transport + SCSI block commands — Phase 7 vMedia.
//!
//! Answers the SCSI command set the host's `usb-storage` driver sends against the
//! virtual mass-storage device (docs/29): INQUIRY / TEST UNIT READY / REQUEST
//! SENSE / READ CAPACITY / READ(10) / WRITE(10) / MODE SENSE. Backed by a
//! `BlockDevice` (the vMedia image, `vmedia.rs`).
//!
//! Clean-room from the public USB Mass Storage Bulk-Only Transport spec (Rev 1.0)
//! + SPC/SPC-4 + SBC-3. Provenance: `docs/29-vusb-vmedia.md`. No HP code copied.

#![allow(dead_code)]

use alloc::vec;
use alloc::vec::Vec;

/// CBW signature "USBC" / CSW signature "USBS".
const CBW_SIG: u32 = 0x4342_5355;
const CSW_SIG: u32 = 0x5342_5355;
/// SCSI operation codes.
const OP_TEST_UNIT_READY: u8 = 0x00;
const OP_REQUEST_SENSE: u8 = 0x03;
const OP_INQUIRY: u8 = 0x12;
const OP_MODE_SENSE: u8 = 0x1A;
const OP_START_STOP: u8 = 0x1B;
const OP_PREVENT_ALLOW: u8 = 0x1E;
const OP_READ_CAPACITY: u8 = 0x25;
const OP_READ10: u8 = 0x28;
const OP_WRITE10: u8 = 0x2A;
/// SCSI status.
pub const SCS_OK: u8 = 0x00;
pub const SCS_FAILED: u8 = 0x01;
/// CSW status values.
pub const CSW_OK: u8 = 0x00;
pub const CSW_FAILED: u8 = 0x01;
/// Max blocks per READ(10) — caps the response allocation so a hostile host
/// can't force a multi-MB alloc (nblocks is a 16-bit host field) that OOMs the heap.
const MAX_XFER_BLOCKS: u32 = 256; // 256 * 512 B = 128 KB

/// A block device backing the virtual mass-storage LUN.
pub trait BlockDevice {
    fn block_size(&self) -> u32; // bytes per block (usually 512)
    fn block_count(&self) -> u32;
    fn read(&mut self, lba: u32, buf: &mut [u8]) -> bool;
    fn write(&mut self, lba: u32, buf: &[u8]) -> bool;
    fn removable(&self) -> bool {
        false
    }
    fn media_present(&self) -> bool {
        true
    }
    fn vendor(&self) -> &str {
        "ilo3-rs "
    }
    fn product(&self) -> &str {
        "Virtual Media "
    }
    fn revision(&self) -> &str {
        "1.00"
    }
}

/// A parsed CBW.
pub struct Cbw {
    pub tag: u32,
    pub dir_in: bool,
    pub lun: u8,
    pub cb: [u8; 16],
    pub cb_len: usize,
}

/// Parse a 31-byte CBW; None if the signature is wrong.
pub fn parse_cbw(b: &[u8]) -> Option<Cbw> {
    if b.len() < 31 || u32::from_le_bytes([b[0], b[1], b[2], b[3]]) != CBW_SIG {
        return None;
    }
    let mut cb = [0u8; 16];
    let cb_len = (b[14] & 0x1F) as usize;
    cb.copy_from_slice(&b[15..15 + 16]);
    Some(Cbw {
        tag: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
        dir_in: b[12] & 0x80 != 0,
        lun: b[13] & 0x0F,
        cb,
        cb_len,
    })
}

/// Build the 13-byte CSW for a completed command.
pub fn build_csw(tag: u32, residue: u32, status: u8) -> [u8; 13] {
    let mut c = [0u8; 13];
    c[0..4].copy_from_slice(&CSW_SIG.to_le_bytes());
    c[4..8].copy_from_slice(&tag.to_le_bytes());
    c[8..12].copy_from_slice(&residue.to_le_bytes());
    c[12] = status;
    c
}

/// Dispatch one CBW against the device. Returns (CSW status, data phase bytes).
pub fn handle(cbw: &Cbw, dev: &mut dyn BlockDevice) -> (u8, Option<Vec<u8>>) {
    match cbw.cb[0] {
        OP_TEST_UNIT_READY => (csw_status(dev.media_present()), None),
        OP_REQUEST_SENSE => (SCS_OK, Some(request_sense())),
        OP_INQUIRY => (SCS_OK, Some(inquiry(dev))),
        OP_MODE_SENSE => (SCS_OK, Some(mode_sense(dev))),
        OP_READ_CAPACITY => read_capacity(dev),
        OP_READ10 => read10(&cbw.cb, dev),
        OP_WRITE10 => write10(&cbw.cb, dev),
        OP_START_STOP | OP_PREVENT_ALLOW => (SCS_OK, None),
        _ => (CSW_FAILED, None),
    }
}

fn csw_status(ok: bool) -> u8 {
    if ok {
        CSW_OK
    } else {
        CSW_FAILED
    }
}

/// Standard INQUIRY data (SPC-4 §6.4.2) — peripheral type 0 (SBC direct-access).
fn inquiry(dev: &dyn BlockDevice) -> Vec<u8> {
    let mut r = vec![0u8; 36];
    r[0] = if dev.removable() { 0x80 } else { 0x00 }; // peripheral device type
    r[1] = 0x80; // RMB
    r[2] = 0x04; // SPC-2
    r[3] = 0x02; // response data format
    r[4] = 31; // additional length
    put_str(&mut r[8..16], dev.vendor());
    put_str(&mut r[16..32], dev.product());
    put_str(&mut r[32..36], dev.revision());
    r
}

fn request_sense() -> Vec<u8> {
    // Fixed-format sense: no sense / no additional sense.
    let mut r = vec![0u8; 18];
    r[0] = 0x70; // valid, current errors
    r[7] = 10; // additional sense length
    r
}

fn mode_sense(dev: &dyn BlockDevice) -> Vec<u8> {
    let bs = dev.block_size();
    let mut r = vec![0u8; 8];
    r[3] = 8; // mode data length (header only)
    r[4] = 0x00; // medium type
    r[7] = if dev.removable() { 0x80 } else { 0x00 };
    let _ = bs;
    r
}

fn read_capacity(dev: &mut dyn BlockDevice) -> (u8, Option<Vec<u8>>) {
    if !dev.media_present() {
        return (CSW_FAILED, None);
    }
    let last = dev.block_count().saturating_sub(1);
    let mut r = Vec::with_capacity(8);
    r.extend_from_slice(&last.to_be_bytes());
    r.extend_from_slice(&dev.block_size().to_be_bytes());
    (SCS_OK, Some(r))
}

fn read10(cb: &[u8; 16], dev: &mut dyn BlockDevice) -> (u8, Option<Vec<u8>>) {
    let lba = u32::from_be_bytes([cb[2], cb[3], cb[4], cb[5]]);
    let nblocks = (u16::from_be_bytes([cb[7], cb[8]]) as u32).min(MAX_XFER_BLOCKS);
    let bs = dev.block_size() as usize;
    let mut out = vec![0u8; (nblocks as usize).saturating_mul(bs)];
    let mut off = 0;
    for i in 0..nblocks {
        if (lba as u64) + (i as u64) >= dev.block_count() as u64 {
            break;
        }
        let end = (off + bs).min(out.len());
        if !dev.read(lba + i, &mut out[off..end]) {
            return (CSW_FAILED, Some(out));
        }
        off += bs;
    }
    (SCS_OK, Some(out))
}

fn write10(cb: &[u8; 16], _dev: &mut dyn BlockDevice) -> (u8, Option<Vec<u8>>) {
    // WRITE(10): the host sends data in the DATA-OUT phase; the actual write path
    // feeds blocks through `dev.write` (vusb.rs streams the payload). Here we
    // acknowledge; the block loop is handled by the transfer layer.
    let _lba = u32::from_be_bytes([cb[2], cb[3], cb[4], cb[5]]);
    (SCS_OK, None)
}

/// A WRITE(10) data phase: write `data` starting at `lba`.
pub fn write10_data(cb: &[u8; 16], data: &[u8], dev: &mut dyn BlockDevice) -> u8 {
    let lba = u32::from_be_bytes([cb[2], cb[3], cb[4], cb[5]]);
    let bs = dev.block_size() as usize;
    if bs == 0 {
        return CSW_FAILED;
    }
    let mut off = 0;
    let mut i = 0u32;
    while off + bs <= data.len() && (lba as u64) + (i as u64) < dev.block_count() as u64 {
        if !dev.write(lba + i, &data[off..off + bs]) {
            return CSW_FAILED;
        }
        off += bs;
        i += 1;
    }
    CSW_OK
}

fn put_str(dst: &mut [u8], s: &str) {
    for (i, c) in s.bytes().take(dst.len()).enumerate() {
        dst[i] = c;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Ram([u8; 512]);
    impl BlockDevice for Ram {
        fn block_size(&self) -> u32 {
            512
        }
        fn block_count(&self) -> u32 {
            1
        }
        fn read(&mut self, _lba: u32, buf: &mut [u8]) -> bool {
            buf.copy_from_slice(&self.0);
            true
        }
        fn write(&mut self, _lba: u32, buf: &[u8]) -> bool {
            self.0.copy_from_slice(buf);
            true
        }
    }

    #[test]
    fn parse_roundtrip() {
        let mut c = [0u8; 31];
        c[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
        c[4..8].copy_from_slice(&0x11223344u32.to_le_bytes());
        c[12] = 0x80; // dir IN
        c[14] = 6; // 6-byte CDB
        c[15] = OP_INQUIRY;
        let cbw = parse_cbw(&c).unwrap();
        assert_eq!(cbw.tag, 0x11223344);
        assert!(cbw.dir_in);
        assert_eq!(cbw.cb[0], OP_INQUIRY);
        assert_eq!(cbw.cb_len, 6);
    }

    #[test]
    fn inquiry_and_capacity() {
        let mut dev = Ram([0u8; 512]);
        let mut c = [0u8; 31];
        c[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
        c[14] = 6;
        let mut cbw = parse_cbw(&c).unwrap();
        cbw.cb[0] = OP_INQUIRY;
        let (st, data) = handle(&cbw, &mut dev);
        assert_eq!(st, SCS_OK);
        let d = data.unwrap();
        assert_eq!(d.len(), 36);
        assert_eq!(d[0], 0x00); // non-removable direct-access
        assert_eq!(&d[8..16], b"ilo3-rs ");

        cbw.cb = [0u8; 16];
        cbw.cb[0] = OP_READ_CAPACITY;
        let (st, data) = handle(&cbw, &mut dev);
        assert_eq!(st, SCS_OK);
        let d = data.unwrap();
        assert_eq!(u32::from_be_bytes([d[0], d[1], d[2], d[3]]), 0); // last LBA = 0
        assert_eq!(u32::from_be_bytes([d[4], d[5], d[6], d[7]]), 512);
    }
}
