//! BPF frame I/O for a `feth` peer interface.
//!
//! The peer side receives every Ethernet frame injected by the phone and
//! accepts frames written by the host.  BPF writes are limited to the BPF
//! snapshot size; 1500-byte frames are well below that limit.  The larger
//! AF_NDRV injection path mentioned in older documentation is only needed for
//! jumbo MTUs, which this prototype does not use.
//!
//! Opening `/dev/bpf*`, binding to an interface and enabling promiscuous mode
//! require root.  The helper does that and passes the configured descriptor to
//! the unprivileged protocol process via `SCM_RIGHTS`.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::os::fd::{IntoRawFd, RawFd};
use std::time::Duration;

use tether_core::error::{Result, TetherError};
use tether_core::ethernet::{self, ETHERNET_MAX_FRAME, ETHERNET_MIN_FRAME};
use tether_core::net::FrameIo;

// Darwin ioctl numbers, checked against <net/bpf.h> and <sys/ioccom.h>.
// They are architecture-independent on macOS.
const BIOCSBLEN: libc::c_ulong = 0xc004_4266; // _IOWR('B', 102, u_int)
const BIOCPROMISC: libc::c_ulong = 0x2000_4269; // _IO('B', 105)
const BIOCSETIF: libc::c_ulong = 0x8020_426c; // _IOW('B', 108, struct ifreq)
const BIOCIMMEDIATE: libc::c_ulong = 0x8004_4270; // _IOW('B', 112, u_int)
const BIOCSHDRCMPLT: libc::c_ulong = 0x8004_4275; // _IOW('B', 117, u_int)
const BIOCSSEESENT: libc::c_ulong = 0x8004_4277; // _IOW('B', 119, u_int)

// On Darwin the kernel reports `bh_hdrlen` as 18: the 8-byte BPF_TIMEVAL,
// two 32-bit lengths and a 16-bit header length, packed without the two bytes
// of trailing padding that `sizeof(struct bpf_hdr)` adds in userspace.
const BPF_HEADER_LEN: usize = 18;
const BPF_HEADER_MAX: usize = 64;
const BPF_ALIGNMENT: usize = 4;
const DEFAULT_BUFFER: u32 = 256 * 1024;

#[repr(C)]
struct IfReq {
    name: [u8; 16],
    rest: [u8; 16], // union ifr_ifru; only the name is used
}

impl IfReq {
    fn new(name: &str) -> Result<Self> {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() >= 16 {
            return Err(TetherError::network("invalid BPF interface name"));
        }
        let mut request = Self {
            name: [0; 16],
            rest: [0; 16],
        };
        request.name[..bytes.len()].copy_from_slice(bytes);
        Ok(request)
    }
}

pub struct BpfSocket {
    fd: RawFd,
    interface: String,
    read_buffer: Vec<u8>,
    pending: VecDeque<Vec<u8>>,
    link_up: bool,
}

impl BpfSocket {
    /// Open and configure a new BPF device for `interface`.  Root required.
    pub fn open(interface: &str) -> Result<Self> {
        if interface.is_empty() || interface.len() >= 16 {
            return Err(TetherError::network("invalid BPF interface name"));
        }
        let mut last_error = None;
        for index in 0..256 {
            let path = format!("/dev/bpf{index}");
            match OpenOptions::new().read(true).write(true).open(&path) {
                Ok(file) => {
                    let fd = file.into_raw_fd();
                    match configure(fd, interface) {
                        Ok(buffer_size) => {
                            return Ok(Self {
                                fd,
                                interface: interface.to_string(),
                                read_buffer: vec![0u8; buffer_size as usize],
                                pending: VecDeque::new(),
                                link_up: false,
                            });
                        }
                        Err(error) => {
                            unsafe { libc::close(fd) };
                            return Err(error);
                        }
                    }
                }
                Err(error) if error.kind() == ErrorKind::PermissionDenied => {
                    return Err(TetherError::new(
                        tether_core::error::ErrorCategory::PermissionRequired,
                        format!("cannot open {path}: permission denied (root helper required)"),
                    ));
                }
                Err(error) => {
                    last_error = Some(format!("{path}: {error}"));
                }
            }
        }
        Err(TetherError::network(format!(
            "no usable BPF device found{}",
            last_error
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        )))
    }

    /// Adopt a descriptor created and configured by the privileged helper.
    ///
    /// # Safety / contract
    /// The caller must pass a live, uniquely owned BPF descriptor.  The
    /// descriptor is closed on drop.
    pub fn from_raw_fd(fd: RawFd, interface: &str) -> Result<Self> {
        if fd < 0 {
            return Err(TetherError::network("invalid BPF file descriptor"));
        }
        let mut buffer_size = DEFAULT_BUFFER;
        let mut size = buffer_size;
        let result = unsafe { libc::ioctl(fd, BIOCSBLEN, &mut size) };
        if result == 0 && size >= 2048 {
            buffer_size = size;
        }
        Ok(Self {
            fd,
            interface: interface.to_string(),
            read_buffer: vec![0u8; buffer_size as usize],
            pending: VecDeque::new(),
            link_up: false,
        })
    }

