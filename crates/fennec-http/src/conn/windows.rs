//! A socket as `conn`'s list of open TLS connections keys it, on Windows
//! (docs/design/windows.md, 7g): std names a socket there a `SOCKET`
//! (`AsRawSocket`) where Unix names a descriptor (`AsRawFd`), so these are
//! the descriptor traits' stand-ins over it, and the list, the one
//! shutdown's end of each connection with its `close_notify` among them,
//! is the same code on both.

use std::net::TcpStream;
use std::os::windows::io::{AsRawSocket, FromRawSocket};

/// A socket's handle, as the list keys it.
pub type RawFd = std::os::windows::io::RawSocket;

/// std's `AsRawFd`, over a socket's `SOCKET`.
pub trait AsRawFd {
    fn as_raw_fd(&self) -> RawFd;
}

impl AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.as_raw_socket()
    }
}

/// std's `FromRawFd`, over a socket's `SOCKET`.
pub trait FromRawFd {
    /// # Safety
    ///
    /// `raw` is an open socket, which the value owns: its drop closes it.
    unsafe fn from_raw_fd(raw: RawFd) -> Self;
}

impl FromRawFd for TcpStream {
    unsafe fn from_raw_fd(raw: RawFd) -> Self {
        // SAFETY: the caller's, as std's `FromRawSocket` asks it.
        unsafe { TcpStream::from_raw_socket(raw) }
    }
}
