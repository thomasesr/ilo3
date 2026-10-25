//! Virtual UHCI host controller — Phase 7 vUSB (docs/29).
//!
//! The host sees a software-emulated UHCI (USB 1.1) controller on PCI fn .4; the
//! BMC *walks the host's frame-list + TD/QH schedule* (over the CHIF shared
//! window), executes each transfer against the virtual devices (hid.rs /
//! usb_msc.rs), and writes TD status back. This module encodes the register file,
//! the TD/QH structures + token/status decode, and the walk skeleton.
//!
//! Clean-room from the Intel UHCI 1.1 spec (public). The host-memory access seam
//! (`HostMem`) and register base are CONFIRM-ON-HW — the BSP IODevice config table
//! holds the HC register base (docs/29 Open items). No HP code copied.

#![allow(dead_code)]

/// UHCI register offsets (Intel UHCI 1.1 §4).
pub mod reg {
    pub const USBCMD: usize = 0x00; // u16
    pub const USBSTS: usize = 0x02; // u16
    pub const USBINTR: usize = 0x04; // u16
    pub const FRNUM: usize = 0x06; // u16
    pub const FRBASEADD: usize = 0x08; // u32
    pub const SOFMOD: usize = 0x0C; // u8
    pub const PORTSC1: usize = 0x10; // u16
    pub const PORTSC2: usize = 0x12; // u16
}

/// USBCMD bits.
pub mod cmd {
    pub const RUN: u16 = 0x0001;
    pub const HCRESET: u16 = 0x0002;
    pub const GLOBAL_RESUME: u16 = 0x0010;
    pub const EN_MAX_PKT: u16 = 0x0080;
}

/// USBSTS bits.
pub mod sts {
    pub const HCHALTED: u16 = 0x0020;
    pub const INTERRUPT: u16 = 0x0001;
}

/// TD sizes + link-pointer bits.
pub const TD_LEN: usize = 32;
pub const QH_LEN: usize = 16;
pub const LINK_TD: u32 = 0x0000_0000; // bit0=0
pub const LINK_QH: u32 = 0x0000_0002; // bit1=1
pub const LINK_DEPTH_FIRST: u32 = 0x0000_0004;
pub const LINK_TERMINATE: u32 = 0x0000_0001;

/// A UHCI transfer descriptor (Intel UHCI 1.1 §3.2.2).
pub struct Td {
    pub link: u32,
    pub status: u32,
    pub token: u32,
    pub buffer: u32,
}

impl Td {
    pub fn active(&self) -> bool {
        self.status & 0x0080_0000 != 0
    }
    pub fn pid(&self) -> u8 {
        (self.token & 0xFF) as u8
    }
    pub fn device(&self) -> u8 {
        ((self.token >> 8) & 0x7F) as u8
    }
    pub fn endpoint(&self) -> u8 {
        ((self.token >> 15) & 0x0F) as u8
    }
    pub fn data_toggle(&self) -> bool {
        self.token & (1 << 19) != 0
    }
    pub fn max_len(&self) -> usize {
        ((self.token >> 21) & 0x7FF) as usize + 1
    }
    pub fn is_setup(&self) -> bool {
        self.pid() == 0x2D
    }
}

/// Host-memory access over the CHIF shared window (0x40000000, docs/06/28).
/// Addresses are host-physical; the window remap is CONFIRM-ON-HW.
pub trait HostMem {
    fn read(&mut self, addr: u32, buf: &mut [u8]);
    fn write(&mut self, addr: u32, buf: &[u8]);
}

/// Emulated UHCI register file (host writes are trapped into this shadow).
#[derive(Default)]
pub struct Uhci {
    cmd: u16,
    sts: u16,
    intr: u16,
    frame_base: u32,
    portsc1: u16,
    portsc2: u16,
}

impl Uhci {
    pub fn new() -> Self {
        Self {
            sts: sts::HCHALTED, // halted until RUN is set
            ..Default::default()
        }
    }

    pub fn read_reg(&self, off: usize) -> u32 {
        match off {
            reg::USBCMD => self.cmd as u32,
            reg::USBSTS => self.sts as u32,
            reg::USBINTR => self.intr as u32,
            reg::FRNUM => 0, // current frame (not modelled)
            reg::FRBASEADD => self.frame_base,
            reg::PORTSC1 => self.portsc1 as u32,
            reg::PORTSC2 => self.portsc2 as u32,
            _ => 0,
        }
    }

    pub fn write_reg(&mut self, off: usize, val: u32) {
        match off {
            reg::USBCMD => {
                let v = val as u16;
                if v & cmd::HCRESET != 0 {
                    self.cmd = 0;
                    self.sts = sts::HCHALTED;
                } else {
                    self.cmd = v;
                    if v & cmd::RUN != 0 {
                        self.sts &= !sts::HCHALTED;
                    }
                }
            }
            reg::USBSTS => {
                // Writes clear the status bits they set (RW1C).
                self.sts &= !(val as u16 & 0x3F);
            }
            reg::USBINTR => self.intr = val as u16,
            reg::FRBASEADD => self.frame_base = val & 0xFFFF_F000,
            reg::PORTSC1 => self.portsc1 = val as u16,
            reg::PORTSC2 => self.portsc2 = val as u16,
            _ => {}
        }
    }

    pub fn running(&self) -> bool {
        self.cmd & cmd::RUN != 0
    }

    /// Walk the host frame list, calling `on_td` for each active TD; the callback
    /// returns the data to write back to the TD's buffer for IN transfers.
    /// The exact frame-list/TD walk + short-packet handling is CONFIRM-ON-HW.
    pub fn walk(
        &mut self,
        host: &mut dyn HostMem,
        mut on_td: impl FnMut(&Td) -> Option<alloc::vec::Vec<u8>>,
    ) {
        if !self.running() {
            return;
        }
        for frame in 0..1024u32 {
            let mut ptr = self.frame_base + frame * 4;
            for _depth in 0..32 {
                let mut p = [0u8; 4];
                host.read(ptr, &mut p);
                let link = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
                if link & LINK_TERMINATE != 0 {
                    break;
                }
                if link & LINK_QH == 0 {
                    // TD — decode + dispatch.
                    let mut b = [0u8; TD_LEN];
                    host.read(link & !0xF, &mut b);
                    let td = Td {
                        link: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                        status: u32::from_le_bytes([b[4], b[5], b[6], b[7]]),
                        token: u32::from_le_bytes([b[8], b[9], b[10], b[11]]),
                        buffer: u32::from_le_bytes([b[12], b[13], b[14], b[15]]),
                    };
                    if td.active() {
                        if let Some(data) = on_td(&td) {
                            host.write(td.buffer, &data);
                        }
                        // CONFIRM-ON-HW: write back TD status (clear active, set
                        // actual-length, error bits) to complete the transfer.
                    }
                    ptr = td.link & !0xF;
                } else {
                    // QH — follow the element link pointer (offset 4).
                    let mut b = [0u8; 4];
                    host.read(link & !0xF, &mut b);
                    ptr = u32::from_le_bytes(b) & !0xF;
                }
                if ptr == 0 {
                    break;
                }
            }
        }
    }
}
