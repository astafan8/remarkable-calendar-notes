//! QTFB socket client: the Unix-only socket and shared-memory plumbing
//! around the pure wire protocol in [`crate::protocol`].
//!
//! This talks the AppLoad "QTFB" IPC protocol used on reMarkable OS 3.x: a
//! `SOCK_SEQPACKET` Unix socket at `/tmp/qtfb.sock`, backed by a POSIX
//! shared-memory framebuffer named after a *framebuffer key*.
//!
//! The key is **not** hardcoded. AppLoad passes it to each external app it
//! launches through the `QTFB_KEY` environment variable, and the host's
//! initialize reply confirms which key the connection was actually bound
//! to (`shmKeyDefined`) — that confirmed key, not the requested one, names
//! the shared-memory object this client maps.

use crate::protocol::{self, InputEvent};
use std::io;
use std::os::unix::io::RawFd;
use std::time::Duration;

const SOCKET_PATH: &str = "/tmp/qtfb.sock";

pub use crate::protocol::{input_kind, FBFMT_RM2FB};

pub struct QtfbClient {
    fd: RawFd,
    shm_ptr: *mut u8,
    shm_len: usize,
    pub width: usize,
    pub height: usize,
    /// The framebuffer key the host confirmed for this connection.
    pub framebuffer_key: i32,
}

// The shared-memory pointer is only ever touched from the single thread
// that owns this client (the app's main render loop).
unsafe impl Send for QtfbClient {}

impl QtfbClient {
    /// Connect to the AppLoad QTFB host using the framebuffer key from the
    /// `QTFB_KEY` environment variable, and negotiate the reMarkable 2
    /// native framebuffer format.
    pub fn connect(width: usize, height: usize) -> io::Result<Self> {
        let key = protocol::framebuffer_key_from_env()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        Self::connect_with_key(key, width, height)
    }

    /// Connect using an explicit framebuffer key.
    pub fn connect_with_key(key: i32, width: usize, height: usize) -> io::Result<Self> {
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (dst, src) in addr.sun_path.iter_mut().zip(SOCKET_PATH.bytes()) {
            *dst = src as libc::c_char;
        }
        let connect_rc = unsafe {
            libc::connect(
                fd,
                (&addr as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if connect_rc != 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }

        let init_msg = protocol::init_message(key, protocol::FBFMT_RM2FB);
        send_all(fd, &init_msg)?;

        let mut reply = [0u8; 32];
        let n = unsafe { libc::recv(fd, reply.as_mut_ptr().cast(), reply.len(), 0) };
        if n <= 0 {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "qtfb host rejected initialize (connection closed without reply)",
            ));
        }
        let Some(init) = protocol::parse_init_reply(&reply, n as usize) else {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "qtfb host sent a truncated initialize reply",
            ));
        };
        if init.shm_key_defined == 0 || init.shm_size < width * height * 2 {
            unsafe { libc::close(fd) };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "qtfb shared memory undefined or too small (key {}, {} bytes)",
                    init.shm_key_defined, init.shm_size
                ),
            ));
        }

        // Map the object named by the key the *host* confirmed.
        let shm_path = format!("{}\0", protocol::shm_path(init.shm_key_defined));
        let shm_fd = unsafe { libc::open(shm_path.as_ptr().cast(), libc::O_RDWR) };
        if shm_fd < 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }
        let shm_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                init.shm_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                shm_fd,
                0,
            )
        };
        unsafe { libc::close(shm_fd) };
        if shm_ptr == libc::MAP_FAILED {
            unsafe { libc::close(fd) };
            return Err(io::Error::other("failed to map qtfb shared framebuffer"));
        }

        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }

        Ok(QtfbClient {
            fd,
            shm_ptr: shm_ptr.cast(),
            shm_len: init.shm_size,
            width,
            height,
            framebuffer_key: init.shm_key_defined,
        })
    }

    /// Direct mutable access to the shared framebuffer memory, expected to
    /// hold RGB565 little-endian pixel data (see
    /// `calnotes_core::render::FrameBuffer::write_rgb565_into`).
    pub fn shared_memory(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.shm_ptr, self.shm_len) }
    }

    pub fn request_full_update(&self) -> io::Result<()> {
        send_all(self.fd, &protocol::update_all_message())
    }

    /// Request the host repaint only `x,y,w,h` — the basis of this app's
    /// incremental pen drawing, which touches a small dirty rectangle per
    /// stroke segment instead of repainting the whole screen.
    pub fn request_partial_update(&self, x: i32, y: i32, w: i32, h: i32) -> io::Result<()> {
        send_all(self.fd, &protocol::update_partial_message(x, y, w, h))
    }

    fn terminate(&self) {
        let _ = send_all(self.fd, &protocol::terminate_message());
    }

    /// Drain every pending server message, returning input events (touch,
    /// pen, and virtual-keyboard key presses forwarded by the AppLoad
    /// window chrome). Returns `Err` if the host closed the connection
    /// (the window was closed and the app must exit).
    pub fn poll_events(&self) -> io::Result<Vec<InputEvent>> {
        let mut out = Vec::new();
        loop {
            let mut buf = [0u8; 32];
            let n = unsafe { libc::recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "qtfb host closed the socket",
                ));
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    return Ok(out);
                }
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
            if let Some(ev) = protocol::parse_input_event(&buf, n as usize) {
                out.push(ev);
            }
        }
    }
}

impl Drop for QtfbClient {
    fn drop(&mut self) {
        self.terminate();
        unsafe {
            libc::munmap(self.shm_ptr.cast(), self.shm_len);
            libc::close(self.fd);
        }
    }
}

fn send_all(fd: RawFd, buf: &[u8]) -> io::Result<()> {
    let mut waited = Duration::ZERO;
    loop {
        let n = unsafe { libc::send(fd, buf.as_ptr().cast(), buf.len(), 0) };
        if n as usize == buf.len() {
            return Ok(());
        }
        if n < 0 {
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock if waited < Duration::from_millis(200) => {
                    std::thread::sleep(Duration::from_millis(2));
                    waited += Duration::from_millis(2);
                    continue;
                }
                _ => return Err(err),
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short write to qtfb socket",
        ));
    }
}
