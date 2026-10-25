//! KVM remote-console task — Phase 7 (docs/46).
//!
//! The data path: smoltcp TcpSocket → TLS 1.3 (`TlsStream`) → WebSocket
//! (`WsStream`) → RFB (`RfbSession`). Serves a software-rendered framebuffer now;
//! the DVC capture (`capture.rs`) slots in once brought up on hardware.
//!
//! Port: 6080 (noVNC's default; the stock IRC used 17990, docs/45). A separate
//! listener from HTTPS for now — consolidating WS upgrade onto the 443 listener is
//! a follow-up. KVM is single-active (docs/45), so the framebuffer is a leaked
//! `&'static mut` reused across sequential sessions (no aliasing). Privilege gate
//! (`REMOTE_CONSOLE`) is enforced at the web session layer that launches KVM.

#![allow(dead_code)]

use alloc::boxed::Box;

use crate::certstore;
use crate::framebuf::Framebuffer;
use crate::rfb::{NullInput, RfbSession};
use crate::tcp::{self, TcpStream};
use crate::tls_handshake::ServerHandshake;
use crate::tls_stream::TlsStream;
use crate::uart::Uart;
use crate::users::Privileges;
use crate::webauth;
use crate::ws::WsStream;
use crate::x509;

const FB_W: usize = 1024;
const FB_H: usize = 768;
const KVM_PORT: u16 = 6080;

const ANCHORS: &[&[u8]] = &[];

/// Draw a placeholder pattern (gradient + a few solid blocks) so the first
/// FramebufferUpdate delivers a visible frame before capture exists.
fn draw_pattern(fb: &mut Framebuffer) {
    for y in 0..FB_H {
        let band = (y / 16) as u16;
        let shade = (band * 0x11) & 0x1F;
        let p = (shade << 11) | (shade << 6) | shade; // grey ramp
        for x in 0..FB_W {
            fb.set_pixel(x, y, p);
        }
    }
    fb.fill_rect(64, 64, 256, 48, 0x001F); // blue block
    fb.fill_rect(64, 144, 256, 48, 0x07E0); // green block
    fb.fill_rect(64, 224, 256, 48, 0xF800); // red block
    fb.mark_all_dirty();
}

/// Serve KVM: accept → TLS → WebSocket → RFB, one session at a time.
#[embassy_executor::task]
pub async fn kvm_task() {
    let con = Uart::console();

    // Fail closed: no TLS identity → no KVM (mirrors web.rs).
    let Some(store) = certstore::CertStore::load_from_flash() else {
        con.write_bytes(b"[kvm] no TLS cert provisioned - KVM disabled\n");
        return;
    };
    let Some(seed) = x509::ed25519_seed_from_pkcs8(&store.key_der) else {
        con.write_bytes(b"[kvm] cert store key invalid - KVM disabled\n");
        return;
    };
    let cert: &'static [u8] = Box::leak(store.cert_der.into_boxed_slice());

    // Single-active KVM: one leaked framebuffer reused across sequential sessions.
    let fb: &'static mut Framebuffer = Box::leak(Box::new(Framebuffer::new(FB_W, FB_H)));
    draw_pattern(fb);

    let listener = tcp::add_tcp();
    tcp::with_sockets(|s| {
        use smoltcp::socket::tcp::Socket as TcpSocket;
        let sock = s.get_mut::<TcpSocket>(listener);
        let _ = sock.listen(KVM_PORT);
    });
    con.write_bytes(b"[kvm] RFB/WebSocket KVM listening on :6080\n");

    loop {
        let conn = tcp::accept(listener).await;
        let stream = TcpStream::new(conn);

        let (eph, _) = crate::crypto::x25519_keypair();
        let hs = ServerHandshake::new(cert, &seed, ANCHORS, eph);

        let ok = match TlsStream::handshake(stream, hs).await {
            Ok(tls) => match WsStream::accept(tls).await {
                Ok(ws) => {
                    // Authenticate the console: the session token (which must carry
                    // REMOTE_CONSOLE) rides the WebSocket URL (?token=...). Server-
                    // auth-only TLS does NOT authenticate the client, so this check
                    // is the access boundary for the KVM listener.
                    let authed = ws
                        .token()
                        .and_then(webauth::privs_for_token)
                        .is_some_and(|p| p.contains(Privileges::REMOTE_CONSOLE));
                    if !authed {
                        con.write_bytes(b"[kvm] unauthenticated console attempt - rejected\n");
                        false
                    } else {
                        match RfbSession::handshake(ws, fb, NullInput).await {
                            Ok(sess) => sess.run().await.is_ok(),
                            Err(_) => false,
                        }
                    }
                }
                Err(_) => false,
            },
            Err(_) => false,
        };
        let _ = ok;

        // Re-arm the listener for the next connection.
        tcp::with_sockets(|s| {
            use smoltcp::socket::tcp::Socket as TcpSocket;
            let sock = s.get_mut::<TcpSocket>(listener);
            sock.abort();
            let _ = sock.listen(KVM_PORT);
        });
    }
}
