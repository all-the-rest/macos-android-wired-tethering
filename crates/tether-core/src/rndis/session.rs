//! RNDIS host session and state machine.
//!
//! The flow follows [MS-RNDIS] section 3.1:
//! INITIALIZE -> query device properties -> SET packet filter -> data.  A
//! control command is transferred with `SEND_ENCAPSULATED_COMMAND`; the
//! completion is fetched with `GET_ENCAPSULATED_RESPONSE`.  The interrupt
//! endpoint is drained when present, but polling the control channel is kept
//! as a fallback because several real devices only answer when polled
//! (the same robustness strategy documented by Linux `rndis_host`).

use std::collections::VecDeque;
use std::thread;
use std::time::{Duration, Instant};

use crate::descriptor::{TetherFunction, TransferType};
use crate::error::{ErrorCategory, Result, TetherError};
use crate::ethernet;
use crate::rndis::message::{
    self, IndicateStatus, ResetComplete, MSG_INDICATE_STATUS, MSG_INITIALIZE_CMPLT, MSG_KEEPALIVE,
    MSG_KEEPALIVE_CMPLT, MSG_QUERY_CMPLT, MSG_RESET_CMPLT, MSG_SET_CMPLT, STATUS_MEDIA_CONNECT,
    STATUS_MEDIA_DISCONNECT, STATUS_SUCCESS,
};
use crate::rndis::oids::{
    self, DEFAULT_PACKET_FILTER, OID_802_3_CURRENT_ADDRESS, OID_802_3_PERMANENT_ADDRESS,
    OID_GEN_CURRENT_LOOKAHEAD, OID_GEN_CURRENT_PACKET_FILTER, OID_GEN_LINK_SPEED,
    OID_GEN_MAXIMUM_FRAME_SIZE, OID_GEN_MAXIMUM_TOTAL_SIZE, OID_GEN_MEDIA_CONNECT_STATUS,
    OID_GEN_PHYSICAL_MEDIUM,
};
use crate::rndis::packet::{self, PacketReassembler};
use crate::transport::{ControlInRequest, ControlOutRequest, UsbError, UsbTransport};

