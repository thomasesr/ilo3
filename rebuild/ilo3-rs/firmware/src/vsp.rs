//! VSP / SOL text console — Phase 7a (docs/45, docs/34).
//!
//! The cheap "remote console" that ships before KVM: a line-protocol bridge from
//! the host's serial console (LPC COM ports, IRQ 40-42, docs/12) to a TCP
//! listener (default port 3002, docs/45). `vsp` / `textcons` CLI verbs (clp.rs)
//! enter passthrough; the SSH/TCP layer streams the bridged bytes.
//!
//! Both halves are HW-gated: the host-UART transport rides CHIF/LPC (docs/28), and
//! the TCP listener rides the MAC ring (docs/11). This is a structural scaffold.
//! Clean-room: line protocol is trivial; `docs/45-irc-p84-protocol.md` (VSP notes).
//! No HP code copied.

#![allow(dead_code)]

/// Default VSP/TEXTCONS listener port (docs/45).
pub const VSP_PORT: u16 = 3002;

/// LPC COM IRQ lines (docs/12: IODLPCCOM0-2).
pub const LPCCOM_IRQS: [u32; 3] = [40, 41, 42];

/// Host-serial RX: read pending bytes from the LPC COM port into `buf`.
/// CONFIRM-ON-HW — the LPC/CHIF host-serial register path (docs/28) is not yet
/// mapped to MMIO; returns 0 (no data) until brought up on hardware.
pub fn read_host_serial(buf: &mut [u8]) -> usize {
    let _ = buf;
    0
}

/// Host-serial TX: write bytes out to the LPC COM port.
/// CONFIRM-ON-HW — see read_host_serial.
pub fn write_host_serial(_data: &[u8]) {}

/// Serve VSP: accept a connection and bridge host-serial ↔ socket. Structural —
/// not spawned until the LPC serial path + MAC ring are up (HW-gated).
#[embassy_executor::task]
pub async fn vsp_task() {
    // The real bridge loops: read_host_serial → socket, socket → write_host_serial.
    // Not spawned yet; see module docs.
    core::future::pending::<()>().await;
}
