use std::time::Duration;

use tether_core::error::ErrorCategory;
use tether_core::rndis::message::{
    self, parse_indicate_status, parse_initialize_complete, parse_packet, parse_query_complete,
};
use tether_core::rndis::mock::{mock_function, MockRndisDevice};
use tether_core::rndis::session::{RndisSession, SessionConfig};
use tether_core::transport::UsbError;

fn config() -> SessionConfig {
    SessionConfig {
        control_timeout: Duration::from_millis(60),
        notification_timeout: Duration::from_millis(5),
        poll_interval: Duration::from_millis(1),
        ..SessionConfig::default()
    }
}

fn init_complete(request_id: u32, length_override: Option<u32>) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&message::MSG_INITIALIZE_CMPLT.to_le_bytes());
    data.extend_from_slice(&length_override.unwrap_or(52).to_le_bytes());
    data.extend_from_slice(&request_id.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&4u32.to_le_bytes());
    data.extend_from_slice(&0x4000u32.to_le_bytes());
    data.extend_from_slice(&2u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data
}

#[test]
fn wrong_request_id_times_out_without_accepting_the_completion() {
    let mut device = MockRndisDevice::new();
    device.force_response = Some(init_complete(999, None));
    let mut session = RndisSession::new(device, &mock_function(true), config()).unwrap();
    let error = session.initialize().unwrap_err();
    assert_eq!(error.category, ErrorCategory::ProtocolTimeout);
}

#[test]
fn truncated_initialize_completion_is_rejected() {
    let mut device = MockRndisDevice::new();
    let mut response = init_complete(1, Some(51));
    response.truncate(51);
    device.force_response = Some(response);
    let mut session = RndisSession::new(device, &mock_function(true), config()).unwrap();
    let error = session.initialize().unwrap_err();
    assert_eq!(error.category, ErrorCategory::ProtocolViolation);
}

#[test]
fn control_transfer_timeout_has_usb_timeout_category() {
    let mut device = MockRndisDevice::new();
    device.fail_control_next = Some(UsbError::Timeout);
    let mut session = RndisSession::new(device, &mock_function(true), config()).unwrap();
    let error = session.initialize().unwrap_err();
    assert_eq!(error.category, ErrorCategory::UsbTimeout);
}

#[test]
fn frame_larger_than_negotiated_transfer_is_refused() {
    let mut device = MockRndisDevice::new().with_max_frame_size(1514);
    device.max_transfer_size = 44;
    let mut session = RndisSession::new(device, &mock_function(true), config()).unwrap();
    session.initialize().unwrap();
    let frame = vec![0u8; 60];
    let error = session.send_frame(&frame).unwrap_err();
    assert_eq!(error.category, ErrorCategory::ProtocolViolation);
    assert!(error.message.contains("does not fit"));
}

#[test]
fn out_of_bounds_query_information_is_rejected() {
    let mut response = Vec::new();
    response.extend_from_slice(&message::MSG_QUERY_CMPLT.to_le_bytes());
    response.extend_from_slice(&24u32.to_le_bytes());
    response.extend_from_slice(&1u32.to_le_bytes());
    response.extend_from_slice(&0u32.to_le_bytes());
    response.extend_from_slice(&4u32.to_le_bytes());
    response.extend_from_slice(&16u32.to_le_bytes());
    assert!(parse_query_complete(&response).is_err());
}

#[test]
fn out_of_bounds_packet_oob_data_is_rejected() {
    let mut packet = vec![0u8; 44];
    packet[0..4].copy_from_slice(&message::MSG_PACKET.to_le_bytes());
    packet[4..8].copy_from_slice(&44u32.to_le_bytes());
    packet[8..12].copy_from_slice(&36u32.to_le_bytes());
    packet[12..16].copy_from_slice(&0u32.to_le_bytes());
    packet[16..20].copy_from_slice(&0u32.to_le_bytes()); // OOB offset 0 -> absolute 8
    packet[20..24].copy_from_slice(&8u32.to_le_bytes()); // OOB length
    assert!(parse_packet(&packet).is_err());
}

#[test]
fn malformed_indicate_status_buffer_does_not_hide_the_status() {
    let mut response = Vec::new();
    response.extend_from_slice(&message::MSG_INDICATE_STATUS.to_le_bytes());
    response.extend_from_slice(&24u32.to_le_bytes());
    response.extend_from_slice(&0x4001_000cu32.to_le_bytes());
    response.extend_from_slice(&8u32.to_le_bytes());
    response.extend_from_slice(&100u32.to_le_bytes());
    response.extend_from_slice(&[0u8; 4]);
    let parsed = parse_indicate_status(&response).unwrap();
    assert_eq!(parsed.status, 0x4001_000c);
    assert_eq!(parsed.buffer, None);
}

#[test]
fn initialize_completion_rejects_invalid_alignment_exponent() {
    let mut data = init_complete(1, None);
    data[40..44].copy_from_slice(&8u32.to_le_bytes());
    assert!(parse_initialize_complete(&data).is_err());
}

#[test]
fn header_longer_than_buffer_is_rejected() {
    let mut data = vec![0u8; 12];
    data[0..4].copy_from_slice(&message::MSG_KEEPALIVE.to_le_bytes());
    data[4..8].copy_from_slice(&40u32.to_le_bytes());
    assert!(message::parse_header(&data).is_err());
}
