//! UID-button-hold → iLO self-reset (host-independent reset for recovery).
//!
//! Holding the front-panel UID button for `HOLD_SECS` triggers a soft reset
//! (src/reset.rs): it re-runs the SPL boot chain + its recovery window while the
//! host stays up (a soft reset keeps the PCI link enumerated). This is the
//! physical escape hatch when main is running but its host channels are broken —
//! the operator re-enters SPL recovery on demand, with NO AC cycle (which on a
//! ProLiant would also drop the host, since iLO is on standby power). See docs/49.
//!
//! SAFETY/UX: the UID button register/bit are CONFIRM-ON-HW (src/cpld.rs). To make
//! a mis-identified bit non-dangerous, the monitor ARMS only after it has first
//! seen the button RELEASED: if the chosen bit is wrong and reads permanently
//! asserted, the monitor never arms (no boot-reset loop) rather than resetting
//! every 10 s. Once the bit is pinned live, this still behaves identically.

use embassy_time::{Duration, Timer};

use crate::{cpld, dbglog, reset};

const POLL_MS: u64 = 100;
const HOLD_SECS: u64 = 10;
const NEED: u32 = (HOLD_SECS * 1000 / POLL_MS) as u32; // consecutive pressed polls

#[embassy_executor::task]
pub async fn uid_reset_task() {
    // Require an initial release before arming (defuses a stuck/mis-identified bit).
    let mut armed = false;
    let mut held: u32 = 0;
    loop {
        let pressed = cpld::uid_button_pressed();
        if !armed {
            if !pressed {
                armed = true;
            }
        } else if pressed {
            held += 1;
            if held >= NEED {
                dbglog::log_bytes(b"[uid] held 10s -> iLO soft reset\n");
                // SAFETY: deliberate, operator-requested reset.
                unsafe { reset::soft_reset() }
            }
        } else {
            held = 0; // released early -> a fresh continuous hold is required
        }
        Timer::after(Duration::from_millis(POLL_MS)).await;
    }
}
