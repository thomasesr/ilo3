//! HTTPS web server: picoserve over smoltcp TCP + our TLS 1.3 stream (Phase 5).
//!
//! The data path is: smoltcp TcpSocket (tcp.rs) → TlsStream (tls_stream.rs) →
//! picoserve. picoserve does no crypto — it sees the decrypted plaintext HTTP
//! stream. Handlers are hand-rolled JSON (lean, no serde derive in the hot path);
//! grow them to the 58-name `json/*` contract in docs/33.

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;
use core::future::Future;

use embedded_io_async::{ErrorType, Read, Write as _};
use picoserve::AppBuilder;
use picoserve::response::StatusCode;
use picoserve::routing::{get, post};
use picoserve::time::Timer;

use crate::tcp::{self, TcpStream};
use crate::tls_handshake::ServerHandshake;
use crate::tls_stream::{TlsStream, TlsStreamError};
use crate::uart::Uart;
use crate::users::Privileges;
use crate::webauth::{self, Auth};
use crate::{certstore, x509};

/// Trust anchors for optional mTLS client-cert auth. Empty = server-auth only
/// (browsers present no client cert); populate from a flash anchor store to
/// require + verify client certs. Must be CA certs (x509::verify_chain enforces).
static ANCHORS: &[&[u8]] = &[];

/// picoserve async-runtime marker.
pub struct WebRuntime;

/// Timer backed by embassy-time.
pub struct WebTimer;

impl Timer<WebRuntime> for WebTimer {
    fn delay(&self, duration: picoserve::time::Duration) -> impl Future<Output = ()> {
        embassy_time::Timer::after(embassy_time::Duration::from_millis(duration.as_millis()))
    }
}

// ---- handlers (hand-rolled JSON, wired to the HAL) ----

async fn json_overview(auth: Auth) -> Result<String, StatusCode> {
    auth.require(Privileges::LOGIN)?;
    let (maj, min) = crate::cpld::version();
    Ok(format!(
        "{{\"name\":\"ilo3-rs\",\"version\":\"0.0.1\",\"cpld\":\"{}.{}\"}}\n",
        maj, min
    ))
}

async fn json_health_summary(auth: Auth) -> Result<String, StatusCode> {
    auth.require(Privileges::LOGIN)?;
    let mut sensors = String::new();
    for (i, s) in crate::sensors::TABLE.iter().enumerate() {
        if i > 0 {
            sensors.push(',');
        }
        match s.read() {
            Ok(t) => sensors.push_str(&format!("{{\"name\":\"{}\",\"temp\":{}}}", s.name, t)),
            Err(_) => sensors.push_str(&format!("{{\"name\":\"{}\",\"temp\":null}}", s.name)),
        }
    }
    let mut psus = String::new();
    for (i, p) in crate::psu::BAYS.iter().enumerate() {
        if i > 0 {
            psus.push(',');
        }
        let present = p.present();
        match p.health() {
            Ok(h) => psus.push_str(&format!(
                "{{\"bay\":{},\"present\":{},\"health\":{}}}",
                p.bay, present, h
            )),
            Err(_) => psus.push_str(&format!("{{\"bay\":{},\"present\":{}}}", p.bay, present)),
        }
    }
    Ok(format!("{{\"sensors\":[{}],\"psus\":[{}]}}\n", sensors, psus))
}

async fn json_host_power(auth: Auth) -> Result<String, StatusCode> {
    auth.require(Privileges::LOGIN)?;
    let state = match crate::power::state() {
        crate::power::State::On => "on",
        crate::power::State::Off => "off",
        crate::power::State::InReset => "in_reset",
    };
    Ok(format!("{{\"state\":\"{}\"}}\n", state))
}

async fn json_network(auth: Auth) -> Result<String, StatusCode> {
    auth.require(Privileges::LOGIN)?;
    let m = crate::net::MAC_ADDR;
    Ok(format!(
        "{{\"mac\":\"{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}\"}}\n",
        m[0], m[1], m[2], m[3], m[4], m[5]
    ))
}

async fn json_sessions(auth: Auth) -> Result<&'static str, StatusCode> {
    auth.require(Privileges::LOGIN)?;
    Ok("{\"sessions\":[]}\n")
}

/// The router app (built once; `impl Trait` in assoc type needs nightly).
struct App;

impl picoserve::AppBuilder for App {
    type PathRouter = impl picoserve::routing::PathRouter;

    fn build_app(self) -> picoserve::Router<Self::PathRouter> {
        picoserve::Router::new()
            .route("/json/login", post(webauth::login))
            .route("/json/logout", post(webauth::logout))
            .route("/json/overview", get(json_overview))
            .route("/json/health_summary", get(json_health_summary))
            .route("/json/host_power", get(json_host_power))
            .route("/json/network", get(json_network))
            .route("/json/sessions", get(json_sessions))
    }
}

