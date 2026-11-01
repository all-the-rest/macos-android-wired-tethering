//! Deterministic mock RNDIS device used by unit and integration tests.
//!
//! This is a real protocol counterpart, not a queue of canned responses: it
//! parses the host's INITIALIZE/QUERY/SET/RESET/KEEPALIVE messages, validates
//! their lengths, maintains the packet filter and link state, and emits
//! standard completions and `RESPONSE_AVAILABLE` notifications.

use std::collections::VecDeque;
use std::time::Duration;

use crate::descriptor::{Protocol, TetherFunction, TransferType};
use crate::rndis::message::{
    self, MSG_HALT, MSG_INDICATE_STATUS, MSG_INITIALIZE, MSG_KEEPALIVE, MSG_QUERY, MSG_RESET,
    MSG_SET, STATUS_SUCCESS,
};
use crate::rndis::oids::{
    self, OID_802_3_CURRENT_ADDRESS, OID_802_3_MAXIMUM_LIST_SIZE, OID_802_3_PERMANENT_ADDRESS,
    OID_GEN_CURRENT_LOOKAHEAD, OID_GEN_CURRENT_PACKET_FILTER, OID_GEN_LINK_SPEED,
    OID_GEN_MAC_OPTIONS, OID_GEN_MAXIMUM_FRAME_SIZE, OID_GEN_MAXIMUM_TOTAL_SIZE,
    OID_GEN_MEDIA_CONNECT_STATUS, OID_GEN_PHYSICAL_MEDIUM, STATUS_INVALID_OID,
};
use crate::rndis::packet;
use crate::transport::{ControlInRequest, ControlOutRequest, UsbError, UsbTransport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlRecord {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub data: Vec<u8>,
}

pub struct MockRndisDevice {
    pub initialized: bool,
    pub max_transfer_size: u32,
    pub max_packets_per_transfer: u32,
    pub packet_alignment_exponent: u32,
    pub max_frame_size: u32,
    pub max_total_size: u32,
    pub link_speed: u32,
    pub link_up: bool,
    pub filter: u32,
    pub permanent_mac: [u8; 6],
    pub current_mac: [u8; 6],
    pub notifications_enabled: bool,
    pub physical_medium: u32,
    pub packet_filter_fallback: bool,
    pending_response: Option<Vec<u8>>,
    notifications: VecDeque<Vec<u8>>,
    to_host: VecDeque<Vec<u8>>,
    raw_to_host: VecDeque<Vec<u8>>,
    from_host: Vec<Vec<u8>>,
    control_log: Vec<ControlRecord>,
    pub failing_oids: Vec<u32>,
    pub fail_control_next: Option<UsbError>,
    pub fail_bulk_in_next: Option<UsbError>,
    pub fail_bulk_out_next: Option<UsbError>,
    pub fail_interrupt_next: Option<UsbError>,
    pub bulk_out_requests: Vec<Vec<u8>>,
    pub interrupt_requests: u64,
    pub bulk_in_requests: u64,
    pub force_response: Option<Vec<u8>>,
    pub malformed_data_in_next: bool,
}

impl Default for MockRndisDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRndisDevice {
    pub fn new() -> Self {
        Self {
            initialized: false,
            max_transfer_size: 0x4000,
            max_packets_per_transfer: 4,
            packet_alignment_exponent: 2,
            max_frame_size: 1514,
            max_total_size: 1514,
            link_speed: 100_000_000,
            link_up: true,
            filter: 0,
            permanent_mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x22],
            current_mac: [0x02, 0x00, 0x00, 0x00, 0x00, 0x11],
            notifications_enabled: true,
            physical_medium: 0,
            packet_filter_fallback: false,
            pending_response: None,
            notifications: VecDeque::new(),
            to_host: VecDeque::new(),
            raw_to_host: VecDeque::new(),
            from_host: Vec::new(),
            control_log: Vec::new(),
            failing_oids: Vec::new(),
            fail_control_next: None,
            fail_bulk_in_next: None,
            fail_bulk_out_next: None,
            fail_interrupt_next: None,
            bulk_out_requests: Vec::new(),
            interrupt_requests: 0,
            bulk_in_requests: 0,
            force_response: None,
            malformed_data_in_next: false,
        }
    }

    pub fn with_notifications(mut self, enabled: bool) -> Self {
        self.notifications_enabled = enabled;
        self
    }

    pub fn with_link_up(mut self, up: bool) -> Self {
        self.link_up = up;
        self
    }

    pub fn with_max_frame_size(mut self, size: u32) -> Self {
        self.max_frame_size = size;
        self
    }

    pub fn with_max_total_size(mut self, size: u32) -> Self {
        self.max_total_size = size;
        self
    }

    pub fn with_mac(mut self, mac: [u8; 6]) -> Self {
        self.current_mac = mac;
        self
    }

    pub fn with_host_mac(mut self, mac: [u8; 6]) -> Self {
        self.permanent_mac = mac;
        self
    }

    pub fn queue_frame_to_host(&mut self, frame: Vec<u8>) {
        self.to_host.push_back(frame);
    }

    pub fn queue_raw_transfer_to_host(&mut self, bytes: Vec<u8>) {
        self.raw_to_host.push_back(bytes);
    }

    pub fn received_frames(&self) -> &[Vec<u8>] {
        &self.from_host
    }

    pub fn take_received_frames(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.from_host)
    }

    pub fn control_log(&self) -> &[ControlRecord] {
        &self.control_log
    }

    pub fn pending_notifications(&self) -> usize {
        self.notifications.len()
    }

    pub fn inject_indication(&mut self, status: u32, buffer: &[u8]) {
        let status_offset = 20usize;
        let length = status_offset + buffer.len();
        let mut response = Vec::with_capacity(length);
        response.extend_from_slice(&MSG_INDICATE_STATUS.to_le_bytes());
        response.extend_from_slice(&(length as u32).to_le_bytes());
        response.extend_from_slice(&status.to_le_bytes());
        response.extend_from_slice(&(buffer.len() as u32).to_le_bytes());
        response.extend_from_slice(&(status_offset as u32).to_le_bytes());
        response.extend_from_slice(buffer);
        self.pending_response = Some(response);
        if self.notifications_enabled {
            self.notifications.push_back(vec![0x01]);
        }
    }

    pub fn inject_speed_change(&mut self, down_bps: u32, up_bps: u32) {
        let mut notification = vec![0xa1u8, 0x2a, 0, 0, 0, 0, 0, 8];
        notification.extend_from_slice(&down_bps.to_le_bytes());
        notification.extend_from_slice(&up_bps.to_le_bytes());
        self.notifications.push_back(notification);
    }

    pub fn inject_response_available(&mut self) {
        if self.notifications_enabled {
            self.notifications.push_back(vec![0x01]);
        }
    }

    fn queue_completion(&mut self, response: Vec<u8>) {
        self.pending_response = Some(response);
        if self.notifications_enabled {
            self.notifications.push_back(vec![0x01]);
        }
    }

    fn init_completion(&self, request_id: u32) -> Vec<u8> {
        let mut response = Vec::with_capacity(52);
        response.extend_from_slice(&message::MSG_INITIALIZE_CMPLT.to_le_bytes());
        response.extend_from_slice(&52u32.to_le_bytes());
        response.extend_from_slice(&request_id.to_le_bytes());
        response.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
        response.extend_from_slice(&1u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&1u32.to_le_bytes()); // connectionless
        response.extend_from_slice(&0u32.to_le_bytes()); // 802.3
        response.extend_from_slice(&self.max_packets_per_transfer.to_le_bytes());
        response.extend_from_slice(&self.max_transfer_size.to_le_bytes());
        response.extend_from_slice(&self.packet_alignment_exponent.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response
    }

    fn query_value(&self, oid: u32) -> Option<Vec<u8>> {
        if self.failing_oids.contains(&oid) {
            return None;
        }
        let value = match oid {
            OID_GEN_MAXIMUM_FRAME_SIZE => self.max_frame_size.to_le_bytes().to_vec(),
            OID_GEN_LINK_SPEED => self.link_speed.to_le_bytes().to_vec(),
            OID_GEN_MEDIA_CONNECT_STATUS => {
                if self.link_up {
                    oids::MEDIA_CONNECTED.to_le_bytes().to_vec()
                } else {
                    oids::MEDIA_DISCONNECTED.to_le_bytes().to_vec()
                }
            }
            OID_802_3_PERMANENT_ADDRESS => self.permanent_mac.to_vec(),
            OID_802_3_CURRENT_ADDRESS => self.current_mac.to_vec(),
            OID_802_3_MAXIMUM_LIST_SIZE => 32u32.to_le_bytes().to_vec(),
            OID_GEN_MAXIMUM_TOTAL_SIZE => self.max_total_size.to_le_bytes().to_vec(),
            OID_GEN_PHYSICAL_MEDIUM => self.physical_medium.to_le_bytes().to_vec(),
            OID_GEN_CURRENT_PACKET_FILTER => self.filter.to_le_bytes().to_vec(),
            OID_GEN_CURRENT_LOOKAHEAD => self.max_frame_size.to_le_bytes().to_vec(),
            OID_GEN_MAC_OPTIONS => 0u32.to_le_bytes().to_vec(),
            _ => return None,
        };
        Some(value)
    }

    fn query_completion(&self, request_id: u32, status: u32, information: &[u8]) -> Vec<u8> {
        let length = 24 + information.len();
        let mut response = Vec::with_capacity(length);
        response.extend_from_slice(&message::MSG_QUERY_CMPLT.to_le_bytes());
        response.extend_from_slice(&(length as u32).to_le_bytes());
        response.extend_from_slice(&request_id.to_le_bytes());
        response.extend_from_slice(&status.to_le_bytes());
        response.extend_from_slice(&(information.len() as u32).to_le_bytes());
        response.extend_from_slice(&message::QUERY_COMPLETE_INLINE_DATA_OFFSET.to_le_bytes());
        response.extend_from_slice(information);
        response
    }

    fn handle_control(&mut self, data: &[u8]) -> Result<(), UsbError> {
        let header = message::parse_header(data).map_err(|error| UsbError::Other(error.message))?;
        match header.kind {
            MSG_INITIALIZE => {
                if header.length < 24 {
                    return Err(UsbError::Other("short INITIALIZE".into()));
                }
                let major = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
                if major != 1 {
                    let response = self.init_completion(header.request_id);
                    // Rewrite status to not supported would require a
                    // different fixture; report a stall instead.
                    let _ = response;
                    return Err(UsbError::Other("unsupported RNDIS major version".into()));
                }
                self.initialized = true;
                let response = self.init_completion(header.request_id);
                self.queue_completion(response);
            }
            MSG_QUERY => {
                if header.length < 28 {
                    return Err(UsbError::Other("short QUERY".into()));
                }
                let oid = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
                if !self.initialized {
                    let response =
                        self.query_completion(header.request_id, oids::STATUS_FAILURE, &[]);
                    self.queue_completion(response);
                    return Ok(());
                }
                match self.query_value(oid) {
                    Some(value) => {
                        let response = self.query_completion(header.request_id, 0, &value);
                        self.queue_completion(response);
                    }
                    None => {
                        let response =
                            self.query_completion(header.request_id, STATUS_INVALID_OID, &[]);
                        self.queue_completion(response);
                    }
                }
            }
            MSG_SET => {
                if header.length < 28 {
                    return Err(UsbError::Other("short SET".into()));
                }
                let oid = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
                let info_len =
                    u32::from_le_bytes([data[16], data[17], data[18], data[19]]) as usize;
                let offset = u32::from_le_bytes([data[20], data[21], data[22], data[23]]) as usize;
                let start = 8usize
                    .checked_add(offset)
                    .ok_or_else(|| UsbError::Other("SET offset overflow".into()))?;
                let end = start
                    .checked_add(info_len)
                    .ok_or_else(|| UsbError::Other("SET length overflow".into()))?;
                if end > data.len() || start < 28 {
                    return Err(UsbError::Other("SET information outside message".into()));
                }
                if oid == OID_GEN_CURRENT_PACKET_FILTER && info_len == 4 {
                    let requested = u32::from_le_bytes([
                        data[start],
                        data[start + 1],
                        data[start + 2],
                        data[start + 3],
                    ]);
                    if self.packet_filter_fallback && requested & oids::PACKET_TYPE_PROMISCUOUS != 0
                    {
                        let mut response = Vec::with_capacity(16);
                        response.extend_from_slice(&message::MSG_SET_CMPLT.to_le_bytes());
                        response.extend_from_slice(&16u32.to_le_bytes());
                        response.extend_from_slice(&header.request_id.to_le_bytes());
                        response.extend_from_slice(&oids::STATUS_NOT_SUPPORTED.to_le_bytes());
                        self.queue_completion(response);
                        return Ok(());
                    }
                    self.filter = requested;
                }
                let mut response = Vec::with_capacity(16);
                response.extend_from_slice(&message::MSG_SET_CMPLT.to_le_bytes());
                response.extend_from_slice(&16u32.to_le_bytes());
                response.extend_from_slice(&header.request_id.to_le_bytes());
                response.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
                self.queue_completion(response);
            }
            MSG_RESET => {
                self.initialized = false;
                self.filter = 0;
                let mut response = Vec::with_capacity(16);
                response.extend_from_slice(&message::MSG_RESET_CMPLT.to_le_bytes());
                response.extend_from_slice(&16u32.to_le_bytes());
                response.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
                response.extend_from_slice(&0u32.to_le_bytes());
                self.queue_completion(response);
            }
            MSG_KEEPALIVE => {
                let mut response = Vec::with_capacity(16);
                response.extend_from_slice(&message::MSG_KEEPALIVE_CMPLT.to_le_bytes());
                response.extend_from_slice(&16u32.to_le_bytes());
                response.extend_from_slice(&header.request_id.to_le_bytes());
                response.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
                self.queue_completion(response);
            }
            MSG_HALT => {
                self.initialized = false;
                self.filter = 0;
            }
            _ => {
                return Err(UsbError::Other(format!(
                    "mock does not implement RNDIS message 0x{:08x}",
                    header.kind
                )));
            }
        }
        Ok(())
    }
}

