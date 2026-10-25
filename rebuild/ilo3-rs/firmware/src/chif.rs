//! CHIF host-interface transport — BMC side (Phase 6, docs/28).
//!
//! Gromit-XE host<->BMC Channel InterFace: one shared-memory + doorbell transport
//! carrying IPMI-over-PCI, health, inventory, and (later) vMedia/vKVM. The host
//! side is upstream `hpilo.c`; the BMC side is net-new. This is the transport
//! (framing + CCB ownership handshake + doorbells); channel->service routing sits
//! on top (a chif task dispatching to ipmi/inventory/...).
//!
//! Registers (CONFIRMED, docs/28); shared-mem window is identity-mapped at
//! 0x40000000 (docs/13). CCB ownership convention + exact field widths are the
//! RE's best reading — CONFIRM-ON-HW with the host driver.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

// MMIO control registers.
const BAR_REG: usize = 0x01F0_2050; // host-window BAR programming
const CTRL0: usize = 0x01F0_20D0;
const CTRL1: usize = 0x01F0_20D4; // init: enable (0x04000000); TX: (1<<channel) = ring host
const RX_DOORBELL: usize = 0x01F0_3004; // u16; bit2 = host->BMC message pending
const RX_PENDING: u16 = 0x0004;
const CTRL1_ENABLE: u32 = 0x0400_0000;

// Shared memory (host PCI BAR2; identity-mapped in BMC space).
const SHMEM: usize = 0x4000_0000;
const DATA_REGION: usize = 0x4000; // 16 KB payload area before the header
const MAGIC_CHIF: u32 = 0x4649_4843; // "CHIF" (msg type 0x18)
const MAGIC_CHI5: u32 = 0x3549_4843; // "CHI5" (ChifLite, type 0x19)

// CCBs (shared memory) + channel descriptors (BMC RAM).
const NR_CCB: usize = 32;
const CCB_SIZE: usize = 0x80;
const CCB_OFFSET: usize = 0x04; // u32 message offset into the window
const CCB_OWN_A: usize = 0x10; // u32 ring control, bit31 = ownership
const OWN_BIT: u32 = 1 << 31;
const NR_DESC: usize = 10;
pub const CH_FREE: u16 = 0xFFFF;

#[inline]
fn rd32(a: usize) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
#[inline]
fn wr32(a: usize, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) };
}
#[inline]
fn shmem_u8(off: usize) -> u8 {
    unsafe { read_volatile((SHMEM + off) as *const u8) }
}
#[inline]
fn shmem_u16(off: usize) -> u16 {
    (shmem_u8(off) as u16) | ((shmem_u8(off + 1) as u16) << 8)
}

fn ccb_base(i: usize) -> usize {
    SHMEM + i * CCB_SIZE
}

/// A received CHIF message: channel id + a copy of the payload.
pub struct RxMsg {
    pub channel: u16,
    pub ccb: usize,
    pub len: usize,
}

/// The CHIF transport. Owns the 10 channel descriptors' ids (BMC RAM).
pub struct Chif {
    channels: [u16; NR_DESC], // descriptor +0x28 channel ids (CH_FREE = unused)
}

impl Chif {
    pub const fn new() -> Self {
        Chif { channels: [CH_FREE; NR_DESC] }
    }

    /// Program the BAR + enable the interface. `window_phys`/`log2_size` describe
    /// the shared-memory window as the host sees it (docs/28 §2).
    pub fn init(&mut self, window_phys: u32, log2_size: u32) {
        wr32(BAR_REG, (window_phys.wrapping_sub(0x4000_0000)) | log2_size);
        wr32(CTRL0, 0);
        wr32(CTRL1, CTRL1_ENABLE);
    }

    /// Open a channel: claim a free descriptor for `id`. Returns its index.
    pub fn open_channel(&mut self, id: u16) -> Option<usize> {
        let i = self.channels.iter().position(|&c| c == CH_FREE)?;
        self.channels[i] = id;
        Some(i)
    }

    pub fn close_channel(&mut self, id: u16) {
        if let Some(i) = self.channels.iter().position(|&c| c == id) {
            self.channels[i] = CH_FREE;
        }
    }

    /// Host->BMC message pending? (doorbell status bit2).
    pub fn rx_pending() -> bool {
        unsafe { read_volatile(RX_DOORBELL as *const u16) & RX_PENDING != 0 }
    }

    /// Validate the global control header at shmem+0x4000: magic + 14-byte
    /// checksum (sum of the 14 bytes & 0xff == 0).
    fn header_ok(&self) -> bool {
        let magic = rd32(SHMEM + DATA_REGION);
        if magic != MAGIC_CHIF && magic != MAGIC_CHI5 {
            return false;
        }
        let sum = (0..14).fold(0u8, |a, i| a.wrapping_add(shmem_u8(DATA_REGION + i)));
        sum == 0
    }

    /// Poll for one received message, copying its payload into `buf`. Returns the
    /// channel id + length. Caller dispatches, then calls `rx_release`.
    pub fn poll_rx(&mut self, buf: &mut [u8]) -> Option<RxMsg> {
        if !Self::rx_pending() || !self.header_ok() {
            return None;
        }
        // Find a CCB the host has produced (ownership bit set).
        let ccb = (0..NR_CCB).find(|&i| rd32(ccb_base(i) + CCB_OWN_A) & OWN_BIT != 0)?;
        let off = rd32(ccb_base(ccb) + CCB_OFFSET) as usize;
        if off + DATA_REGION + 8 > (NR_CCB * CCB_SIZE).max(0x10_0000) {
            return None; // bounds guard against a bad offset
        }
        let msg = off; // message region = shmem + offset
        // length @ +0x00 = payload length; channel @ +0x06; payload @ +0x08
        // (mirror of send()). Copy the PAYLOAD, not the header.
        let len = (shmem_u16(msg) as usize)
            .min(DATA_REGION - 8)
            .min(buf.len());
        let channel = shmem_u16(msg + 0x06);
        for (i, b) in buf[..len].iter_mut().enumerate() {
            *b = shmem_u8(msg + 0x08 + i);
        }
        Some(RxMsg { channel, ccb, len })
    }

    /// Release an RX CCB back to the host (clear the ownership bit).
    pub fn rx_release(&self, ccb: usize) {
        let a = ccb_base(ccb) + CCB_OWN_A;
        wr32(a, rd32(a) & !OWN_BIT);
    }

    /// Send a response on `channel`: write the payload into CCB 0's message
    /// region, set the per-message header, claim ownership, ring the host.
    pub fn send(&self, channel: u16, ccb: usize, data: &[u8]) {
        let off = rd32(ccb_base(ccb) + CCB_OFFSET) as usize;
        let n = data.len().min(DATA_REGION - 8);
        unsafe {
            // length @ +0x00, channel id @ +0x06, payload @ +0x08
            write_volatile((SHMEM + off) as *mut u16, n as u16);
            write_volatile((SHMEM + off + 0x06) as *mut u16, channel);
            for (i, &b) in data[..n].iter().enumerate() {
                write_volatile((SHMEM + off + 0x08 + i) as *mut u8, b);
            }
        }
        // claim ownership (BMC produced), then ring the host doorbell
        let a = ccb_base(ccb) + CCB_OWN_A;
        wr32(a, rd32(a) | OWN_BIT);
        if let Some(slot) = self.channels.iter().position(|&c| c == channel) {
            wr32(CTRL1, 1 << slot);
        }
    }
}
