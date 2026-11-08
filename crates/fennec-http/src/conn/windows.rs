//! A socket as `conn`'s list of open TLS connections keys it, on Windows
//! (docs/design/windows.md, 7g): std names a socket there a `SOCKET`
//! (`AsRawSocket`) where Unix names a descriptor (`AsRawFd`), so these are
//! the descriptor traits' stand-ins over it, and the list, the one
//! shutdown's end of each connection with its `close_notify` among them,
//! is the same code on both.

use std::ffi::c_void;
use std::net::TcpStream;
use std::os::windows::io::{AsRawSocket, FromRawSocket};

#[link(name = "kernel32")]
extern "system" {
    fn CancelIoEx(handle: *mut c_void, overlapped: *mut c_void) -> i32;
}

/// Wakes a thread blocked reading `sock`, which the one shutdown has shut
/// for reading: on Unix that shutdown wakes it, a read answering the end;
/// Winsock's (`SD_RECEIVE`) does not, and a connection waiting for its next
/// request held its thread until the process's exit cut it with a reset,
/// no `close_notify` sent (`every_tls_end_is_a_close_notify_the_shutdown_s_too`
/// on Windows CI). Its reads are cancelled instead: std makes every socket
/// overlapped, so a blocking read is a request on the socket's handle that
/// `CancelIoEx` ends from another thread, and the read after it meets the
/// shut read side and fails, ending the connection with its `close_notify`.
pub fn cancel_reads(sock: &TcpStream) {
    // SAFETY: the socket's own handle, open while it is borrowed; a socket
    // with no read under way has nothing to cancel, which is no error here.
    unsafe {
        CancelIoEx(sock.as_raw_socket() as *mut c_void, std::ptr::null_mut());
    }
}

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
