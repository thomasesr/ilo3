//! Networking — smoltcp TCP/IP over the GXE MAC (Phase 3).
//!
//! The TCP/IP stack is fully built here; it drives the MAC through the `MacRing`
//! seam. The GXE MAC's TX/RX DMA ring is NOT statically recoverable (docs/11 —
//! RE-confirmed: the datapath goes through INTEGRITY DMA object methods, not raw
//! descriptor registers), so `StubRing` is a placeholder that yields no RX and
//! drops TX. Once the real ring is pinned on hardware, implement `MacRing` for it
//! and the whole stack (DHCP, UDP, TCP) comes alive unchanged.
//!
//! MDIO + MAC config (reset/addr/hash) are usable now via crate::mac.

#![allow(dead_code)]

use embassy_time::{Duration, Timer};
use smoltcp::iface::{Config, Interface};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::dhcpv4;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpCidr};

use crate::timer;

const MTU: usize = 1536;

/// The MAC datapath seam. A real implementation moves Ethernet frames through
/// the GXE DMA ring; `StubRing` stands in until that ring is reconstructed on HW.
pub trait MacRing {
    /// Pull one received frame into `buf`; return its length, or None if empty.
    fn receive(&mut self, buf: &mut [u8]) -> Option<usize>;
    /// Queue one frame for transmit; return whether it was accepted.
    fn transmit(&mut self, frame: &[u8]) -> bool;
}

/// HW-gated placeholder (docs/11): no RX, TX dropped. Replace on hardware.
pub struct StubRing;
impl MacRing for StubRing {
    fn receive(&mut self, _buf: &mut [u8]) -> Option<usize> {
        None
    }
    fn transmit(&mut self, _frame: &[u8]) -> bool {
        false
    }
}

/// smoltcp phy device over a `MacRing`.
pub struct GxeDevice<R: MacRing> {
    ring: R,
    rx: [u8; MTU],
    tx: [u8; MTU],
}

impl<R: MacRing> GxeDevice<R> {
    pub fn new(ring: R) -> Self {
        GxeDevice { ring, rx: [0; MTU], tx: [0; MTU] }
    }
}

pub struct RxToken<'a>(&'a mut [u8]);
pub struct TxToken<'a, R: MacRing>(&'a mut [u8; MTU], &'a mut R);

impl<'a> phy::RxToken for RxToken<'a> {
    fn consume<Res, F: FnOnce(&[u8]) -> Res>(self, f: F) -> Res {
        f(self.0)
    }
}

impl<'a, R: MacRing> phy::TxToken for TxToken<'a, R> {
    fn consume<Res, F: FnOnce(&mut [u8]) -> Res>(self, len: usize, f: F) -> Res {
        let r = f(&mut self.0[..len]);
        self.1.transmit(&self.0[..len]); // stub drops; real ring sends
        r
    }
}

impl<R: MacRing> Device for GxeDevice<R> {
    type RxToken<'a> = RxToken<'a> where Self: 'a;
    type TxToken<'a> = TxToken<'a, R> where Self: 'a;

    fn receive(&mut self, _t: Instant) -> Option<(RxToken<'_>, TxToken<'_, R>)> {
        let n = self.ring.receive(&mut self.rx)?;
        // Disjoint field borrows: rx for the RxToken, tx + ring for the TxToken.
        Some((RxToken(&mut self.rx[..n]), TxToken(&mut self.tx, &mut self.ring)))
    }

    fn transmit(&mut self, _t: Instant) -> Option<TxToken<'_, R>> {
        Some(TxToken(&mut self.tx, &mut self.ring))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = MTU;
        caps.max_burst_size = Some(1);
        caps.medium = Medium::Ethernet;
        caps
    }
}

/// smoltcp monotonic time from the hardware counter (1 MHz → ms).
fn now() -> Instant {
    Instant::from_millis((timer::now_ticks() / 1000) as i64)
}

/// Placeholder station MAC (locally-administered). Real address = NVRAM/SDR.
pub const MAC_ADDR: [u8; 6] = [0x02, 0x00, 0x00, 0x11, 0x03, 0x00];

/// Networking task: bring up the interface, run DHCP, poll the stack. Until the
/// MAC ring is real (docs/11) this runs but passes no traffic — the stack, DHCP
/// client and socket plumbing are validated structurally, ready for the ring.
#[embassy_executor::task]
pub async fn net_task() {
    let mut device = GxeDevice::new(StubRing);
    crate::mac::Mac::mac0().set_addr(&MAC_ADDR);

    let hw = HardwareAddress::Ethernet(EthernetAddress(MAC_ADDR));
    let config = Config::new(hw);
    let mut iface = Interface::new(config, &mut device, now());

    // Sockets live in the shared set (crate::tcp) so the web task and this poll
    // loop both drive them.
    let dhcp = crate::tcp::add_socket(dhcpv4::Socket::new());

    loop {
        crate::tcp::with_sockets(|sockets| {
            iface.poll(now(), &mut device, sockets);

            // Apply DHCP results to the interface addresses/routes.
            if let Some(event) = sockets.get_mut::<dhcpv4::Socket>(dhcp).poll() {
                match event {
                    dhcpv4::Event::Configured(cfg) => {
                        iface.update_ip_addrs(|a| {
                            a.clear();
                            let _ = a.push(IpCidr::Ipv4(cfg.address));
                        });
                        if let Some(gw) = cfg.router {
                            let _ = iface.routes_mut().add_default_ipv4_route(gw);
                        }
                    }
                    dhcpv4::Event::Deconfigured => {
                        iface.update_ip_addrs(|a| a.clear());
                        iface.routes_mut().remove_default_ipv4_route();
                    }
                }
            }
        });

        Timer::after(Duration::from_millis(100)).await;
    }
}
