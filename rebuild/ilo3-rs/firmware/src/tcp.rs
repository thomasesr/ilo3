//! smoltcp TCP → async `embedded-io-async` bridge (Phase 5 web).
//!
//! smoltcp is poll-based and single-owner; picoserve is async and wants to own
//! a stream. This module bridges the two: a shared `SocketSet` (behind a
//! critical-section mutex — single core) that the poll loop and the async
//! connection tasks both access. `TcpStream` wraps a `SocketHandle` and
//! implements `embedded_io_async::{Read, Write}` by registering the socket's
//! recv/send wakers and re-acquiring the socket on each poll (never holding the
//! `&mut SocketSet` across an await point, so the poll loop keeps running).

#![allow(dead_code)]

use core::cell::RefCell;
use core::fmt;
use core::future::poll_fn;
use core::task::Poll;

use alloc::vec;
use critical_section::Mutex;
use embedded_io_async::{ErrorType, Read, Write};
use smoltcp::iface::{SocketHandle, SocketSet, SocketStorage};
use smoltcp::socket::AnySocket;
use smoltcp::socket::tcp::{RecvError, SendError, Socket as TcpSocket, SocketBuffer};

/// Number of sockets (DHCP + listener + connections).
pub const SOCKET_COUNT: usize = 8;
const SOCKET_BUF: usize = 16 * 1024;

static mut STORAGE: [SocketStorage<'static>; SOCKET_COUNT] = [SocketStorage::EMPTY; SOCKET_COUNT];
static SOCKETS: Mutex<RefCell<Option<SocketSet<'static>>>> = Mutex::new(RefCell::new(None));

/// Initialise the shared socket set (call once at boot, before any net task).
pub fn init() {
    // SAFETY: STORAGE is written once here and never aliased afterwards.
    let storage = unsafe { &mut STORAGE[..] };
    let set = SocketSet::new(storage);
    critical_section::with(|cs| *SOCKETS.borrow_ref_mut(cs) = Some(set));
}

/// Run `f` with exclusive access to the shared socket set.
pub fn with_sockets<R>(f: impl FnOnce(&mut SocketSet<'static>) -> R) -> R {
    critical_section::with(|cs| {
        let mut set = SOCKETS.borrow_ref_mut(cs);
        f(set.as_mut().expect("tcp::init not called"))
    })
}

/// Add a TCP socket with its own rx/tx buffers; returns its handle.
pub fn add_tcp() -> SocketHandle {
    let rx = SocketBuffer::new(vec![0u8; SOCKET_BUF]);
    let tx = SocketBuffer::new(vec![0u8; SOCKET_BUF]);
    let socket = TcpSocket::new(rx, tx);
    with_sockets(|s| s.add(socket))
}

/// Add any socket to the shared set; returns its handle.
pub fn add_socket<T: AnySocket<'static>>(socket: T) -> SocketHandle {
    with_sockets(|s| s.add(socket))
}

/// smoltcp TCP socket error (embedded-io compatible).
#[derive(Debug)]
pub enum TcpError {
    Closed,
}

impl fmt::Display for TcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tcp error")
    }
}

impl core::error::Error for TcpError {}

impl embedded_io::Error for TcpError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}

/// An async Read+Write over a smoltcp TCP socket, keyed by `SocketHandle`.
pub struct TcpStream {
    handle: SocketHandle,
}

impl TcpStream {
    pub fn new(handle: SocketHandle) -> Self {
        TcpStream { handle }
    }
}

impl ErrorType for TcpStream {
    type Error = TcpError;
}

impl Read for TcpStream {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let handle = self.handle;
        poll_fn(|cx| {
            let result = with_sockets(|s| {
                let sock = s.get_mut::<TcpSocket>(handle);
                sock.register_recv_waker(cx.waker());
                sock.recv_slice(buf)
            });
            match result {
                Ok(n) if n > 0 => Poll::Ready(Ok(n)),
                Ok(_) => Poll::Pending,
                Err(RecvError::Finished) => Poll::Ready(Ok(0)),
                Err(_) => Poll::Ready(Err(TcpError::Closed)),
            }
        })
        .await
    }
}

impl Write for TcpStream {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let handle = self.handle;
        poll_fn(|cx| {
            let result = with_sockets(|s| {
                let sock = s.get_mut::<TcpSocket>(handle);
                sock.register_send_waker(cx.waker());
                sock.send_slice(buf)
            });
            match result {
                Ok(n) if n > 0 => Poll::Ready(Ok(n)),
                Ok(_) => Poll::Pending,
                Err(SendError::InvalidState) => Poll::Ready(Err(TcpError::Closed)),
            }
        })
        .await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Wait until the listener socket has an established connection, then return
/// its handle (smoltcp 0.12 transitions the listener itself into the connected
/// state — no separate `accept`).
pub async fn accept(listener: SocketHandle) -> SocketHandle {
    poll_fn(|cx| {
        let ready = with_sockets(|s| {
            let sock = s.get_mut::<TcpSocket>(listener);
            sock.register_recv_waker(cx.waker());
            sock.is_active()
        });
        if ready {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    })
    .await;
    listener
}
