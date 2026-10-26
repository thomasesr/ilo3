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

// Shared memory (host PCI BAR2; identity-mapped in BMC space). SHMEM is the ASIC
// host-window aperture base (0x40000000, docs/13 CHIF-shmem region); the window
// must be size-aligned and backed by BMC RAM the host-window hardware can reach.
// WINDOW_SIZE is what the host enumerates as BAR2 — it MUST be large enough to
// hold every offset we use (CCBs 0..0x1000, header 0x4000, messages up to 0xC000+
// 0x4000 = 0x10000). 1 MB matches HP's firmware (docs/28 §10) with margin.
const SHMEM: usize = 0x4000_0000;
const WINDOW_SIZE: u32 = 0x10_0000; // 1 MB; power-of-two, >= 0x10000 used range
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

    /// Compute the `0x01F02050` BAR value for a window at `base` of `size` bytes.
    ///
    /// Encoding decompiled from HP's chif init (ilo3_194.bin @0x14fc40, docs/28
    /// §10): the low bits are a size FIELD = `bit_length(size / 4096)`, and the
    /// ASIC decodes `size = 4096 << (field - 1)` — so 1 MB → field 9, 64 KB → 5.
    /// The high bits are `base - 0x40000000` (base must be size-aligned so the
    /// field bits never collide). The host PCI-enumerates BAR2 at this size.
    ///
    /// The original firmware wrote `log2(size)` here (20 for 1 MB) → field 20 →
    /// `size = 4096 << 19 = 2 GB` → invalid → the ASIC defaulted the host BAR to
    /// 4 KB, which overflowed every window offset and broke the whole interface.
    pub fn bar_value(base: u32, size: u32) -> u32 {
        let field = 32 - (size >> 12).leading_zeros(); // bit_length(size/4096)
        base.wrapping_sub(0x4000_0000) | field
    }

    /// Program the BAR + enable the interface. Uses SHMEM/WINDOW_SIZE. NOTE: the
    /// host enumerates the BAR size at its PCI probe, which happens during host
    /// POST — AFTER iLO releases host power. The SPL programs the same value
    /// earlier (spl/src/recovery.rs) so the size is already correct at enumeration;
    /// this call keeps the window live for the running firmware.
    pub fn init(&mut self) {
        wr32(BAR_REG, Self::bar_value(SHMEM as u32, WINDOW_SIZE));
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
    /// channel id + length. Caller dispatches, then calls `rx_release`. Gated on
    /// the doorbell (`0x01F03004` bit2) — the production path used by chif_server.
    pub fn poll_rx(&mut self, buf: &mut [u8]) -> Option<RxMsg> {
        if !Self::rx_pending() || !self.header_ok() {
            return None;
        }
        self.rx_from_ccb(buf)
    }

    /// Doorbell-FREE receive: scan CCB ownership directly, no `rx_pending()` gate.
    /// Used by the debug/reflash channel (dbgchif) so a host that cannot ring the
    /// host->BMC doorbell (BAR3 aperture, CONFIRM-ON-HW) can still drive it — the
    /// periodic poll picks up any CCB the host marks owned, exactly like the
    /// doorbell-free SPL recovery mailbox. `header_ok()` (magic + checksum) still
    /// guards against acting on a stale/garbage window, and `rx_release` clears the
    /// ownership bit after each request so a static header can't re-trigger.
    pub fn poll_rx_polled(&mut self, buf: &mut [u8]) -> Option<RxMsg> {
        if !self.header_ok() {
            return None;
        }
        self.rx_from_ccb(buf)
    }

    /// Shared RX core: find a host-produced CCB (ownership bit set) and copy its
    /// payload into `buf`. (Bounds-guarded against a bad message offset.)
    fn rx_from_ccb(&mut self, buf: &mut [u8]) -> Option<RxMsg> {
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