const CONTROL_REQUEST_SEND: u8 = 0x00;
const CONTROL_REQUEST_GET: u8 = 0x01;
const CLASS_INTERFACE_OUT: u8 = 0x21;
const CLASS_INTERFACE_IN: u8 = 0xa1;
const RESPONSE_AVAILABLE: u8 = 0x01;
const CONNECTION_SPEED_CHANGE: u8 = 0x2a;

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub control_timeout: Duration,
    pub data_timeout: Duration,
    pub notification_timeout: Duration,
    pub poll_interval: Duration,
    pub max_transfer_size: u32,
    pub max_frame_size: u32,
    pub keepalive_interval: Duration,
    pub packet_filter: u32,
    pub host_mac_fallback: [u8; 6],
    pub allow_reduced_filter: bool,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            control_timeout: Duration::from_secs(5),
            data_timeout: Duration::from_millis(100),
            notification_timeout: Duration::from_millis(250),
            poll_interval: Duration::from_millis(20),
            max_transfer_size: 0x4000,
            max_frame_size: 1514,
            keepalive_interval: Duration::from_secs(5),
            packet_filter: DEFAULT_PACKET_FILTER,
            host_mac_fallback: [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            allow_reduced_filter: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub protocol: crate::descriptor::Protocol,
    pub host_mac: [u8; 6],
    pub host_mac_from_device: bool,
    pub reported_host_mac: Option<[u8; 6]>,
    pub reported_device_mac: Option<[u8; 6]>,
    pub max_frame_size: u32,
    pub max_transfer_size: u32,
    pub max_packets_per_transfer: u32,
    pub packet_alignment: usize,
    pub link_speed_100bps: Option<u32>,
    pub media_connected: bool,
    pub physical_medium: Option<u32>,
    pub packet_filter: u32,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionStats {
    pub control_requests: u64,
    pub control_timeouts: u64,
    pub frames_rx: u64,
    pub frames_tx: u64,
    pub bytes_rx: u64,
    pub bytes_tx: u64,
    pub malformed_frames: u64,
    pub keepalives_sent: u64,
    pub notifications: u64,
    pub resets: u64,
    pub stale_responses: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notification {
    IndicateStatus { status: u32, buffer: Vec<u8> },
    ConnectionSpeedChange { down_bps: u32, up_bps: u32 },
    Other(u8),
}

pub struct RndisSession<T: UsbTransport> {
    transport: T,
    control_interface: u8,
    data_interface: u8,
    bulk_in: u8,
    bulk_out: u8,
    bulk_max_packet: usize,
    interrupt_in: Option<u8>,
    config: SessionConfig,
    next_request_id: u32,
    info: Option<SessionInfo>,
    initialized: bool,
    reassembler: PacketReassembler,
    pending_notifications: VecDeque<Notification>,
    last_control: Instant,
    stats: SessionStats,
}

impl<T: UsbTransport> RndisSession<T> {
    pub fn new(transport: T, function: &TetherFunction, config: SessionConfig) -> Result<Self> {
        if function.data.bulk_in.transfer_type != TransferType::Bulk
            || function.data.bulk_out.transfer_type != TransferType::Bulk
        {
            return Err(TetherError::unsupported(
                "RNDIS data interface does not expose bulk endpoints",
            ));
        }
        if !function.data.bulk_in.is_in() || !function.data.bulk_out.is_out() {
            return Err(TetherError::unsupported(
                "RNDIS bulk endpoint directions are inconsistent",
            ));
        }
        let bulk_max_packet = usize::from(
            function
                .data
                .bulk_out
                .max_packet_size
                .min(function.data.bulk_in.max_packet_size),
        )
        .max(1);
        Ok(Self {
            transport,
            control_interface: function.control.number,
            data_interface: function.data.number,
            bulk_in: function.data.bulk_in.address,
            bulk_out: function.data.bulk_out.address,
            bulk_max_packet,
            interrupt_in: function
                .control
                .interrupt_in
                .as_ref()
                .map(|endpoint| endpoint.address),
            config,
            next_request_id: 1,
            info: None,
            initialized: false,
            reassembler: PacketReassembler::new(2 * 1024 * 1024),
            pending_notifications: VecDeque::new(),
            last_control: Instant::now(),
            stats: SessionStats::default(),
        })
    }

    pub fn info(&self) -> Option<&SessionInfo> {
        self.info.as_ref()
    }

    pub fn stats(&self) -> &SessionStats {
        &self.stats
    }

    pub fn stats_mut(&mut self) -> &mut SessionStats {
        &mut self.stats
    }

    pub fn initialized(&self) -> bool {
        self.initialized
    }

    pub fn control_interface(&self) -> u8 {
        self.control_interface
    }

    pub fn data_interface(&self) -> u8 {
        self.data_interface
    }

    pub fn media_connected(&self) -> bool {
        self.info
            .as_ref()
            .map(|info| info.media_connected)
            .unwrap_or(false)
    }

    pub fn take_notifications(&mut self) -> Vec<Notification> {
        self.pending_notifications.drain(..).collect()
    }

    /// Complete initialization and return the negotiated session parameters.
    pub fn initialize(&mut self) -> Result<SessionInfo> {
        if self.initialized {
            return Err(TetherError::new(
                ErrorCategory::Internal,
                "RNDIS session is already initialized",
            ));
        }
        self.reassembler.clear();
        self.pending_notifications.clear();
        let request_id = self.next_request_id();
        let request = message::encode_initialize_request(
            request_id,
            self.config.max_transfer_size.max(0x2c),
        )?;
        let response = self.command(request, MSG_INITIALIZE_CMPLT, Some(request_id))?;
        let completion = message::parse_initialize_complete(&response)?;
        self.check_status(completion.status, "RNDIS INITIALIZE")?;
        if completion.request_id != request_id {
            return Err(TetherError::protocol(
                "RNDIS INITIALIZE completion has the wrong request id",
            ));
        }
        if completion.medium != 0 {
            return Err(TetherError::unsupported(format!(
                "RNDIS medium {} is not 802.3",
                completion.medium
            )));
        }
        if completion.device_flags & 0x1 == 0 {
            return Err(TetherError::unsupported(
                "RNDIS device did not negotiate connectionless 802.3 mode",
            ));
        }
        if completion.max_packets_per_transfer == 0 {
            return Err(TetherError::protocol(
                "RNDIS MaxPacketsPerTransfer must be at least 1",
            ));
        }
        if completion.max_transfer_size < 44 {
            return Err(TetherError::protocol(
                "RNDIS MaxTransferSize is too small for a packet header",
            ));
        }
        let max_transfer_size = self
            .config
            .max_transfer_size
            .min(completion.max_transfer_size)
            .max(44);
        let packet_alignment = 1usize << completion.packet_alignment_exponent;
        let mut warnings = Vec::new();
        if max_transfer_size < self.config.max_frame_size + 44 {
            warnings.push(format!(
                "device MaxTransferSize {} is smaller than frame size {} + header",
                max_transfer_size, self.config.max_frame_size
            ));
        }

        // Query the designated host Ethernet address.  RNDIS uses the
        // permanent address as the address the *host* should use; Linux's
        // rndis_host does the same.  Fall back to the current address and
        // finally to a stable local address.
        let mut reported_host_mac = None;
        let mut reported_device_mac = None;
        let mut host_mac_from_device = false;
        let mut host_mac = self.config.host_mac_fallback;
        match self.query_oid(OID_802_3_PERMANENT_ADDRESS) {
            Ok(bytes) => {
                if let Some(mac) = oids::parse_mac(&bytes) {
                    reported_host_mac = Some(mac);
                    if crate::descriptor::valid_host_mac(&mac) {
                        host_mac = mac;
                        host_mac_from_device = true;
                    } else {
                        warnings.push("device reported an invalid unicast host MAC".to_string());
                    }
                } else {
                    warnings
                        .push("OID_802_3_PERMANENT_ADDRESS had an unexpected length".to_string());
                }
            }
            Err(error) => warnings.push(format!(
                "OID_802_3_PERMANENT_ADDRESS unavailable: {}",
                error.message
            )),
        }
        if let Ok(bytes) = self.query_oid(OID_802_3_CURRENT_ADDRESS) {
            if let Some(mac) = oids::parse_mac(&bytes) {
                reported_device_mac = Some(mac);
                if !host_mac_from_device && crate::descriptor::valid_host_mac(&mac) {
                    host_mac = mac;
                    host_mac_from_device = true;
                }
            }
        }
        if !host_mac_from_device {
            warnings.push(format!(
                "using locally generated host MAC {}",
                ethernet::MacAddr(host_mac)
            ));
        }

        let max_frame_size = match self.query_u32(OID_GEN_MAXIMUM_FRAME_SIZE) {
            Ok(value) if (14..=1518).contains(&value) => value,
            Ok(value) => {
                warnings.push(format!(
                    "OID_GEN_MAXIMUM_FRAME_SIZE {} outside 14..1518; using 1514",
                    value
                ));
                1514
            }
            Err(error) => {
                warnings.push(format!(
                    "OID_GEN_MAXIMUM_FRAME_SIZE unavailable: {}",
                    error.message
                ));
                self.config.max_frame_size.clamp(14, 1518)
            }
        };
        let link_speed_100bps = self.query_u32(OID_GEN_LINK_SPEED).ok();
        if link_speed_100bps.is_none() {
            warnings.push("OID_GEN_LINK_SPEED unavailable".to_string());
        }
        let physical_medium = self.query_u32(OID_GEN_PHYSICAL_MEDIUM).ok();
        if let Ok(total) = self.query_u32(OID_GEN_MAXIMUM_TOTAL_SIZE) {
            if total < max_frame_size {
                warnings.push(format!(
                    "OID_GEN_MAXIMUM_TOTAL_SIZE {} smaller than frame size {}",
                    total, max_frame_size
                ));
            }
        } else {
            warnings.push("OID_GEN_MAXIMUM_TOTAL_SIZE unavailable".to_string());
        }
        let media_connected = match self.query_u32(OID_GEN_MEDIA_CONNECT_STATUS) {
            Ok(oids::MEDIA_CONNECTED) => true,
            Ok(oids::MEDIA_DISCONNECTED) => false,
            Ok(other) => {
                warnings.push(format!(
                    "unknown OID_GEN_MEDIA_CONNECT_STATUS value {}",
                    other
                ));
                true
            }
            Err(error) => {
                warnings.push(format!(
                    "OID_GEN_MEDIA_CONNECT_STATUS unavailable: {}",
                    error.message
                ));
                true
            }
        };

        // A device may reject promiscuous filters.  Try progressively
        // narrower filters, but never silently proceed without a filter.
        let filters = if self.config.allow_reduced_filter {
            vec![
                self.config.packet_filter,
                oids::PACKET_TYPE_DIRECTED
                    | oids::PACKET_TYPE_BROADCAST
                    | oids::PACKET_TYPE_ALL_MULTICAST,
                oids::PACKET_TYPE_DIRECTED | oids::PACKET_TYPE_BROADCAST,
            ]
        } else {
            vec![self.config.packet_filter]
        };
        let mut applied_filter = None;
        let mut filter_errors = Vec::new();
        for filter in filters {
            match self.set_oid(OID_GEN_CURRENT_PACKET_FILTER, &filter.to_le_bytes()) {
                Ok(()) => {
                    applied_filter = Some(filter);
                    break;
                }
                Err(error) => filter_errors.push(format!("0x{filter:08x}: {}", error.message)),
            }
        }
        let packet_filter = applied_filter.ok_or_else(|| {
            TetherError::protocol(format!(
                "device rejected all packet filters ({})",
                filter_errors.join("; ")
            ))
        })?;
        if packet_filter != self.config.packet_filter {
            warnings.push(format!(
                "device rejected the preferred packet filter; using 0x{packet_filter:08x}"
            ));
        }
        // Optional lookahead improves throughput on some Windows-derived
        // devices; failure is harmless and therefore only a warning.
        if self
            .set_oid(OID_GEN_CURRENT_LOOKAHEAD, &max_frame_size.to_le_bytes())
            .is_err()
        {
            warnings.push("OID_GEN_CURRENT_LOOKAHEAD could not be set".to_string());
        }

        let info = SessionInfo {
            protocol: crate::descriptor::Protocol::Rndis,
            host_mac,
            host_mac_from_device,
            reported_host_mac,
            reported_device_mac,
            max_frame_size,
            max_transfer_size,
            max_packets_per_transfer: completion.max_packets_per_transfer,
            packet_alignment,
            link_speed_100bps,
            media_connected,
            physical_medium,
            packet_filter,
            warnings,
        };
        self.info = Some(info.clone());
        self.initialized = true;
        self.last_control = Instant::now();
        Ok(info)
    }

    /// Receive up to one bulk transfer worth of Ethernet frames.  A timeout is
    /// not an error; it simply means no frame is currently available.
    pub fn receive_frames(&mut self, timeout: Duration) -> Result<Vec<Vec<u8>>> {
        let mut buffer = vec![0u8; self.max_transfer_size()];
        let received = match self.transport.bulk_in(self.bulk_in, &mut buffer, timeout) {
            Ok(0) => return Ok(Vec::new()),
            Ok(received) => received,
            Err(UsbError::Timeout) => return Ok(Vec::new()),
            Err(error) => return Err(error.into_tether("RNDIS bulk IN")),
        };
        if received > buffer.len() {
            return Err(TetherError::protocol(
                "USB transport returned more bytes than the buffer size",
            ));
        }
        let frames = self.reassembler.push(&buffer[..received])?;
        let mut accepted = Vec::with_capacity(frames.len());
        for frame in frames {
            match ethernet::parse_ethernet(&frame) {
                Ok(parsed) if frame.len() <= self.max_frame_size() as usize => {
                    self.stats.frames_rx += 1;
                    self.stats.bytes_rx += frame.len() as u64;
                    if parsed.ethertype == ethernet::ETHERTYPE_ARP {
                        // nothing to do; parsing above is the validation
                    }
                    accepted.push(frame);
                }
                Ok(_) => {
                    self.stats.malformed_frames += 1;
                }
                Err(_) => {
                    self.stats.malformed_frames += 1;
                }
            }
        }
        Ok(accepted)
    }

    /// Send any number of Ethernet frames, batching them according to the
    /// device's advertised limits.  Returns the number of frames sent.
    pub fn send_frames(&mut self, frames: &[&[u8]]) -> Result<usize> {
        let mut offset = 0usize;
        while offset < frames.len() {
            for frame in &frames[offset..] {
                if frame.len() > self.max_frame_size() as usize {
                    return Err(TetherError::protocol(format!(
                        "Ethernet frame of {} bytes exceeds negotiated frame size {}",
                        frame.len(),
                        self.max_frame_size()
                    )));
                }
                if frame.len() < ethernet::ETHERNET_MIN_FRAME {
                    return Err(TetherError::protocol(
                        "Ethernet frame is shorter than 14 bytes",
                    ));
                }
            }
            let info = self.info.as_ref().ok_or_else(|| {
                TetherError::new(ErrorCategory::Internal, "RNDIS session is not initialized")
            })?;
            let encoded = packet::encode_transfer(
                &frames[offset..],
                info.max_packets_per_transfer as usize,
                info.max_transfer_size as usize,
                info.packet_alignment,
                self.bulk_max_packet,
            )?;
            if encoded.frames == 0 {
                break;
            }
            let written = self
                .transport
                .bulk_out(self.bulk_out, &encoded.bytes, self.config.data_timeout)
                .map_err(|error| error.into_tether("RNDIS bulk OUT"))?;
            if written != encoded.bytes.len() {
                return Err(TetherError::new(
                    ErrorCategory::UsbTransfer,
                    format!(
                        "short RNDIS bulk OUT transfer: {} of {} bytes",
                        written,
                        encoded.bytes.len()
                    ),
                ));
            }
            self.stats.frames_tx += encoded.frames as u64;
            self.stats.bytes_tx += frames[offset..offset + encoded.frames]
                .iter()
                .map(|frame| frame.len() as u64)
                .sum::<u64>();
            offset += encoded.frames;
        }
        Ok(offset)
    }

    pub fn send_frame(&mut self, frame: &[u8]) -> Result<()> {
        let sent = self.send_frames(&[frame])?;
        if sent != 1 {
            return Err(TetherError::protocol("frame was not sent"));
        }
        Ok(())
    }

    /// Drain the interrupt endpoint.  Each notification is queued for the
    /// engine; control messages are consumed and answered here.
    pub fn poll_notifications(&mut self, timeout: Duration) -> Result<Vec<Notification>> {
        let Some(endpoint) = self.interrupt_in else {
            return Ok(Vec::new());
        };
        let mut buffer = [0u8; 64];
        match self.transport.interrupt_in(endpoint, &mut buffer, timeout) {
            Ok(0) => return Ok(Vec::new()),
            Ok(length) => {
                if length > buffer.len() {
                    return Err(TetherError::protocol(
                        "USB transport returned an oversized interrupt notification",
                    ));
                }
                self.handle_interrupt_bytes(&buffer[..length], true)?;
            }
            Err(UsbError::Timeout) | Err(UsbError::Stall) => {}
            Err(error) => return Err(error.into_tether("RNDIS interrupt IN")),
        }
        Ok(self.take_notifications())
    }

    /// Send a keepalive if the control channel has been idle long enough.
    pub fn keepalive_if_due(&mut self) -> Result<bool> {
        if !self.initialized {
            return Ok(false);
        }
        if self.last_control.elapsed() < self.config.keepalive_interval {
            return Ok(false);
        }
        let request_id = self.next_request_id();
        let request = message::encode_keepalive_request(request_id)?;
        self.stats.keepalives_sent += 1;
        let response = self.command(request, MSG_KEEPALIVE_CMPLT, Some(request_id))?;
        let completion = message::parse_keepalive_complete(&response)?;
        if completion.request_id != request_id {
            return Err(TetherError::protocol(
                "RNDIS keepalive completion has the wrong request id",
            ));
        }
        self.check_status(completion.status, "RNDIS KEEPALIVE")?;
        self.last_control = Instant::now();
        Ok(true)
    }

    /// Send HALT without waiting for a completion (the specification does not
    /// define one for HALT).
    pub fn halt(&mut self) -> Result<()> {
        if !self.initialized {
            return Ok(());
        }
        let request = message::encode_halt_request(self.next_request_id())?;
        let _ = self.send_encapsulated(&request);
        self.initialized = false;
        Ok(())
    }

    /// Reset the device and wait for RESET_CMPLT.  The caller must initialize
    /// again afterwards.
    pub fn reset(&mut self) -> Result<ResetComplete> {
        self.reassembler.clear();
        self.pending_notifications.clear();
        let request = message::encode_reset_request()?;
        self.send_encapsulated(&request)?;
        self.last_control = Instant::now();
        let response = self.wait_for_message(MSG_RESET_CMPLT, None, self.config.control_timeout)?;
        let completion = message::parse_reset_complete(&response)?;
        self.check_status(completion.status, "RNDIS RESET")?;
        self.initialized = false;
        self.info = None;
        self.stats.resets += 1;
        Ok(completion)
    }

    fn max_transfer_size(&self) -> usize {
        self.info
            .as_ref()
            .map(|info| info.max_transfer_size as usize)
            .unwrap_or(self.config.max_transfer_size as usize)
            .clamp(64, 1024 * 1024)
    }

    fn max_frame_size(&self) -> u32 {
        self.info
            .as_ref()
            .map(|info| info.max_frame_size)
            .unwrap_or(self.config.max_frame_size)
            .clamp(14, 1518)
    }

    fn command(
        &mut self,
        request: Vec<u8>,
        expected_kind: u32,
        expected_request_id: Option<u32>,
    ) -> Result<Vec<u8>> {
        self.send_encapsulated(&request)?;
        self.last_control = Instant::now();
        let response = self.wait_for_message(
            expected_kind,
            expected_request_id,
            self.config.control_timeout,
        )?;
        Ok(response)
    }

    fn send_encapsulated(&mut self, request: &[u8]) -> Result<()> {
        if request.len() > 4096 {
            return Err(TetherError::protocol(
                "RNDIS control request exceeds the safe control buffer size",
            ));
        }
        let written = self
            .transport
            .control_out(&ControlOutRequest {
                request_type: CLASS_INTERFACE_OUT,
                request: CONTROL_REQUEST_SEND,
                value: 0,
                index: u16::from(self.control_interface),
                data: request,
                timeout: self.config.control_timeout,
            })
            .map_err(|error| error.into_tether("SEND_ENCAPSULATED_COMMAND"))?;
        if written != request.len() {
            return Err(TetherError::new(
                ErrorCategory::UsbTransfer,
                "short SEND_ENCAPSULATED_COMMAND transfer",
            ));
        }
        self.stats.control_requests += 1;
        Ok(())
    }

    fn get_encapsulated(&mut self) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; 8192];
        let received = self
            .transport
            .control_in(
                &ControlInRequest {
                    request_type: CLASS_INTERFACE_IN,
                    request: CONTROL_REQUEST_GET,
                    value: 0,
                    index: u16::from(self.control_interface),
                    // A device with no completion ready often stalls this
                    // request.  Keep polling responsive instead of blocking
                    // for the full command timeout.
                    timeout: self.config.control_timeout.min(Duration::from_millis(500)),
                },
                &mut buffer,
            )
            .map_err(|error| error.into_tether("GET_ENCAPSULATED_RESPONSE"))?;
        if received > buffer.len() {
            return Err(TetherError::protocol(
                "GET_ENCAPSULATED_RESPONSE returned more bytes than requested",
            ));
        }
        buffer.truncate(received);
        if buffer.is_empty() {
            return Err(TetherError::protocol(
                "GET_ENCAPSULATED_RESPONSE returned an empty message",
            ));
        }
        Ok(buffer)
    }

    fn wait_for_message(
        &mut self,
        expected_kind: u32,
        expected_request_id: Option<u32>,
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        // Some devices only answer after their interrupt endpoint was polled,
        // while others answer immediately.  Drain one notification if the
        // endpoint exists, then poll the control channel regardless.
        if self.interrupt_in.is_some() {
            let mut notification = [0u8; 64];
            if let Ok(length) = self.transport.interrupt_in(
                self.interrupt_in.expect("checked above"),
                &mut notification,
                self.config.notification_timeout.min(timeout),
            ) {
                if length > 0 && length <= notification.len() {
                    self.handle_interrupt_bytes(&notification[..length], false)?;
                }
            }
        }
        let mut attempts = 0u64;
        loop {
            if Instant::now() >= deadline {
                self.stats.control_timeouts += 1;
                return Err(TetherError::timeout(format!(
                    "timed out waiting for RNDIS completion 0x{expected_kind:08x}"
                )));
            }
            match self.get_encapsulated() {
                Ok(response) => {
                    let header = message::parse_header(&response)?;
                    if header.kind == expected_kind {
                        if let Some(expected) = expected_request_id {
                            if header.request_id != expected {
                                self.stats.stale_responses += 1;
                                continue;
                            }
                        }
                        return Ok(response);
                    }
                    match header.kind {
                        MSG_INDICATE_STATUS => {
                            let indication = message::parse_indicate_status(&response)?;
                            self.record_indication(indication);
                        }
                        MSG_KEEPALIVE => {
                            self.answer_keepalive(header.request_id)?;
                        }
                        MSG_KEEPALIVE_CMPLT => {
                            // A late response to our own keepalive; ignore.
                        }
                        _ => {
                            self.stats.stale_responses += 1;
                        }
                    }
                }
                Err(error)
                    if matches!(
                        error.category,
                        ErrorCategory::UsbTimeout | ErrorCategory::ProtocolTimeout
                    ) || error.message.contains("stall") =>
                {
                    // Polling an empty control channel commonly returns a
                    // protocol stall; keep polling until the deadline.
                }
                Err(error) => return Err(error),
            }
            attempts += 1;
            if attempts > 256 {
                return Err(TetherError::timeout(
                    "RNDIS control response polling exceeded its attempt limit",
                ));
            }
            thread::sleep(self.config.poll_interval);
        }
    }

    fn handle_interrupt_bytes(&mut self, bytes: &[u8], consume_response: bool) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        // The bare RNDIS notification is one byte (RESPONSE_AVAILABLE).  The
        // full CDC notification form starts with bmRequestType 0xA1.
        let (notification_type, payload) = if bytes[0] == 0xa1 && bytes.len() >= 8 {
            (bytes[1], &bytes[8..])
        } else {
            (bytes[0], bytes)
        };
        self.stats.notifications += 1;
        match notification_type {
            RESPONSE_AVAILABLE => {
                if consume_response {
                    let response = self.get_encapsulated()?;
                    let header = message::parse_header(&response)?;
                    match header.kind {
                        MSG_INDICATE_STATUS => {
                            let indication = message::parse_indicate_status(&response)?;
                            self.record_indication(indication);
                        }
                        MSG_KEEPALIVE => self.answer_keepalive(header.request_id)?,
                        _ => {
                            // A completion fetched outside a command is
                            // stale.  Record it without disturbing the
                            // command correlation state.
                            self.stats.stale_responses += 1;
                            self.pending_notifications
                                .push_back(Notification::Other(header.kind as u8));
                        }
                    }
                }
            }
            CONNECTION_SPEED_CHANGE => {
                if payload.len() >= 8 {
                    let down = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    let up = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                    self.pending_notifications
                        .push_back(Notification::ConnectionSpeedChange {
                            down_bps: down,
                            up_bps: up,
                        });
                } else {
                    self.pending_notifications
                        .push_back(Notification::Other(notification_type));
                }
            }
            other => self
                .pending_notifications
                .push_back(Notification::Other(other)),
        }
        Ok(())
    }

    fn answer_keepalive(&mut self, request_id: u32) -> Result<()> {
        let mut response = Vec::with_capacity(16);
        response.extend_from_slice(&MSG_KEEPALIVE_CMPLT.to_le_bytes());
        response.extend_from_slice(&16u32.to_le_bytes());
        response.extend_from_slice(&request_id.to_le_bytes());
        response.extend_from_slice(&STATUS_SUCCESS.to_le_bytes());
        self.send_encapsulated(&response)
    }

    fn record_indication(&mut self, indication: IndicateStatus<'_>) {
        if indication.status == STATUS_MEDIA_CONNECT {
            if let Some(info) = &mut self.info {
                info.media_connected = true;
            }
        } else if indication.status == STATUS_MEDIA_DISCONNECT {
            if let Some(info) = &mut self.info {
                info.media_connected = false;
            }
        } else if let Some(buffer) = indication.buffer {
            // Some devices send status 0 with a buffer containing the OID and
            // value.  Parse conservatively; a malformed optional buffer must
            // not kill the session.
            if buffer.len() >= 8 {
                let oid = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
                if oid == OID_GEN_MEDIA_CONNECT_STATUS {
                    let value = u32::from_le_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]);
                    if let Some(info) = &mut self.info {
                        info.media_connected = value == oids::MEDIA_CONNECTED;
                    }
                }
            }
        }
        self.pending_notifications
            .push_back(Notification::IndicateStatus {
                status: indication.status,
                buffer: indication.buffer.unwrap_or(&[]).to_vec(),
            });
    }

    fn query_oid(&mut self, oid: u32) -> Result<Vec<u8>> {
        let request_id = self.next_request_id();
        let request = message::encode_query_request(request_id, oid, &[])?;
        let response = self.command(request, MSG_QUERY_CMPLT, Some(request_id))?;
        let completion = message::parse_query_complete(&response)?;
        if completion.request_id != request_id {
            return Err(TetherError::protocol(
                "RNDIS QUERY completion has the wrong request id",
            ));
        }
        self.check_status(
            completion.status,
            &format!("RNDIS QUERY {}", oids::oid_name(oid)),
        )?;
        Ok(completion.information.to_vec())
    }

    fn query_u32(&mut self, oid: u32) -> Result<u32> {
        let bytes = self.query_oid(oid)?;
        oids::parse_u32_le(&bytes).ok_or_else(|| {
            TetherError::protocol(format!(
                "RNDIS QUERY {} returned {} bytes, expected 4",
                oids::oid_name(oid),
                bytes.len()
            ))
        })
    }

    fn set_oid(&mut self, oid: u32, information: &[u8]) -> Result<()> {
        let request_id = self.next_request_id();
        let request = message::encode_set_request(request_id, oid, information)?;
        let response = self.command(request, MSG_SET_CMPLT, Some(request_id))?;
        let completion = message::parse_set_complete(&response)?;
        if completion.request_id != request_id {
            return Err(TetherError::protocol(
                "RNDIS SET completion has the wrong request id",
            ));
        }
        self.check_status(
            completion.status,
            &format!("RNDIS SET {}", oids::oid_name(oid)),
        )
    }

    fn check_status(&self, status: u32, context: &str) -> Result<()> {
        if status == STATUS_SUCCESS {
            Ok(())
        } else {
            Err(TetherError::protocol(format!(
                "{} failed: {} (0x{status:08x})",
                context,
                oids::status_name(status)
            )))
        }
    }

    fn next_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        if self.next_request_id == 0 {
            self.next_request_id = 1;
        }
        id
    }

    /// Expose the raw transport for diagnostics and orderly teardown.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }
}

impl From<UsbError> for TetherError {
    fn from(error: UsbError) -> Self {
        error.into_tether("USB transport")
    }
}

impl UsbError {
    pub fn into_tether(self, context: &str) -> TetherError {
        let category = match self {
            UsbError::Timeout => ErrorCategory::UsbTimeout,
            UsbError::Stall => ErrorCategory::UsbTransfer,
            UsbError::Disconnected => ErrorCategory::UsbNotFound,
            UsbError::Permission => ErrorCategory::UsbPermission,
            UsbError::Busy => ErrorCategory::UsbBusy,
            UsbError::Other(_) => ErrorCategory::UsbTransfer,
        };
        TetherError::new(category, format!("{context}: {}", self.message()))
    }
}
