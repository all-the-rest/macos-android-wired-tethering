//! Newline-delimited helper protocol with optional `SCM_RIGHTS` fd passing.
//!
//! The helper owns the privileged feth/BPF setup.  It sends the configured BPF
//! descriptor to the unprivileged process together with the `CREATE` response;
//! all later commands are plain text on the same Unix stream.  A line-oriented
//! protocol is intentionally small and easy to audit.

use std::io;
use std::mem;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

pub const PROTOCOL_VERSION: u32 = 1;
const MAX_LINE: usize = 64 * 1024;

#[repr(C, align(8))]
struct ControlBuffer([u8; 128]);

pub struct FdChannel {
    stream: UnixStream,
    buffer: Vec<u8>,
    pending_fd: Option<RawFd>,
}

impl FdChannel {
    pub fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            pending_fd: None,
        }
    }

    pub fn raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(timeout)
    }

    pub fn send_line(&self, line: &str) -> io::Result<()> {
        self.send_line_with_fd(line, None)
    }

    pub fn send_line_with_fd(&self, line: &str, fd: Option<RawFd>) -> io::Result<()> {
        let mut payload = line.as_bytes().to_vec();
        if !payload.ends_with(b"\n") {
            payload.push(b'\n');
        }
        let mut iovec = libc::iovec {
            iov_base: payload.as_ptr() as *mut libc::c_void,
            iov_len: payload.len(),
        };
        let mut message: libc::msghdr = unsafe { mem::zeroed() };
        message.msg_iov = &mut iovec;
        message.msg_iovlen = 1;
        let mut control = ControlBuffer([0u8; 128]);
        if let Some(fd) = fd {
            unsafe {
                message.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
                message.msg_controllen =
                    libc::CMSG_SPACE(mem::size_of::<libc::c_int>() as u32) as libc::socklen_t;
                let cmsg = libc::CMSG_FIRSTHDR(&message);
                if cmsg.is_null() {
                    return Err(io::Error::other(
                        "failed to construct SCM_RIGHTS control message",
                    ));
                }
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len =
                    libc::CMSG_LEN(mem::size_of::<libc::c_int>() as u32) as libc::socklen_t;
                let data = libc::CMSG_DATA(cmsg) as *mut libc::c_int;
                std::ptr::write_unaligned(data, fd);
            }
        }
        let sent = unsafe { libc::sendmsg(self.stream.as_raw_fd(), &message, 0) };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        if sent as usize != payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short helper protocol write",
            ));
        }
        Ok(())
    }

    /// Receive one line.  `Ok(None)` means the peer closed the stream.
    pub fn recv_line(&mut self) -> io::Result<Option<String>> {
        loop {
            if let Some(position) = self.buffer.iter().position(|&byte| byte == b'\n') {
                if position > MAX_LINE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "helper protocol line too long",
                    ));
                }
                let line: Vec<u8> = self.buffer.drain(..=position).collect();
                let line = String::from_utf8(line).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 protocol line")
                })?;
                return Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()));
            }
            if self.buffer.len() > MAX_LINE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "helper protocol line too long",
                ));
            }
            let mut chunk = [0u8; 4096];
            let (received, fd) = recv_with_fd(self.stream.as_raw_fd(), &mut chunk)?;
            if let Some(fd) = fd {
                if self.pending_fd.is_some() {
                    unsafe { libc::close(fd) };
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "more than one fd in a single helper message",
                    ));
                }
                self.pending_fd = Some(fd);
            }
            if received == 0 {
                if let Some(fd) = self.pending_fd.take() {
                    unsafe { libc::close(fd) };
                }
                return Ok(None);
            }
            self.buffer.extend_from_slice(&chunk[..received]);
        }
    }

    /// Take the file descriptor that accompanied the most recent line.
    pub fn take_fd(&mut self) -> Option<RawFd> {
        self.pending_fd.take()
    }
}

fn recv_with_fd(fd: RawFd, buffer: &mut [u8]) -> io::Result<(usize, Option<RawFd>)> {
    let mut iovec = libc::iovec {
        iov_base: buffer.as_mut_ptr() as *mut libc::c_void,
        iov_len: buffer.len(),
    };
    let mut message: libc::msghdr = unsafe { mem::zeroed() };
    message.msg_iov = &mut iovec;
    message.msg_iovlen = 1;
    let mut control = ControlBuffer([0u8; 128]);
    message.msg_control = control.0.as_mut_ptr() as *mut libc::c_void;
    message.msg_controllen = control.0.len() as libc::socklen_t;
    let received = unsafe { libc::recvmsg(fd, &mut message, 0) };
    if received < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut passed_fd = None;
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&message);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET
                && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                && (*cmsg).cmsg_len
                    >= libc::CMSG_LEN(mem::size_of::<libc::c_int>() as u32) as libc::socklen_t
            {
                let data = libc::CMSG_DATA(cmsg) as *const libc::c_int;
                passed_fd = Some(std::ptr::read_unaligned(data));
            }
            cmsg = libc::CMSG_NXTHDR(&message, cmsg);
        }
    }
    if message.msg_flags & libc::MSG_CTRUNC != 0 {
        if let Some(fd) = passed_fd.take() {
            unsafe { libc::close(fd) };
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SCM_RIGHTS control message was truncated",
        ));
    }
    Ok((received as usize, passed_fd))
}

/// Helper-side peer credentials.  Returns `(uid, gid)`.
pub fn peer_credentials(stream: &UnixStream) -> io::Result<(libc::uid_t, libc::gid_t)> {
    let mut uid = 0 as libc::uid_t;
    let mut gid = 0 as libc::gid_t;
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::IntoRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn sends_and_receives_a_line_without_fd() {
        let (a, b) = UnixStream::pair().unwrap();
        let sender = FdChannel::new(a);
        let mut receiver = FdChannel::new(b);
        sender.send_line("PING").unwrap();
        assert_eq!(receiver.recv_line().unwrap().as_deref(), Some("PING"));
    }

    #[test]
    fn passes_a_file_descriptor() {
        let (a, b) = UnixStream::pair().unwrap();
        let sender = FdChannel::new(a);
        let mut receiver = FdChannel::new(b);
        let file = std::fs::File::open("/dev/null").unwrap();
        let raw = file.into_raw_fd();
        sender.send_line_with_fd("CREATE 1500", Some(raw)).unwrap();
        let line = receiver.recv_line().unwrap().unwrap();
        assert_eq!(line, "CREATE 1500");
        let received = receiver.take_fd().expect("fd passed");
        let mut stat: libc::stat = unsafe { mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(received, &mut stat) }, 0);
        assert_eq!(stat.st_mode & libc::S_IFMT, libc::S_IFCHR);
        unsafe {
            libc::close(raw);
            libc::close(received);
        }
    }

    #[test]
    fn rejects_oversized_line() {
        let (a, b) = UnixStream::pair().unwrap();
        let sender = FdChannel::new(a);
        let mut receiver = FdChannel::new(b);
        let line = "x".repeat(MAX_LINE + 10);
        let handle = std::thread::spawn(move || {
            let _ = sender.send_line(&line);
        });
        assert!(receiver.recv_line().is_err());
        let _ = handle.join();
    }

    #[test]
    fn eof_returns_none() {
        let (a, b) = UnixStream::pair().unwrap();
        drop(a);
        let mut receiver = FdChannel::new(b);
        assert!(receiver.recv_line().unwrap().is_none());
    }
}
