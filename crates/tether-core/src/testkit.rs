//! End-to-end synthetic scenarios (no USB hardware, no root).
//!
//! These scenarios are used by the `simulate` CLI command and by the
//! integration tests.  They exercise the real RNDIS session, packet framing,
//! Ethernet validation and the bidirectional bridge, with only the USB and
//! macOS I/O layers replaced by deterministic mocks.

use std::time::Duration;

use crate::descriptor::Protocol;
use crate::engine::{EngineConfig, TetherEngine};
use crate::error::{Result, TetherError};
use crate::ethernet::{self, MacAddr};
use crate::net::MockFrameIo;
use crate::rndis::mock::{mock_function, MockRndisDevice};
use crate::rndis::session::{RndisSession, SessionConfig, SessionInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    Basic,
    Dhcp,
    Faults,
}

impl Scenario {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "basic" => Ok(Self::Basic),
            "dhcp" => Ok(Self::Dhcp),
            "faults" => Ok(Self::Faults),
            other => Err(TetherError::new(
                crate::error::ErrorCategory::Configuration,
                format!("unknown scenario '{other}' (expected basic, dhcp or faults)"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Dhcp => "dhcp",
            Self::Faults => "faults",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimulationReport {
    pub scenario: Scenario,
    pub info: SessionInfo,
    pub frames_host_to_phone: usize,
    pub frames_phone_to_host: usize,
    pub bytes_host_to_phone: usize,
    pub bytes_phone_to_host: usize,
    pub link_changes: usize,
    pub warnings: Vec<String>,
    pub received_by_device: usize,
    pub delivered_to_host: usize,
    pub control_requests: usize,
    /// Frames delivered to the host in this run (bounded by the scenario).
    pub delivered_frames: Vec<Vec<u8>>,
}

pub fn run_simulation(scenario: Scenario) -> Result<SimulationReport> {
    let host_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x42]);
    let phone_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x11]);
    let mut device = MockRndisDevice::new()
        .with_mac(phone_mac.0)
        .with_host_mac(host_mac.0);
    let mut bridge = MockFrameIo::new();

    match scenario {
        Scenario::Basic => {
            bridge.queue_from_host(ethernet::build_arp_request(
                host_mac,
                [192, 168, 42, 2],
                [192, 168, 42, 1],
            ));
            device.queue_frame_to_host(ethernet::build_arp_request(
                phone_mac,
                [192, 168, 42, 1],
                [192, 168, 42, 2],
            ));
        }
        Scenario::Dhcp => {
            bridge.queue_from_host(ethernet::build_dhcp_discover(host_mac, 0x1234_5678));
            device.queue_frame_to_host(ethernet::build_dhcp_offer(
                host_mac,
                0x1234_5678,
                [192, 168, 42, 2],
                [192, 168, 42, 1],
                phone_mac,
            ));
        }
        Scenario::Faults => {
            // The device rejects the preferred promiscuous filter and a
            // nominal OID; the session must use the reduced filter and
            // continue with defaults.
            device.packet_filter_fallback = true;
            device
                .failing_oids
                .push(crate::rndis::oids::OID_GEN_MAXIMUM_TOTAL_SIZE);
            device.queue_frame_to_host(ethernet::build_arp_request(
                phone_mac,
                [192, 168, 42, 1],
                [192, 168, 42, 2],
            ));
            bridge.queue_from_host(ethernet::build_arp_request(
                host_mac,
                [192, 168, 42, 2],
                [192, 168, 42, 1],
            ));
        }
    }

    let transport = device;
    let function = mock_function(true);
    let config = SessionConfig {
        host_mac_fallback: host_mac.0,
        control_timeout: Duration::from_millis(500),
        notification_timeout: Duration::from_millis(20),
        poll_interval: Duration::from_millis(1),
        data_timeout: Duration::from_millis(100),
        ..SessionConfig::default()
    };
    let session = RndisSession::new(transport, &function, config)?;
    let engine_config = EngineConfig {
        notification_timeout: Duration::from_millis(5),
        bridge_timeout: Duration::from_millis(1),
        receive_timeout: Duration::from_millis(2),
        max_reconnect_attempts: 1,
        reconnect_backoff: Duration::from_millis(1),
        idle_sleep: Duration::from_millis(1),
    };
    let mut engine = TetherEngine::new(session, bridge, engine_config);
    let info = engine.initialize()?;
    if info.protocol != Protocol::Rndis {
        return Err(TetherError::internal(
            "simulated session negotiated a non-RNDIS protocol",
        ));
    }

    let mut idle_rounds = 0usize;
    for _ in 0..2000 {
        let report = engine.step()?;
        if report.idle {
            idle_rounds += 1;
            if idle_rounds >= 3 {
                break;
            }
        } else {
            idle_rounds = 0;
        }
    }

    let received_by_device = engine.session().transport().received_frames().len();
    let delivered_frames = engine.bridge().sent_frames().to_vec();
    let delivered_to_host = delivered_frames.len();
    let stats = engine.stats().clone();
    Ok(SimulationReport {
        scenario,
        info: info.clone(),
        frames_host_to_phone: stats.frames_host_to_phone as usize,
        frames_phone_to_host: stats.frames_phone_to_host as usize,
        bytes_host_to_phone: stats.bytes_host_to_phone as usize,
        bytes_phone_to_host: stats.bytes_phone_to_host as usize,
        link_changes: stats.link_changes as usize,
        warnings: info.warnings.clone(),
        received_by_device,
        delivered_to_host,
        control_requests: engine.session().stats().control_requests as usize,
        delivered_frames,
    })
}
