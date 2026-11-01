use std::time::Duration;

use tether_core::engine::{EngineConfig, TetherEngine};
use tether_core::error::ErrorCategory;
use tether_core::ethernet::{self, MacAddr};
use tether_core::net::{FrameIo, MockFrameIo};
use tether_core::rndis::mock::{mock_function, MockRndisDevice};
use tether_core::rndis::oids::{self, DEFAULT_PACKET_FILTER};
use tether_core::rndis::session::{Notification, RndisSession, SessionConfig};
use tether_core::testkit::{run_simulation, Scenario};
use tether_core::transport::UsbError;

fn macs() -> (MacAddr, MacAddr) {
    (
        MacAddr([0x02, 0, 0, 0, 0, 0x42]),
        MacAddr([0x02, 0, 0, 0, 0, 0x11]),
    )
}

fn short_config() -> SessionConfig {
    SessionConfig {
        control_timeout: Duration::from_millis(500),
        notification_timeout: Duration::from_millis(20),
        poll_interval: Duration::from_millis(1),
        data_timeout: Duration::from_millis(100),
        ..SessionConfig::default()
    }
}

fn arp(mac: MacAddr, src: [u8; 4], dst: [u8; 4]) -> Vec<u8> {
    ethernet::build_arp_request(mac, src, dst)
}

#[test]
fn basic_simulation_moves_frames_both_ways() {
    let report = run_simulation(Scenario::Basic).unwrap();
    assert_eq!(
        report.info.protocol,
        tether_core::descriptor::Protocol::Rndis
    );
    assert_eq!(report.info.host_mac, [0x02, 0, 0, 0, 0, 0x42]);
    assert_eq!(report.frames_host_to_phone, 1);
    assert_eq!(report.frames_phone_to_host, 1);
    assert_eq!(report.received_by_device, 1);
    assert_eq!(report.delivered_to_host, 1);
    assert_eq!(report.bytes_host_to_phone, 42);
    assert_eq!(report.bytes_phone_to_host, 42);
    assert!(report.control_requests >= 8);
    assert_eq!(report.link_changes, 1);
    assert!(
        report.info.warnings.is_empty(),
        "{:?}",
        report.info.warnings
    );
}

#[test]
fn dhcp_simulation_sends_a_real_dhcp_discover() {
    let report = run_simulation(Scenario::Dhcp).unwrap();
    assert_eq!(report.frames_host_to_phone, 1);
    assert_eq!(report.received_by_device, 1);
    assert_eq!(report.delivered_frames.len(), 1);
    let offer = ethernet::parse_dhcp_packet(&report.delivered_frames[0]).unwrap();
    assert_eq!(offer.op, 2);
    assert_eq!(offer.message_type, 2);
    assert_eq!(offer.xid, 0x1234_5678);
}

#[test]
fn faults_simulation_uses_reduced_packet_filter() {
    let report = run_simulation(Scenario::Faults).unwrap();
    assert_eq!(report.info.packet_filter, 0x0d);
    assert!(report
        .warnings
        .iter()
        .any(|warning| warning.contains("packet filter")));
    assert_eq!(report.frames_host_to_phone, 1);
    assert_eq!(report.frames_phone_to_host, 1);
}

#[test]
fn session_works_without_interrupt_notifications() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new()
        .with_notifications(false)
        .with_host_mac(host.0);
    device.queue_frame_to_host(arp(host, [10, 0, 0, 1], [10, 0, 0, 2]));
    let function = mock_function(false);
    let mut session = RndisSession::new(device, &function, short_config()).unwrap();
    let info = session.initialize().unwrap();
    assert_eq!(info.host_mac, host.0);

    let frames = session.receive_frames(Duration::from_millis(100)).unwrap();
    assert_eq!(frames.len(), 1);
    session
        .send_frame(&arp(host, [10, 0, 0, 2], [10, 0, 0, 1]))
        .unwrap();
    assert_eq!(session.transport().received_frames().len(), 1);
}