    pub fn interface(&self) -> &str {
        &self.interface
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }
}

fn configure(fd: RawFd, interface: &str) -> Result<u32> {
    let mut size = DEFAULT_BUFFER;
    if unsafe { libc::ioctl(fd, BIOCSBLEN, &mut size) } != 0 {
        return Err(last_os_error("BIOCSBLEN"));
    }
    if size < 2048 {
        return Err(TetherError::network(
            "BPF driver returned an unusably small buffer",
        ));
    }
    let mut value: libc::c_uint = 1;
    if unsafe { libc::ioctl(fd, BIOCIMMEDIATE, &mut value) } != 0 {
        return Err(last_os_error("BIOCIMMEDIATE"));
    }
    let mut see_sent: libc::c_uint = 0;
    if unsafe { libc::ioctl(fd, BIOCSSEESENT, &mut see_sent) } != 0 {
        return Err(last_os_error("BIOCSSEESENT"));
    }
    let mut request = IfReq::new(interface)?;
    if unsafe { libc::ioctl(fd, BIOCSETIF, &mut request) } != 0 {
        return Err(last_os_error("BIOCSETIF"));
    }
    let mut header_complete: libc::c_uint = 1;
    if unsafe { libc::ioctl(fd, BIOCSHDRCMPLT, &mut header_complete) } != 0 {
        return Err(last_os_error("BIOCSHDRCMPLT"));
    }
    if unsafe { libc::ioctl(fd, BIOCPROMISC) } != 0 {
        return Err(last_os_error("BIOCPROMISC"));
    }
    Ok(size)
}

fn last_os_error(operation: &str) -> TetherError {
    TetherError::network(format!(
        "{operation} failed: {}",
        std::io::Error::last_os_error()
    ))
}

impl FrameIo for BpfSocket {
    fn recv_frame(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        if let Some(frame) = self.pending.pop_front() {
            if frame.len() > buffer.len() {
                return Err(TetherError::network("BPF frame exceeds caller buffer"));
            }
            buffer[..frame.len()].copy_from_slice(&frame);
            return Ok(Some(frame.len()));
        }
        if !wait_readable(self.fd, timeout)? {
            return Ok(None);
        }
        let received = unsafe {
            libc::read(
                self.fd,
                self.read_buffer.as_mut_ptr() as *mut libc::c_void,
                self.read_buffer.len(),
            )
        };
        if received < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::WouldBlock || error.kind() == ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(TetherError::network(format!("BPF read failed: {error}")));
        }
        if received == 0 {
            return Ok(None);
        }
        let frames = parse_bpf_packets(&self.read_buffer[..received as usize])?;
        self.pending.extend(frames);
        let Some(frame) = self.pending.pop_front() else {
            return Ok(None);
        };
        if frame.len() > buffer.len() {
            return Err(TetherError::network("BPF frame exceeds caller buffer"));
        }
        buffer[..frame.len()].copy_from_slice(&frame);
        Ok(Some(frame.len()))
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        if frame.len() < ETHERNET_MIN_FRAME || frame.len() > ETHERNET_MAX_FRAME {
            return Err(TetherError::network(
                "invalid Ethernet frame length for BPF",
            ));
        }
        let _ = ethernet::parse_ethernet(frame)?;
        let written =
            unsafe { libc::write(self.fd, frame.as_ptr() as *const libc::c_void, frame.len()) };
        if written < 0 {
            return Err(TetherError::network(format!(
                "BPF write failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        if written as usize != frame.len() {
            return Err(TetherError::network("short BPF write"));
        }
        Ok(())
    }

    fn set_link(&mut self, up: bool) -> Result<()> {
        // The OS-level link is controlled through the helper's LINK command.
        // This method only records what the protocol engine requested; the
        // RemoteFrameIo wrapper performs the privileged transition.
        self.link_up = up;
        Ok(())
    }

    fn link_up(&self) -> bool {
        self.link_up
    }
}

impl Drop for BpfSocket {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
    }
}

fn wait_readable(fd: RawFd, timeout: Duration) -> Result<bool> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, millis) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(TetherError::network(format!("poll on BPF failed: {error}")));
        }
        if result == 0 {
            return Ok(false);
        }
        if poll_fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Err(TetherError::network(
                "BPF descriptor reported a fatal poll event",
            ));
        }
        return Ok(poll_fd.revents & libc::POLLIN != 0);
    }
}