impl UsbTransport for MockRndisDevice {
    fn control_out(&mut self, request: &ControlOutRequest<'_>) -> Result<usize, UsbError> {
        if let Some(error) = self.fail_control_next.take() {
            return Err(error);
        }
        self.control_log.push(ControlRecord {
            request_type: request.request_type,
            request: request.request,
            value: request.value,
            index: request.index,
            data: request.data.to_vec(),
        });
        if request.request_type == 0x21 && request.request == 0x00 {
            self.handle_control(request.data)?;
        } else {
            return Err(UsbError::Other(
                "unexpected control-out request in mock".into(),
            ));
        }
        Ok(request.data.len())
    }

    fn control_in(
        &mut self,
        request: &ControlInRequest,
        buffer: &mut [u8],
    ) -> Result<usize, UsbError> {
        let expected = if let Some(response) = self.force_response.take() {
            // A forced response replaces the mock's normal completion for this
            // request; discard any queued real response so negative tests are
            // deterministic.
            self.pending_response = None;
            self.notifications.clear();
            response
        } else if self.control_log.is_empty() {
            return Err(UsbError::Stall);
        } else if request.request_type != 0xa1 || request.request != 0x01 {
            return Err(UsbError::Other(
                "unexpected control-in request in mock".into(),
            ));
        } else if let Some(response) = self.pending_response.take() {
            response
        } else {
            return Err(UsbError::Stall);
        };
        if expected.len() > buffer.len() {
            return Err(UsbError::Other(
                "mock response does not fit into the control buffer".into(),
            ));
        }
        buffer[..expected.len()].copy_from_slice(&expected);
        Ok(expected.len())
    }

