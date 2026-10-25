//! Virtual media backend — Phase 7 vMedia (docs/29).
//!
//! The mass-storage LUN's block device: an in-memory image (floppy `.img` / ISO
//! `.iso` / USB key) with INSERT/EJECT state, matching the original's `vm_*`
//! manager (`vm_url` / `vm_script` / `vm_stream`). An HTTP(S) URL fetch seam is
//! stubbed here — it rides `smoltcp` (networking is MAC-ring HW-gated, docs/11).
//!
//! Clean-room: the media-source concept is from `docs/29-vusb-vmedia.md`; the
//! block semantics are ours. No HP code copied.

#![allow(dead_code)]

use alloc::vec::Vec;

use crate::usb_msc::BlockDevice;

/// Media kind (drives the peripheral/removable bits).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Floppy,
    Cdrom,
    UsbKey,
}

/// An in-memory media image + attach state.
pub struct Image {
    kind: Kind,
    block_size: u32,
    data: Vec<u8>,
    attached: bool,
}

impl Image {
    /// A new detached image of `block_size`-byte blocks.
    pub fn new(kind: Kind, block_size: u32, data: Vec<u8>) -> Self {
        Image { kind, block_size, data, attached: true }
    }

    pub fn empty(kind: Kind) -> Self {
        Image { kind, block_size: 512, data: Vec::new(), attached: false }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// Insert/eject (docs/29: INSERT/EJECT commands).
    pub fn set_attached(&mut self, on: bool) {
        self.attached = on;
    }

    pub fn attached(&self) -> bool {
        self.attached
    }

    /// Replace the backing image and attach it.
    pub fn load(&mut self, data: Vec<u8>) {
        self.data = data;
        self.attached = true;
    }

    /// Fetch a `.img`/`.iso` from a URL. Stubbed — needs the smoltcp HTTP client
    /// (GET + Range) once networking passes traffic (docs/46 HW-gated).
    pub fn load_from_url(&mut self, _url: &str) -> Result<(), ()> {
        Err(()) // CONFIRM-ON-HW: implement the HTTP GET + block streaming.
    }
}

impl BlockDevice for Image {
    fn block_size(&self) -> u32 {
        self.block_size
    }
    fn block_count(&self) -> u32 {
        (self.data.len() as u32).div_ceil(self.block_size)
    }
    fn read(&mut self, lba: u32, buf: &mut [u8]) -> bool {
        let bs = self.block_size as usize;
        let start = lba as usize * bs;
        let end = (start + bs).min(self.data.len());
        if start >= self.data.len() {
            return false;
        }
        buf[..end - start].copy_from_slice(&self.data[start..end]);
        true
    }
    fn write(&mut self, lba: u32, buf: &[u8]) -> bool {
        let bs = self.block_size as usize;
        let start = lba as usize * bs;
        if start + buf.len() > self.data.len() {
            return false;
        }
        self.data[start..start + buf.len()].copy_from_slice(buf);
        true
    }
    fn removable(&self) -> bool {
        true
    }
    fn media_present(&self) -> bool {
        self.attached && !self.data.is_empty()
    }
    fn vendor(&self) -> &str {
        "ilo3-rs "
    }
    fn product(&self) -> &str {
        match self.kind {
            Kind::Floppy => "Virtual Floppy ",
            Kind::Cdrom => "Virtual CD-ROM ",
            Kind::UsbKey => "Virtual USB Key",
        }
    }
    fn revision(&self) -> &str {
        "1.00"
    }
}
