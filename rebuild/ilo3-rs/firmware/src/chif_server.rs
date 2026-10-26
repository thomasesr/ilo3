//! CHIF server — channel→service routing (Phase 6, docs/28 §5).
//!
//! Drives the CHIF transport (chif.rs): polls host→BMC messages, routes each by
//! channel to a service, and sends the response back + rings the host doorbell.
//! The main host-OS use is IPMI-over-PCI (IPMIPCISR) → the ipmi core; an echo
//! channel aids bring-up; vMedia/health are later services.
//!
//! Channel ids are host-assigned (docs/28), so the routing convention here
//! (0xFFFE = echo, else IPMI) is a best-reading placeholder — CONFIRM-ON-HW with
//! the host hpilo driver. HW-gated (needs host PCI + the shared-mem window), so
//! the task is defined but not spawned yet.

#![allow(dead_code)]

use alloc::vec::Vec;

use embassy_time::{Duration, Timer};

use crate::chif::Chif;
use crate::ipmi::{self, Request};

/// BMC-reserved echo channel (docs/28: 0xFFFE = echo, 0xFFFF = free).
const CH_ECHO: u16 = 0xFFFE;

/// Route one received message to its service and build the response bytes.
fn route(channel: u16, data: &[u8]) -> Vec<u8> {
    match channel {
        CH_ECHO => data.to_vec(),
        _ => ipmi_over_chif(data), // default host path = IPMI-over-PCI
    }
}

/// Decode an IPMI request carried over CHIF, dispatch it, encode the response.
/// Wrapper framing (IPMIPCISR) is the host hpilo convention — CONFIRM-ON-HW; here
/// the payload is taken as a bare [NetFn/LUN][Cmd][data] request.
fn ipmi_over_chif(data: &[u8]) -> Vec<u8> {
    if data.len() < 2 {
        return Vec::new();
    }
    let netfn = data[0] >> 2;
    let lun = data[0] & 0x03;
    let cmd = data[1];
    let resp = ipmi::dispatch(&Request { netfn, lun, cmd, data: &data[2..] });
    let mut out = Vec::with_capacity(3 + resp.data.len());
    out.push(((netfn | 1) << 2) | lun);
    out.push(cmd);
    out.push(resp.cc);
    out.extend_from_slice(&resp.data);
    out
}

/// The CHIF server task: bring up the transport, open the echo + IPMI channels,
/// then poll/route/respond forever. Not spawned until the host interface is
/// validated on hardware (docs/28 window acquisition).
#[embassy_executor::task]
pub async fn chif_task() {
    let mut chif = Chif::new();
    // window_phys / log2(size) come from the platform (host PCI BAR2, docs/28).
    chif.init(); // 1 MB window; BAR size encoded per HP (docs/28 §10)
    let _ = chif.open_channel(CH_ECHO);

    let mut buf = [0u8; 0x4000];
    loop {
        if let Some(rx) = chif.poll_rx(&mut buf) {
            let resp = route(rx.channel, &buf[..rx.len]);
            chif.send(rx.channel, rx.ccb, &resp);
            chif.rx_release(rx.ccb);
        } else {
            Timer::after(Duration::from_millis(5)).await;
        }
    }
}