    fn bulk_out(
        &mut self,
        endpoint: u8,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        if let Some(error) = self.fail_bulk_out_next.take() {
            return Err(error);
        }
        if endpoint & 0x80 != 0 {
            return Err(UsbError::Other("bulk OUT on an IN endpoint".into()));
        }
        self.bulk_out_requests.push(data.to_vec());
        let packets =
            message::split_packet_transfer(data).map_err(|error| UsbError::Other(error.message))?;
        for packet in packets {
            self.from_host.push(packet.data.to_vec());
        }
        Ok(data.len())
    }

    fn bulk_in(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.bulk_in_requests += 1;
        if let Some(error) = self.fail_bulk_in_next.take() {
            return Err(error);
        }
        if endpoint & 0x80 == 0 {
            return Err(UsbError::Other("bulk IN on an OUT endpoint".into()));
        }
        if self.malformed_data_in_next {
            self.malformed_data_in_next = false;
            let bytes = vec![0xffu8; 44];
            if bytes.len() > buffer.len() {
                return Err(UsbError::Other("mock buffer too small".into()));
            }
            buffer[..bytes.len()].copy_from_slice(&bytes);
            return Ok(bytes.len());
        }
        if let Some(raw) = self.raw_to_host.pop_front() {
            if raw.len() > buffer.len() {
                return Err(UsbError::Other("mock raw transfer too large".into()));
            }
            buffer[..raw.len()].copy_from_slice(&raw);
            return Ok(raw.len());
        }
        if self.to_host.is_empty() {
            return Err(UsbError::Timeout);
        }
        let count = self
            .to_host
            .len()
            .min(self.max_packets_per_transfer.max(1) as usize);
        let mut frames = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(frame) = self.to_host.pop_front() {
                frames.push(frame);
            }
        }
        let refs: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
        let encoded = packet::encode_transfer(
            &refs,
            self.max_packets_per_transfer as usize,
            self.max_transfer_size as usize,
            1usize << self.packet_alignment_exponent,
            512,
        )
        .map_err(|error| UsbError::Other(error.message))?;
        if encoded.bytes.len() > buffer.len() {
            return Err(UsbError::Other(
                "mock transfer does not fit into the bulk buffer".into(),
            ));
        }
        buffer[..encoded.bytes.len()].copy_from_slice(&encoded.bytes);
        Ok(encoded.bytes.len())
    }

    fn interrupt_in(
        &mut self,
        endpoint: u8,
        buffer: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, UsbError> {
        self.interrupt_requests += 1;
        if let Some(error) = self.fail_interrupt_next.take() {
            return Err(error);
        }
        if endpoint & 0x80 == 0 {
            return Err(UsbError::Other("interrupt IN on an OUT endpoint".into()));
        }
        let Some(notification) = self.notifications.pop_front() else {
            return Err(UsbError::Timeout);
        };
        if notification.len() > buffer.len() {
            return Err(UsbError::Other("notification too large".into()));
        }
        buffer[..notification.len()].copy_from_slice(&notification);
        Ok(notification.len())
    }
}