// ---- picoserve Socket impl over the TLS stream ----

type Tls = TlsStream<'static, TcpStream>;

pub struct TlsSocket(Rc<RefCell<Tls>>);

pub struct TlsRead<'a>(&'a Rc<RefCell<Tls>>);
pub struct TlsWrite<'a>(&'a Rc<RefCell<Tls>>);

impl ErrorType for TlsRead<'_> {
    type Error = TlsStreamError;
}
impl Read for TlsRead<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().read(buf).await
    }
}

impl ErrorType for TlsWrite<'_> {
    type Error = TlsStreamError;
}
impl embedded_io_async::Write for TlsWrite<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.borrow_mut().write(buf).await
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.borrow_mut().flush().await
    }
}

impl picoserve::io::Write for TlsWrite<'_> {
    async fn write_with<F: FnOnce(picoserve::mem::BorrowedCursor<'_>) -> R, R>(
        &mut self,
        f: F,
    ) -> Result<R, Self::Error> {
        let mut buf = [0u8; 1024];
        let mut buffer = picoserve::mem::BorrowedBuffer::new(&mut buf);
        let r = f(buffer.unfilled());
        let filled = buffer.filled();
        self.0.borrow_mut().write_all(filled).await?;
        Ok(r)
    }
}

impl picoserve::io::Socket<WebRuntime> for TlsSocket {
    type Error = TlsStreamError;
    type ReadHalf<'a>
        = TlsRead<'a>
    where
        Self: 'a;
    type WriteHalf<'a>
        = TlsWrite<'a>
    where
        Self: 'a;

    fn split(&mut self) -> (Self::ReadHalf<'_>, Self::WriteHalf<'_>) {
        let rc = &self.0;
        (TlsRead(rc), TlsWrite(rc))
    }

    async fn abort<T: Timer<WebRuntime>>(
        self,
        _timeouts: &picoserve::Timeouts,
        _timer: &T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        Ok(())
    }

    async fn shutdown<T: Timer<WebRuntime>>(
        self,
        _timeouts: &picoserve::Timeouts,
        _timer: &T,
    ) -> Result<(), picoserve::Error<Self::Error>> {
        Ok(())
    }
}

/// Serve HTTP over an already-connected TLS stream.
pub async fn serve_connection(stream: Tls) {
    let app = App.build_app();
    let config = picoserve::Config::new(Default::default());
    // Per-connection heap buffer — no shared `static mut` (safe if serve ever
    // runs concurrently; we have a global allocator).
    let mut http_buffer = alloc::vec![0u8; 2048];

    let server = picoserve::Server::custom(&app, WebTimer, &config, &mut http_buffer[..]);
    let socket = TlsSocket(Rc::new(RefCell::new(stream)));
    let _ = server.serve(socket).await;
}

/// Networking task: accept HTTPS connections and serve them. Structural until the
/// MAC ring (docs/11) is pinned — real traffic then flows unchanged.
#[embassy_executor::task]
pub async fn web_task() {
    let con = Uart::console();

    // Fail closed: never run the TLS server with a placeholder/zero key. Load the
    // server identity (cert + Ed25519 key) from the flash cert store; if none is
    // provisioned, disable HTTPS rather than serve with an insecure key.
    let Some(store) = certstore::CertStore::load_from_flash() else {
        con.write_bytes(b"[web] no TLS cert provisioned - HTTPS server disabled\n");
        return;
    };
    let Some(seed) = x509::ed25519_seed_from_pkcs8(&store.key_der) else {
        con.write_bytes(b"[web] cert store key invalid - HTTPS server disabled\n");
        return;
    };
    // Leak the cert DER to 'static: a one-time boot identity that lives for the
    // whole run (ServerHandshake borrows it for each connection).
    let cert: &'static [u8] = Box::leak(store.cert_der.into_boxed_slice());

    let listener = tcp::add_tcp();
    tcp::with_sockets(|s| {
        use smoltcp::socket::tcp::Socket as TcpSocket;
        let sock = s.get_mut::<TcpSocket>(listener);
        let _ = sock.listen(443);
    });
    con.write_bytes(b"[web] HTTPS server listening on :443\n");

    loop {
        let conn = tcp::accept(listener).await;
        let stream = TcpStream::new(conn);

        let (eph, _) = crate::crypto::x25519_keypair();
        let hs = ServerHandshake::new(cert, &seed, ANCHORS, eph);

        match TlsStream::handshake(stream, hs).await {
            Ok(tls) => serve_connection(tls).await,
            Err(_) => {
                // handshake failed (e.g. no client cert) — drop the connection.
            }
        }

        // Re-arm the listener socket for the next connection.
        tcp::with_sockets(|s| {
            use smoltcp::socket::tcp::Socket as TcpSocket;
            let sock = s.get_mut::<TcpSocket>(listener);
            sock.abort();
            let _ = sock.listen(443);
        });
    }
}
