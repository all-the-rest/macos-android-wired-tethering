//! Host Ethernet bridge abstraction.
//!
//! Semantics follow the data path:
//! - `recv_frame` hands one frame from the host network stack to the engine
//!   (the engine then sends it to the phone).
//! - `send_frame` delivers one frame received from the phone to the host
//!   network stack.
//!
//! The real implementation is a macOS `feth` pair + BPF socket.  The mock
//! behaves like a full-duplex Ethernet peer and records both directions.

use std::collections::VecDeque;
use std::time::Duration;

use crate::error::{Result, TetherError};
use crate::ethernet::{self, ETHERNET_MAX_FRAME, ETHERNET_MIN_FRAME};

pub trait FrameIo: Send {
    /// Receive one frame from the host network stack.  `Ok(None)` means the
    /// timeout expired.
    fn recv_frame(&mut self, buffer: &mut [u8], timeout: Duration) -> Result<Option<usize>>;

    /// Deliver one phone-originated frame to the host network stack.
    fn send_frame(&mut self, frame: &[u8]) -> Result<()>;

    /// Reflect RNDIS link state on the host interface.
    fn set_link(&mut self, up: bool) -> Result<()>;

    fn link_up(&self) -> bool;
}

#[derive(Debug, Default)]
pub struct MockFrameIo {
    /// Frames queued by the test as if the host stack produced them.
    host_to_phone: VecDeque<Vec<u8>>,
    /// Frames delivered from the phone to the host stack.
    phone_to_host: Vec<Vec<u8>>,
    link_up: bool,
    link_changes: Vec<bool>,
    pub fail_send_next: Option<TetherError>,
    pub fail_recv_next: Option<TetherError>,
}

impl MockFrameIo {
    pub fn new() -> Self {
        Self {
            link_up: false,
            ..Self::default()
        }
    }

    /// Queue a frame that the macOS network stack wants to send to the phone.
    pub fn queue_from_host(&mut self, frame: Vec<u8>) {
        self.host_to_phone.push_back(frame);
    }

    /// Record a frame that should be handed to the host stack.  This is used
    /// when a test wants to preload the host-facing queue; normally frames
    /// arrive through `send_frame`.
    pub fn queue_to_host(&mut self, frame: Vec<u8>) {
        self.phone_to_host.push(frame);
    }

    /// Frames delivered from the phone to the host stack.
    pub fn sent_frames(&self) -> &[Vec<u8>] {
        &self.phone_to_host
    }

    pub fn take_sent_frames(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.phone_to_host)
    }

    pub fn link_changes(&self) -> &[bool] {
        &self.link_changes
    }

    pub fn pending_host_to_phone(&self) -> usize {
        self.host_to_phone.len()
    }
}

impl FrameIo for MockFrameIo {
    fn recv_frame(&mut self, buffer: &mut [u8], _timeout: Duration) -> Result<Option<usize>> {
        if let Some(error) = self.fail_recv_next.take() {
            return Err(error);
        }
        let Some(frame) = self.host_to_phone.pop_front() else {
            return Ok(None);
        };
        if frame.len() > buffer.len() {
            return Err(TetherError::network(
                "mock Ethernet frame does not fit into the receive buffer",
            ));
        }
        buffer[..frame.len()].copy_from_slice(&frame);
        Ok(Some(frame.len()))
    }

    fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        if let Some(error) = self.fail_send_next.take() {
            return Err(error);
        }
        if frame.len() < ETHERNET_MIN_FRAME || frame.len() > ETHERNET_MAX_FRAME {
            return Err(TetherError::network(
                "invalid Ethernet frame length for bridge",
            ));
        }
        let _ = ethernet::parse_ethernet(frame)?;
        self.phone_to_host.push(frame.to_vec());
        Ok(())
    }

    fn set_link(&mut self, up: bool) -> Result<()> {
        if self.link_up != up {
            self.link_changes.push(up);
        }
        self.link_up = up;
        Ok(())
    }

    fn link_up(&self) -> bool {
        self.link_up
    }
}
