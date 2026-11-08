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
/// on Windows CI). std makes every socket overlapped, so a blocking call is
/// a request on the socket's handle that `CancelIoEx` ends from another
/// thread; the read after it meets the shut read side and fails, ending
/// the connection with its `close_notify`.
///
/// It cancels every request on the socket, not its reads alone -- the call
/// takes no overlapped of std's to name one -- so a send blocked on a slow
/// reader fails too: an answer in the middle of its write is cut there,
/// with no `close_notify`, where on Unix it goes on through the shutdown's
/// grace. And `SD_RECEIVE` itself resets a connection whose client sent
/// bytes the server had not read, or sends more after it (Winsock's
/// `shutdown` page). docs/design/windows.md, 7g.
pub fn cancel_io(sock: &TcpStream) {
    // SAFETY: the socket's own handle, open while it is borrowed; a socket
    // with nothing under way has nothing to cancel, which is no error here.
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
