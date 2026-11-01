//! libusb implementation of the hardware-independent USB transport trait.

use std::time::Duration;

use rusb::{Context, DeviceHandle};
use tether_core::transport::{ControlInRequest, ControlOutRequest, UsbError, UsbTransport};

use crate::discover::{map_rusb_result, OpenedDevice};

pub struct LibusbTransport {
    handle: DeviceHandle<Context>,
}

impl LibusbTransport {
    pub fn new(opened: OpenedDevice) -> Self {
        Self {
            handle: opened.handle,
        }
    }

    pub fn handle(&self) -> &DeviceHandle<Context> {
        &self.handle
    }
}

impl UsbTransport for LibusbTransport {
    fn control_out(&mut self, request: &ControlOutRequest<'_>) -> Result<usize, UsbError> {
        self.handle
            .write_control(
                request.request_type,
                request.request,
                request.value,
                request.index,
                request.data,
                request.timeout,
            )
            .map_err(map_rusb_result)
    }

    fn control_in(
        &mut self,
        request: &ControlInRequest,
        buffer: &mut [u8],
    ) -> Result<usize, UsbError> {
        self.handle
            .read_control(
                request.request_type,
                request.request,
                request.value,
                request.index,
                buffer,
                request.timeout,
            )
            .map_err(map_rusb_result)
    }

    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .write_bulk(endpoint, data, timeout)
            .map_err(map_rusb_result)
    }

    fn bulk_in(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .read_bulk(endpoint, buffer, timeout)
            .map_err(map_rusb_result)
    }

    fn interrupt_in(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.handle
            .read_interrupt(endpoint, buffer, timeout)
            .map_err(map_rusb_result)
    }
}
