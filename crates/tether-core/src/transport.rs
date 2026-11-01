//! USB transport abstraction.
//!
//! The trait is intentionally narrow: control transfers on endpoint zero,
//! bulk transfers and interrupt transfers on the endpoints selected by the
//! descriptor layer.  A `rusb` implementation lives in `tether-usb`; the
//! deterministic mock lives in `tether-core::rndis::mock`.

use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbError {
    Timeout,
    Stall,
    Disconnected,
    Permission,
    Busy,
    Other(String),
}

impl UsbError {
    pub fn message(&self) -> &str {
        match self {
            Self::Timeout => "transfer timed out",
            Self::Stall => "endpoint stalled",
            Self::Disconnected => "device disconnected",
            Self::Permission => "permission denied",
            Self::Busy => "interface busy",
            Self::Other(message) => message,
        }
    }
}

impl std::fmt::Display for UsbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for UsbError {}

#[derive(Debug, Clone, Copy)]
pub struct ControlOutRequest<'a> {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub data: &'a [u8],
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct ControlInRequest {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub timeout: Duration,
}

pub trait UsbTransport: Send {
    fn control_out(&mut self, request: &ControlOutRequest<'_>) -> Result<usize, UsbError>;

    fn control_in(
        &mut self,
        request: &ControlInRequest,
        buffer: &mut [u8],
    ) -> Result<usize, UsbError>;

    fn bulk_out(&mut self, endpoint: u8, data: &[u8], timeout: Duration)
        -> Result<usize, UsbError>;

    fn bulk_in(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;

    fn interrupt_in(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError>;
}
