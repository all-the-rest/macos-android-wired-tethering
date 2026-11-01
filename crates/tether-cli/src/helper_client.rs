//! Client for the privileged helper and the helper-backed `FrameIo`.

use std::os::fd::RawFd;
use std::path::Path;
use std::time::Duration;

use tether_core::error::{ErrorCategory, Result, TetherError};
use tether_core::net::FrameIo;
use tether_macos::bpf::BpfSocket;
use tether_macos::ipc::{FdChannel, PROTOCOL_VERSION};

pub struct HelperClient {
    channel: FdChannel,
}

#[derive(Debug, Clone)]
pub struct CreatedBridge {
    pub host_iface: String,
    pub peer_iface: String,
    pub service: String,
    pub fd: RawFd,
}

impl HelperClient {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = std::os::unix::net::UnixStream::connect(path).map_err(|error| {
            TetherError::new(
                ErrorCategory::HelperUnavailable,
                format!("cannot connect to {}: {error}", path.display()),
            )
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .map_err(|error| {
                TetherError::new(ErrorCategory::HelperUnavailable, error.to_string())
            })?;
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .map_err(|error| {
                TetherError::new(ErrorCategory::HelperUnavailable, error.to_string())
            })?;
        Ok(Self {
            channel: FdChannel::new(stream),
        })
    }

    pub fn hello(&mut self) -> Result<()> {
        self.channel
            .send_line(&format!("HELLO {PROTOCOL_VERSION}"))
            .map_err(helper_io)?;
        let line = self
            .channel
            .recv_line()
            .map_err(helper_io)?
            .ok_or_else(|| {
                TetherError::new(
                    ErrorCategory::HelperUnavailable,
                    "helper closed the connection during hello",
                )
            })?;
        if line.starts_with("OK ") {
            Ok(())
        } else {
            Err(parse_error(&line))
        }
    }

    pub fn ping(&mut self) -> Result<()> {
        self.expect_ok("PING")?;
        Ok(())
    }

    pub fn create(&mut self, mtu: u16, host_mac: &str) -> Result<CreatedBridge> {
        self.channel
            .send_line(&format!("CREATE {mtu} {host_mac}"))
            .map_err(helper_io)?;
        let line = self
            .channel
            .recv_line()
            .map_err(helper_io)?
            .ok_or_else(|| {
                TetherError::new(
                    ErrorCategory::HelperUnavailable,
                    "helper closed the connection during CREATE",
                )
            })?;
        if !line.starts_with("OK ") {
            return Err(parse_error(&line));
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() != 4 {
            return Err(TetherError::new(
                ErrorCategory::HelperUnavailable,
                "malformed CREATE response from helper",
            ));
        }
        let fd = self.channel.take_fd().ok_or_else(|| {
            TetherError::new(
                ErrorCategory::HelperUnavailable,
                "helper did not pass a BPF descriptor",
            )
        })?;
        Ok(CreatedBridge {
            host_iface: fields[1].to_string(),
            peer_iface: fields[2].to_string(),
            service: fields[3].to_string(),
            fd,
        })
    }

    pub fn link(&mut self, up: bool) -> Result<()> {
        self.expect_ok(if up { "LINK up" } else { "LINK down" })?;
        Ok(())
    }

    pub fn destroy(&mut self) -> Result<()> {
        self.expect_ok("DESTROY")?;
        Ok(())
    }

    fn expect_ok(&mut self, command: &str) -> Result<String> {
        self.channel.send_line(command).map_err(helper_io)?;
        let line = self
            .channel
            .recv_line()
            .map_err(helper_io)?
            .ok_or_else(|| {
                TetherError::new(
                    ErrorCategory::HelperUnavailable,
                    "helper closed the connection",
                )
            })?;
        if let Some(rest) = line.strip_prefix("OK ") {
            Ok(rest.to_string())
        } else {
            Err(parse_error(&line))
        }
    }
}

fn helper_io(error: std::io::Error) -> TetherError {
    TetherError::new(
        ErrorCategory::HelperUnavailable,
        format!("helper protocol error: {error}"),
    )
}

fn parse_error(line: &str) -> TetherError {
    let mut parts = line.splitn(3, ' ');
    let tag = parts.next().unwrap_or_default();
    let category = parts.next().unwrap_or_default();
    let message = parts.next().unwrap_or("helper request failed");
    if tag != "ERR" {
        return TetherError::new(
            ErrorCategory::HelperUnavailable,
            format!("unexpected helper response: {line}"),
        );
    }
    let category = match category {
        "permission" => ErrorCategory::PermissionRequired,
        "busy" => ErrorCategory::UsbBusy,
        "network" => ErrorCategory::Network,
        "protocol" => ErrorCategory::ProtocolViolation,
        "configuration" => ErrorCategory::Configuration,
        "helper" => ErrorCategory::HelperUnavailable,
        _ => ErrorCategory::HelperUnavailable,
    };
    TetherError::new(category, message)
}

/// `FrameIo` implementation that owns the BPF descriptor passed by the helper
/// and forwards link transitions to the privileged process.
pub struct RemoteFrameIo {
    bpf: BpfSocket,
    helper: HelperClient,
    closed: bool,
}

impl RemoteFrameIo {
    pub fn new(bpf: BpfSocket, helper: HelperClient) -> Self {
        Self {
            bpf,
            helper,
            closed: false,
        }
    }

    pub fn shutdown(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let _ = self.helper.link(false);
        let _ = self.helper.destroy();
    }
}

impl FrameIo for RemoteFrameIo {
    fn recv_frame(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<Option<usize>> {
        self.bpf.recv_frame(buffer, timeout)
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.bpf.send_frame(frame)
    }

    fn set_link(&mut self, up: bool) -> Result<()> {
        // Ask the helper first; only report success once the OS interface
        // actually changed state.
        self.helper.link(up)?;
        self.bpf.set_link(up)
    }

    fn link_up(&self) -> bool {
        self.bpf.link_up()
    }
}

impl Drop for RemoteFrameIo {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn probe(path: &Path) -> Result<()> {
    let mut client = HelperClient::connect(path)?;
    client.hello()?;
    client.ping()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_helper_error_categories() {
        assert_eq!(
            parse_error("ERR permission no").category,
            ErrorCategory::PermissionRequired
        );
        assert_eq!(
            parse_error("ERR busy in use").category,
            ErrorCategory::UsbBusy
        );
        assert_eq!(
            parse_error("ERR network down").category,
            ErrorCategory::Network
        );
        assert_eq!(
            parse_error("ERR protocol bad").category,
            ErrorCategory::ProtocolViolation
        );
        assert_eq!(
            parse_error("ERR configuration bad").category,
            ErrorCategory::Configuration
        );
        assert_eq!(
            parse_error("ERR helper gone").category,
            ErrorCategory::HelperUnavailable
        );
        assert_eq!(
            parse_error("ERR unknown whatever").category,
            ErrorCategory::HelperUnavailable
        );
    }

    #[test]
    fn rejects_malformed_helper_responses() {
        let error = parse_error("OK not-an-error");
        assert_eq!(error.category, ErrorCategory::HelperUnavailable);
        assert!(error.message.contains("unexpected helper response"));
    }
}