/// Parse a BPF read buffer into complete Ethernet frames.  Kept public and
/// pure so it can be tested without `/dev/bpf`.
pub fn parse_bpf_packets(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut frames = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes.len() - offset < BPF_HEADER_LEN {
            return Err(TetherError::network("truncated bpf_hdr in BPF read buffer"));
        }
        let caplen = u32::from_ne_bytes([
            bytes[offset + 8],
            bytes[offset + 9],
            bytes[offset + 10],
            bytes[offset + 11],
        ]) as usize;
        let datalen = u32::from_ne_bytes([
            bytes[offset + 12],
            bytes[offset + 13],
            bytes[offset + 14],
            bytes[offset + 15],
        ]) as usize;
        let header_len = u16::from_ne_bytes([bytes[offset + 16], bytes[offset + 17]]) as usize;
        if !(BPF_HEADER_LEN..=BPF_HEADER_MAX).contains(&header_len) {
            let end = (offset + 32).min(bytes.len());
            return Err(TetherError::network(format!(
                "invalid bpf_hdr length {header_len} at offset {offset} in {}-byte buffer: {:02x?}",
                bytes.len(),
                &bytes[offset..end]
            )));
        }
        if caplen != datalen {
            return Err(TetherError::network(
                "BPF capture was truncated; increase the BPF buffer size",
            ));
        }
        if caplen > 65535 {
            return Err(TetherError::network("BPF frame is unreasonably large"));
        }
        let start = offset + header_len;
        let end = start
            .checked_add(caplen)
            .ok_or_else(|| TetherError::network("BPF frame length overflow"))?;
        if end > bytes.len() {
            return Err(TetherError::network("BPF frame exceeds read buffer"));
        }
        frames.push(bytes[start..end].to_vec());
        let consumed = header_len + caplen;
        let aligned = (consumed + BPF_ALIGNMENT - 1) & !(BPF_ALIGNMENT - 1);
        offset = offset
            .checked_add(aligned)
            .ok_or_else(|| TetherError::network("BPF alignment overflow"))?;
        if frames.len() > 4096 {
            return Err(TetherError::network("too many frames in one BPF read"));
        }
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bpf_record(frame: &[u8]) -> Vec<u8> {
        let mut record = vec![0u8; BPF_HEADER_LEN];
        record[8..12].copy_from_slice(&(frame.len() as u32).to_ne_bytes());
        record[12..16].copy_from_slice(&(frame.len() as u32).to_ne_bytes());
        record[16..18].copy_from_slice(&(BPF_HEADER_LEN as u16).to_ne_bytes());
        record.extend_from_slice(frame);
        while record.len() % BPF_ALIGNMENT != 0 {
            record.push(0);
        }
        record
    }

    #[test]
    fn parses_one_and_multiple_bpf_records() {
        let frame = ethernet::build_arp_request(
            ethernet::MacAddr([0x02, 0, 0, 0, 0, 1]),
            [10, 0, 0, 1],
            [10, 0, 0, 2],
        );
        let mut bytes = bpf_record(&frame);
        assert_eq!(parse_bpf_packets(&bytes).unwrap(), vec![frame.clone()]);
        bytes.extend_from_slice(&bpf_record(&frame));
        assert_eq!(
            parse_bpf_packets(&bytes).unwrap(),
            vec![frame.clone(), frame]
        );
    }

    #[test]
    fn rejects_truncated_record() {
        let frame = vec![0u8; 42];
        let record = bpf_record(&frame);
        assert!(parse_bpf_packets(&record[..record.len() - 4]).is_err());
    }

    #[test]
    fn rejects_truncated_capture() {
        let mut record = bpf_record(&[0u8; 42]);
        record[12..16].copy_from_slice(&100u32.to_ne_bytes());
        assert!(parse_bpf_packets(&record).is_err());
    }

    #[test]
    fn rejects_short_bpf_header_length() {
        let mut record = bpf_record(&[0u8; 42]);
        record[16..18].copy_from_slice(&4u16.to_ne_bytes());
        assert!(parse_bpf_packets(&record).is_err());
    }

    #[test]
    fn honors_padded_bpf_header_length() {
        let frame = ethernet::build_arp_request(
            ethernet::MacAddr([0x02, 0, 0, 0, 0, 1]),
            [10, 0, 0, 1],
            [10, 0, 0, 2],
        );
        let mut record = vec![0u8; 20];
        record[8..12].copy_from_slice(&(frame.len() as u32).to_ne_bytes());
        record[12..16].copy_from_slice(&(frame.len() as u32).to_ne_bytes());
        record[16..18].copy_from_slice(&20u16.to_ne_bytes());
        record.extend_from_slice(&frame);
        while record.len() % BPF_ALIGNMENT != 0 {
            record.push(0);
        }
        assert_eq!(parse_bpf_packets(&record).unwrap(), vec![frame]);
    }

    #[test]
    fn ioctl_constants_match_darwin_header_values() {
        // These values were printed by a small C program compiled against the
        // macOS SDK; the test catches accidental edits.
        assert_eq!(BIOCSETIF, 0x8020_426c);
        assert_eq!(BIOCSBLEN, 0xc004_4266);
        assert_eq!(BIOCIMMEDIATE, 0x8004_4270);
        assert_eq!(BIOCSHDRCMPLT, 0x8004_4275);
        assert_eq!(BIOCSSEESENT, 0x8004_4277);
        assert_eq!(BIOCPROMISC, 0x2000_4269);
    }
}
