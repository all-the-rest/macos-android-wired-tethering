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
use crate::net::{FrameIo, MockFrameIo};
use crate::rndis::mock::{mock_function, MockRndisDevice};
use crate::rndis::session::{RndisSession, SessionConfig, SessionInfo};
use crate::transport::UsbTransport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    Basic,
    Dhcp,
    Faults,
    Reconnect,
}

impl Scenario {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "basic" => Ok(Self::Basic),
            "dhcp" => Ok(Self::Dhcp),
            "faults" => Ok(Self::Faults),
            "reconnect" => Ok(Self::Reconnect),
            other => Err(TetherError::new(
                crate::error::ErrorCategory::Configuration,
                format!("unknown scenario '{other}' (expected basic, dhcp, faults or reconnect)"),
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Dhcp => "dhcp",
            Self::Faults => "faults",
            Self::Reconnect => "reconnect",
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
    /// Number of synthetic session reconnects performed by the scenario.
    pub reconnects: usize,
    /// Frames delivered to the host in this run (bounded by the scenario).
    pub delivered_frames: Vec<Vec<u8>>,
}

pub fn run_simulation(scenario: Scenario) -> Result<SimulationReport> {
    if scenario == Scenario::Reconnect {
        return run_reconnect_simulation();
    }

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
        Scenario::Reconnect => unreachable!("handled above"),
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
        notification_interval: Duration::from_millis(1),
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
        reconnects: 0,
        delivered_frames,
    })
}

fn simulation_session_config(host_mac: MacAddr) -> SessionConfig {
    SessionConfig {
        host_mac_fallback: host_mac.0,
        control_timeout: Duration::from_millis(500),
        notification_timeout: Duration::from_millis(20),
        poll_interval: Duration::from_millis(1),
        data_timeout: Duration::from_millis(100),
        ..SessionConfig::default()
    }
}

fn simulation_engine_config() -> EngineConfig {
    EngineConfig {
        notification_timeout: Duration::from_millis(5),
        notification_interval: Duration::from_millis(1),
        bridge_timeout: Duration::from_millis(1),
        receive_timeout: Duration::from_millis(2),
        max_reconnect_attempts: 1,
        reconnect_backoff: Duration::from_millis(1),
        idle_sleep: Duration::from_millis(1),
    }
}

fn run_steps<T: UsbTransport, B: FrameIo>(engine: &mut TetherEngine<T, B>) -> Result<()> {
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
    Ok(())
}

fn run_reconnect_simulation() -> Result<SimulationReport> {
    let host_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x42]);
    let first_phone_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x11]);
    let second_phone_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x22]);

    let mut bridge = MockFrameIo::new();
    bridge.queue_from_host(ethernet::build_arp_request(
        host_mac,
        [192, 168, 42, 2],
        [192, 168, 42, 1],
    ));

    let mut first_device = MockRndisDevice::new()
        .with_mac(first_phone_mac.0)
        .with_host_mac(host_mac.0);
    first_device.queue_frame_to_host(ethernet::build_arp_request(
        first_phone_mac,
        [192, 168, 42, 1],
        [192, 168, 42, 2],
    ));
    let first_session = RndisSession::new(
        first_device,
        &mock_function(true),
        simulation_session_config(host_mac),
    )?;
    let mut first_engine = TetherEngine::new(first_session, bridge, simulation_engine_config());
    first_engine.initialize()?;
    run_steps(&mut first_engine)?;

    let first_stats = first_engine.stats().clone();
    let first_control_requests = first_engine.session().stats().control_requests as usize;
    let first_received = first_engine.session().transport().received_frames().len();
    let (first_session, mut bridge) = first_engine.into_parts();
    drop(first_session);

    bridge.queue_from_host(ethernet::build_arp_request(
        host_mac,
        [192, 168, 42, 2],
        [192, 168, 42, 1],
    ));
    let mut second_device = MockRndisDevice::new()
        .with_mac(second_phone_mac.0)
        .with_host_mac(second_phone_mac.0);
    second_device.queue_frame_to_host(ethernet::build_arp_request(
        second_phone_mac,
        [192, 168, 42, 1],
        [192, 168, 42, 2],
    ));
    let config = SessionConfig {
        host_mac_override: Some(host_mac.0),
        ..simulation_session_config(host_mac)
    };
    let second_session = RndisSession::new(second_device, &mock_function(true), config)?;
    let mut second_engine = TetherEngine::new(second_session, bridge, simulation_engine_config());
    let info = second_engine.initialize()?;
    run_steps(&mut second_engine)?;

    let second_stats = second_engine.stats().clone();
    let second_received = second_engine.session().transport().received_frames().len();
    let delivered_frames = second_engine.bridge().sent_frames().to_vec();
    Ok(SimulationReport {
        scenario: Scenario::Reconnect,
        info: info.clone(),
        frames_host_to_phone: first_stats.frames_host_to_phone as usize
            + second_stats.frames_host_to_phone as usize,
        frames_phone_to_host: first_stats.frames_phone_to_host as usize
            + second_stats.frames_phone_to_host as usize,
        bytes_host_to_phone: first_stats.bytes_host_to_phone as usize
            + second_stats.bytes_host_to_phone as usize,
        bytes_phone_to_host: first_stats.bytes_phone_to_host as usize
            + second_stats.bytes_phone_to_host as usize,
        link_changes: first_stats.link_changes as usize + second_stats.link_changes as usize,
        warnings: info.warnings.clone(),
        received_by_device: first_received + second_received,
        delivered_to_host: delivered_frames.len(),
        control_requests: first_control_requests
            + second_engine.session().stats().control_requests as usize,
        reconnects: 1,
        delivered_frames,
    })
}
