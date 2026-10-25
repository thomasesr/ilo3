//! Virtual USB device set — Phase 7 vUSB glue (docs/29).
//!
//! Holds the always-connected HID keyboard + mouse and the dynamic mass-storage
//! device, and the two data producers that feed them:
//! * the **KVM input stream** (rfb.rs KeyEvent/PointerEvent) → HID reports;
//! * the **UHCI walker** (uhci.rs) → control/bulk transfer dispatch.
//!
//! The HID→report path is real + testable; the UHCI transfer dispatch (enumeration
//! + OUT data) is a scaffold — CONFIRM-ON-HW (docs/29 Open items). Clean-room from
//! USB HID / UHCI / BOT specs. No HP code copied.

#![allow(dead_code)]

use alloc::vec::Vec;

use crate::hid::{self, HidKeyboard};
use crate::uhci::Td;
use crate::usb_msc::{self, Cbw};
use crate::vmedia::{Image, Kind};

/// USB PIDs (UHCI token low byte).
const PID_SETUP: u8 = 0x2D;
const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xE1;

/// The virtual device set behind the emulated UHCI controller.
pub struct VUsb {
    kbd: HidKeyboard,
    last_x: i16,
    last_y: i16,
    msc: Image,
    cbw: Option<Cbw>,
}

impl VUsb {
    pub fn new() -> Self {
        VUsb {
            kbd: HidKeyboard::new(),
            last_x: 0,
            last_y: 0,
            msc: Image::empty(Kind::Cdrom),
            cbw: None,
        }
    }

    pub fn keyboard(&mut self) -> &mut HidKeyboard {
        &mut self.kbd
    }

    pub fn mass_storage(&mut self) -> &mut Image {
        &mut self.msc
    }

    /// KVM KeyEvent → keyboard report (RFB keysym → HID usage).
    pub fn on_key(&mut self, down: bool, keysym: u32) {
        if let Some(k) = hid::keysym_to_key(keysym) {
            if down {
                self.kbd.press(k);
            } else {
                self.kbd.release(k);
            }
        }
    }

    /// KVM PointerEvent → relative mouse delta (absolute coords differenced).
    pub fn on_pointer(&mut self, buttons: u8, x: u16, y: u16) -> [u8; 3] {
        let dx = (x as i16 - self.last_x).clamp(-127, 127) as i8;
        let dy = (y as i16 - self.last_y).clamp(-127, 127) as i8;
        self.last_x = x as i16;
        self.last_y = y as i16;
        hid::mouse_report(buttons, dx, dy)
    }

    pub fn keyboard_report(&self) -> [u8; 8] {
        self.kbd.report()
    }

    /// Route one UHCI TD to a virtual device; returns IN-phase data (or None for
    /// OUT/no-data). Endpoint mapping (CONFIRM-ON-HW) per docs/29: EP0 control,
    /// EP1 keyboard IN, EP2 mouse IN, EP3/4 bulk for mass storage.
    pub fn dispatch(&mut self, td: &Td) -> Option<Vec<u8>> {
        match (td.pid(), td.endpoint()) {
            (PID_SETUP, 0) => Some(control_setup(td)),
            (PID_IN, 1) => Some(self.kbd.report().to_vec()),
            (PID_IN, 2) => Some(hid::mouse_report(0, 0, 0).to_vec()),
            (PID_OUT, _) => {
                // DATA-OUT: consumed by the walker's write path (CBW / WRITE data).
                None
            }
            (PID_IN, ep) if ep >= 3 => self.mass_storage_in(td),
            _ => None,
        }
    }

    /// Mass-storage bulk-IN: data phase (CSW or READ data) for the pending CBW.
    fn mass_storage_in(&mut self, _td: &Td) -> Option<Vec<u8>> {
        // Full BOT sequencing (CBW → data → CSW) rides the walker's OUT/IN
        // alternation — scaffolded; usb_msc::handle/build_csw carry the logic.
        let _ = self.cbw.as_ref();
        None
    }
}

impl Default for VUsb {
    fn default() -> Self {
        Self::new()
    }
}

/// Control-transfer (EP0 SETUP) handler — USB chapter 9 enumeration. Stubbed:
/// the device/configuration/string descriptors + SET_ADDRESS etc. are standard
/// but large; deferred (CONFIRM-ON-HW, docs/29). Returns empty for now.
fn control_setup(_td: &Td) -> Vec<u8> {
    use usb_msc::SCS_OK;
    let _ = SCS_OK;
    Vec::new()
}
