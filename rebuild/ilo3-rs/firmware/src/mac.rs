//! GXELAN MAC + MDIO — register HAL (Phase 2).
//!
//! Ported from `gxe-gmac.c` (docs/11). Two NICs: MAC0 @ 0xC0004000 (dedicated
//! iLO port, own PHY on MDIO) and MAC1 @ 0xC0005000 (sideband NC-SI to the host
//! LOM, no PHY). MDIO physically lives in the MAC0 block (0xC0004080/84). Global
//! reset is in the ASIC core block 0xC00000C0 (MAC0 = bit2, MAC1 = bit7).
//!
//! CONFIRMED and implemented: MDIO protocol, MAC reset, station-address regs
//! (+0x18/1C/20), multicast hash filter (+0x24/28). The TX/RX DMA rings are NOT
//! statically recoverable (fw uses INTEGRITY DMA descriptor objects) — the
//! datapath is a Phase-3 (smoltcp) hardware bring-up item, not in this HAL.

#![allow(dead_code)]

use core::ptr::{read_volatile, write_volatile};

use critical_section as cs;

const MAC0_BASE: usize = 0xC000_4000;
const MAC1_BASE: usize = 0xC000_5000;
const MDIO_BASE: usize = 0xC000_4080; // in the MAC0 block
const CORE_RST: usize = 0xC000_00C0; // ASIC core reset reg (16-bit)

// MAC register offsets (CONFIRMED).
const CTRL: usize = 0x14; // 16-bit control/command
const ADDR0: usize = 0x18; // station addr, big-endian packed
const ADDR1: usize = 0x1C;
const ADDR2: usize = 0x20;
const HASH0: usize = 0x24;
const HASH1: usize = 0x28;

// MDIO (CMD +0x00, DATA +0x04 from MDIO_BASE).
const MDIO_READ: u32 = 1 << 8;
const MDIO_GO: u32 = 1 << 9;
const MDIO_POLL_MAX: u32 = 1_000_000;

const RST_MAC0: u16 = 1 << 2; // 0x04
const RST_MAC1: u16 = 1 << 7; // 0x80

/// Ethernet CRC poly. Standard MSB-first Ethernet CRC-32 (what the reference's
/// `ether_crc` uses). fw RE noted 0x04C11DB6 (1 bit off) — CONFIRM ON HW.
const ETHER_CRC_POLY: u32 = 0x04C1_1DB7;

/// One MAC instance.
pub struct Mac {
    base: usize,
    rst_bit: u16,
}

impl Mac {
    pub const fn mac0() -> Self {
        Mac { base: MAC0_BASE, rst_bit: RST_MAC0 }
    }
    pub const fn mac1() -> Self {
        Mac { base: MAC1_BASE, rst_bit: RST_MAC1 }
    }

    /// Pulse the global MAC reset for this NIC (set bit, then clear).
    pub fn reset(&self) {
        cs::with(|_| unsafe {
            let p = CORE_RST as *mut u16;
            let v = read_volatile(p as *const u16);
            write_volatile(p, v | self.rst_bit);
            write_volatile(p, v & !self.rst_bit);
        });
    }

    /// Program the station MAC address (6 bytes, big-endian into 3×16-bit regs).
    pub fn set_addr(&self, a: &[u8; 6]) {
        // SAFETY: fixed, valid MAC MMIO.
        unsafe {
            write_volatile((self.base + ADDR0) as *mut u16, ((a[0] as u16) << 8) | a[1] as u16);
            write_volatile((self.base + ADDR1) as *mut u16, ((a[2] as u16) << 8) | a[3] as u16);
            write_volatile((self.base + ADDR2) as *mut u16, ((a[4] as u16) << 8) | a[5] as u16);
        }
    }

    /// Write the 64-bit multicast hash filter (words 0/1).
    pub fn set_mcast_hash(&self, h0: u32, h1: u32) {
        // SAFETY: fixed, valid MAC MMIO.
        unsafe {
            write_volatile((self.base + HASH0) as *mut u32, h0);
            write_volatile((self.base + HASH1) as *mut u32, h1);
        }
    }

    /// Write the 16-bit MAC control register.
    pub fn set_ctrl(&self, v: u16) {
        // SAFETY: fixed, valid MAC MMIO.
        unsafe { write_volatile((self.base + CTRL) as *mut u16, v) };
    }
}

// ---- MDIO (shared, physically in the MAC0 block) ----

#[inline]
fn mdio_wait() -> bool {
    let mut n = 0;
    while unsafe { read_volatile(MDIO_BASE as *const u32) } & MDIO_GO != 0 {
        n += 1;
        if n > MDIO_POLL_MAX {
            return false;
        }
    }
    true
}

/// Read PHY register `reg` of `phy` over MDIO. Returns None on timeout.
pub fn mdio_read(phy: u8, reg: u8) -> Option<u16> {
    let cmd = (reg as u32 & 0x1f) | ((phy as u32 & 0x1f) << 16) | MDIO_READ;
    // SAFETY: fixed, valid MDIO MMIO.
    unsafe {
        write_volatile(MDIO_BASE as *mut u32, cmd);
        write_volatile(MDIO_BASE as *mut u32, cmd | MDIO_GO);
    }
    if !mdio_wait() {
        return None;
    }
    Some(unsafe { read_volatile((MDIO_BASE + 4) as *const u32) } as u16)
}

/// Write `val` to PHY register `reg` of `phy` over MDIO. False on timeout.
pub fn mdio_write(phy: u8, reg: u8, val: u16) -> bool {
    let cmd = (reg as u32 & 0x1f) | ((phy as u32 & 0x1f) << 16);
    // SAFETY: fixed, valid MDIO MMIO.
    unsafe {
        write_volatile(MDIO_BASE as *mut u32, cmd);
        write_volatile((MDIO_BASE + 4) as *mut u32, val as u32);
        write_volatile(MDIO_BASE as *mut u32, cmd | MDIO_GO);
    }
    mdio_wait()
}

// ---- multicast hash (CRC-32, MSB-first) ----

/// Ethernet CRC-32 over `data` (MSB-first, init all-ones, no final inversion —
/// matches the reference's `ether_crc`).
pub fn ether_crc(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        let mut cur = byte;
        for _ in 0..8 {
            let msb = (crc >> 31) & 1;
            let xor = msb ^ (cur as u32 & 1);
            crc <<= 1;
            if xor != 0 {
                crc ^= ETHER_CRC_POLY;
            }
            cur >>= 1;
        }
    }
    crc
}

/// Multicast hash bit for a 6-byte address: top 6 bits of the CRC (`crc >> 26`).
pub fn mcast_hash_bit(addr: &[u8; 6]) -> u32 {
    ether_crc(addr) >> 26
}