/// Build the descriptor-derived function description used together with the
/// mock device.  The endpoints match the mock's expectations and are also
/// used by the integration tests to exercise `RndisSession::new`.
pub fn mock_function(interrupt: bool) -> TetherFunction {
    use crate::descriptor::{ControlInterface, DataInterface, Endpoint};

    TetherFunction {
        protocol: Protocol::Rndis,
        configuration_value: 1,
        control: ControlInterface {
            number: 0,
            alt: 0,
            class: 0xe0,
            subclass: 1,
            protocol: 3,
            interrupt_in: if interrupt {
                Some(Endpoint {
                    address: 0x82,
                    transfer_type: TransferType::Interrupt,
                    max_packet_size: 8,
                    interval: 9,
                    extra: Vec::new(),
                })
            } else {
                None
            },
        },
        data: DataInterface {
            number: 1,
            alt: 0,
            bulk_in: Endpoint {
                address: 0x81,
                transfer_type: TransferType::Bulk,
                max_packet_size: 512,
                interval: 0,
                extra: Vec::new(),
            },
            bulk_out: Endpoint {
                address: 0x01,
                transfer_type: TransferType::Bulk,
                max_packet_size: 512,
                interval: 0,
                extra: Vec::new(),
            },
        },
        union: None,
        iad: None,
        ethernet: None,
        ncm: None,
        functionals: Vec::new(),
        notes: Vec::new(),
    }
}