#[test]
fn multiple_frames_in_one_transfer_are_reassembled() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new().with_host_mac(host.0);
    for index in 0..4u8 {
        device.queue_frame_to_host(arp(host, [10, 0, 0, index + 1], [10, 0, 0, 0xff]));
    }
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    session.initialize().unwrap();
    let frames = session.receive_frames(Duration::from_millis(100)).unwrap();
    assert_eq!(frames.len(), 4);
    assert_eq!(session.stats().frames_rx, 4);
    assert_eq!(session.stats().malformed_frames, 0);
}

#[test]
fn oversized_ethernet_frame_is_dropped_not_forwarded() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new().with_host_mac(host.0);
    let mut frame = arp(host, [10, 0, 0, 1], [10, 0, 0, 2]);
    frame.resize(1515, 0);
    device.queue_frame_to_host(frame);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    session.initialize().unwrap();
    let frames = session.receive_frames(Duration::from_millis(100)).unwrap();
    assert!(frames.is_empty());
    assert_eq!(session.stats().malformed_frames, 1);
}

#[test]
fn malformed_bulk_transfer_is_rejected() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new().with_host_mac(host.0);
    device.queue_raw_transfer_to_host(vec![0xff; 44]);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    session.initialize().unwrap();
    let error = session
        .receive_frames(Duration::from_millis(100))
        .unwrap_err();
    assert_eq!(error.category, ErrorCategory::ProtocolViolation);
}

#[test]
fn packet_filter_falls_back_when_device_rejects_promiscuous_mode() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new().with_host_mac(host.0);
    device.packet_filter_fallback = true;
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    let info = session.initialize().unwrap();
    assert_eq!(info.packet_filter, 0x0d);
    assert!(info
        .warnings
        .iter()
        .any(|warning| warning.contains("preferred packet filter")));
    assert_ne!(info.packet_filter, DEFAULT_PACKET_FILTER);
}

#[test]
fn reset_then_reinitialize_succeeds() {
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    session.initialize().unwrap();
    let reset = session.reset().unwrap();
    assert_eq!(reset.status, 0);
    assert!(!session.initialized());
    let info = session.initialize().unwrap();
    assert_eq!(info.host_mac, host.0);
    assert_eq!(session.stats().resets, 1);
}

#[test]
fn keepalive_is_sent_and_acknowledged() {
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let mut config = short_config();
    config.keepalive_interval = Duration::ZERO;
    let mut session = RndisSession::new(device, &mock_function(true), config).unwrap();
    session.initialize().unwrap();
    assert!(session.keepalive_if_due().unwrap());
    assert_eq!(session.stats().keepalives_sent, 1);
    assert!(session.keepalive_if_due().unwrap());
    assert_eq!(session.stats().keepalives_sent, 2);
}

#[test]
fn media_indications_update_link_state() {
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    session.initialize().unwrap();
    assert!(session.media_connected());
    session
        .transport_mut()
        .inject_indication(tether_core::rndis::message::STATUS_MEDIA_DISCONNECT, &[]);
    let notifications = session
        .poll_notifications(Duration::from_millis(100))
        .unwrap();
    assert!(notifications
        .iter()
        .any(|n| matches!(n, Notification::IndicateStatus { .. })));
    assert!(!session.media_connected());
    session
        .transport_mut()
        .inject_indication(tether_core::rndis::message::STATUS_MEDIA_CONNECT, &[]);
    session
        .poll_notifications(Duration::from_millis(100))
        .unwrap();
    assert!(session.media_connected());
}

