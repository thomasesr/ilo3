//! Virtual USB HID keyboard + mouse — Phase 7 vUSB (docs/29).
//!
//! Builds USB HID boot-protocol reports (the "always-connected" keyboard/mouse
//! the host enumerates on the virtual UHCI). The RFB KeyEvent/PointerEvent stream
//! (rfb.rs) feeds these; the UHCI walker (uhci.rs) delivers them to the host.
//!
//! Clean-room from the USB HID spec + HID Usage Tables (keyboard/mouse). RFB
//! keysyms (X11) map to HID usages via a focused subset. Provenance:
//! `docs/29-vusb-vmedia.md`. No HP code copied.

#![allow(dead_code)]

/// A decoded input key, either a modifier bit or a key usage.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Modifier(u8), // bit in the report modifier byte
    Usage(u8),    // key usage (report slots 2..7)
}

// Modifier bits (HID boot protocol).
pub const M_CTRL: u8 = 0x01;
pub const M_SHIFT: u8 = 0x02;
pub const M_ALT: u8 = 0x04;
pub const M_GUI: u8 = 0x08;
pub const M_RCTRL: u8 = 0x10;
pub const M_RSHIFT: u8 = 0x20;
pub const M_RALT: u8 = 0x40;

/// Map an RFB/X11 keysym to a keyboard key (subset — common keys only).
pub fn keysym_to_key(keysym: u32) -> Option<Key> {
    Some(match keysym {
        0x20 => Key::Usage(0x2C), // space
        0x0D | 0xFF0D => Key::Usage(0x28), // return
        0x1B | 0xFF1B => Key::Usage(0x29), // escape
        0x09 | 0xFF09 => Key::Usage(0x2B), // tab
        0x08 | 0xFF08 => Key::Usage(0x2A), // backspace
        0xFFFF => Key::Usage(0x4C),        // delete
        0xFF51 => Key::Usage(0x50),        // left
        0xFF52 => Key::Usage(0x52),        // up
        0xFF53 => Key::Usage(0x4F),        // right
        0xFF54 => Key::Usage(0x51),        // down
        0xFFE1 => Key::Modifier(M_SHIFT),
        0xFFE2 => Key::Modifier(M_RSHIFT),
        0xFFE3 => Key::Modifier(M_CTRL),
        0xFFE4 => Key::Modifier(M_RCTRL),
        0xFFE9 => Key::Modifier(M_ALT),
        0xFFEA => Key::Modifier(M_RALT),
        _ if (0x31..=0x39).contains(&keysym) => Key::Usage(0x1E + (keysym - 0x31) as u8), // 1-9
        0x30 => Key::Usage(0x27),                                                        // 0
        _ if (0x61..=0x7A).contains(&keysym) => Key::Usage(0x04 + (keysym - 0x61) as u8), // a-z
        _ if (0x41..=0x5A).contains(&keysym) => Key::Usage(0x04 + (keysym - 0x41) as u8), // A-Z
        _ if (0xFFBE..=0xFFC9).contains(&keysym) => Key::Usage(0x3A + (keysym - 0xFFBE) as u8), // F1-F12
        _ => return None,
    })
}

/// A 6-key rollover keyboard report builder.
#[derive(Default)]
pub struct HidKeyboard {
    mods: u8,
    keys: [u8; 6],
}

impl HidKeyboard {
    pub fn new() -> Self {
        Self { mods: 0, keys: [0; 6] }
    }

    pub fn press(&mut self, k: Key) {
        match k {
            Key::Modifier(m) => self.mods |= m,
            Key::Usage(u) => {
                if !self.keys.contains(&u) {
                    if let Some(slot) = self.keys.iter_mut().find(|s| **s == 0) {
                        *slot = u;
                    }
                }
            }
        }
    }

    pub fn release(&mut self, k: Key) {
        match k {
            Key::Modifier(m) => self.mods &= !m,
            Key::Usage(u) => {
                if let Some(slot) = self.keys.iter_mut().find(|s| **s == u) {
                    *slot = 0;
                }
            }
        }
    }

    /// The 8-byte boot-protocol keyboard report.
    pub fn report(&self) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[0] = self.mods;
        r[2..8].copy_from_slice(&self.keys);
        r
    }
}

/// A 3-byte boot-protocol mouse report (buttons + signed X/Y deltas).
pub fn mouse_report(buttons: u8, dx: i8, dy: i8) -> [u8; 3] {
    [buttons, dx as u8, dy as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_keys() {
        assert_eq!(keysym_to_key(0x61), Some(Key::Usage(0x04))); // 'a'
        assert_eq!(keysym_to_key(0x7A), Some(Key::Usage(0x1D))); // 'z'
        assert_eq!(keysym_to_key(0x31), Some(Key::Usage(0x1E))); // '1'
        assert_eq!(keysym_to_key(0xFFE1), Some(Key::Modifier(M_SHIFT)));
        assert_eq!(keysym_to_key(0x1234), None);
    }

    #[test]
    fn keyboard_report() {
        let mut k = HidKeyboard::new();
        k.press(Key::Modifier(M_SHIFT));
        k.press(Key::Usage(0x04)); // 'a'
        k.press(Key::Usage(0x04)); // dup — ignored
        let r = k.report();
        assert_eq!(r[0], M_SHIFT);
        assert_eq!(r[2], 0x04);
        k.release(Key::Usage(0x04));
        assert_eq!(k.report()[2], 0);
    }
}