#[test]
fn engine_recovers_from_a_transient_transfer_error() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new().with_host_mac(host.0);
    device.queue_frame_to_host(arp(host, [10, 0, 0, 1], [10, 0, 0, 2]));
    let session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    let bridge = MockFrameIo::new();
    let mut engine = TetherEngine::new(
        session,
        bridge,
        EngineConfig {
            notification_timeout: Duration::from_millis(2),
            bridge_timeout: Duration::from_millis(1),
            receive_timeout: Duration::from_millis(2),
            max_reconnect_attempts: 2,
            reconnect_backoff: Duration::from_millis(1),
            idle_sleep: Duration::from_millis(1),
        },
    );
    engine.initialize().unwrap();
    engine.session_mut().transport_mut().fail_bulk_in_next =
        Some(UsbError::Other("injected failure".into()));
    let error = engine.step().unwrap_err();
    assert_eq!(error.category, ErrorCategory::UsbTransfer);
    engine.recover().unwrap();
    assert_eq!(engine.stats().reconnects, 1);
    // The queued frame was consumed by the failed transfer attempt; the
    // recovered session must still be able to send.
    engine
        .session_mut()
        .send_frame(&arp(host, [10, 0, 0, 2], [10, 0, 0, 1]))
        .unwrap();
}

#[test]
fn engine_marks_bridge_down_on_media_disconnect() {
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    let mut engine = TetherEngine::new(session, MockFrameIo::new(), EngineConfig::default());
    engine.initialize().unwrap();
    assert!(engine.bridge().link_up());
    engine
        .session_mut()
        .transport_mut()
        .inject_indication(tether_core::rndis::message::STATUS_MEDIA_DISCONNECT, &[]);
    engine.step().unwrap();
    assert!(!engine.bridge().link_up());
}

#[test]
fn dhcp_discover_sent_to_phone_has_valid_fields() {
    let (host, _phone) = macs();
    let bridge_frame = ethernet::build_dhcp_discover(host, 0xdead_beef);
    let mut bridge = MockFrameIo::new();
    bridge.queue_from_host(bridge_frame.clone());
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    let mut engine = TetherEngine::new(session, bridge, EngineConfig::default());
    engine.initialize().unwrap();
    for _ in 0..20 {
        let report = engine.step().unwrap();
        if report.host_to_phone == 1 {
            break;
        }
    }
    let received = engine.session().transport().received_frames();
    assert_eq!(received.len(), 1);
    let parsed = ethernet::parse_dhcp_discover(&received[0]).unwrap();
    assert_eq!(parsed.xid, 0xdead_beef);
    assert_eq!(parsed.client_mac, host);
    assert_eq!(parsed.message_type, 1);
    assert_eq!(received[0], bridge_frame);
}

#[test]
fn oid_failure_is_reported_as_warning() {
    let (host, _phone) = macs();
    let mut device = MockRndisDevice::new().with_host_mac(host.0);
    device.failing_oids.push(oids::OID_GEN_MAXIMUM_TOTAL_SIZE);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    let info = session.initialize().unwrap();
    assert!(info
        .warnings
        .iter()
        .any(|w| w.contains("MAXIMUM_TOTAL_SIZE")));
}

#[test]
fn packet_filter_is_applied_on_the_mock_device() {
    use tether_core::rndis::oids::DEFAULT_PACKET_FILTER;
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    session.initialize().unwrap();
    assert_eq!(session.transport().filter, DEFAULT_PACKET_FILTER);
}

#[test]
fn repeated_reset_and_initialize_cycles_stay_correlated() {
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let mut session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    for _ in 0..3 {
        session.initialize().unwrap();
        session.reset().unwrap();
    }
    let info = session.initialize().unwrap();
    assert_eq!(info.host_mac, host.0);
    assert_eq!(session.stats().resets, 3);
}

#[test]
fn engine_run_stops_and_marks_link_down() {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    let (host, _phone) = macs();
    let device = MockRndisDevice::new().with_host_mac(host.0);
    let session = RndisSession::new(device, &mock_function(true), short_config()).unwrap();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = stop.clone();
    let handle = std::thread::spawn(move || {
        let mut engine = TetherEngine::new(session, MockFrameIo::new(), EngineConfig::default());
        let result = engine.run(&stop, |_stats| {});
        (result, engine.bridge().link_up())
    });
    std::thread::sleep(Duration::from_millis(80));
    stop_clone.store(true, Ordering::SeqCst);
    let (result, link_up) = handle.join().unwrap();
    result.unwrap();
    assert!(!link_up);
}
