//! Bidirectional engine connecting an RNDIS session to a host Ethernet bridge.
//!
//! The loop is deliberately single-threaded and uses short timeouts.  This is
//! sufficient for a correctness prototype and avoids shared ownership of the
//! USB handle.  A production data path would pipeline bulk transfers on
//! dedicated reader/writer threads; the interface is kept narrow so that can
//! be replaced without touching the protocol core.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{ErrorCategory, Result, TetherError};
use crate::net::FrameIo;
use crate::rndis::session::{Notification, RndisSession, SessionInfo};
use crate::transport::UsbTransport;

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub notification_timeout: Duration,
    pub bridge_timeout: Duration,
    pub receive_timeout: Duration,
    pub max_reconnect_attempts: u32,
    pub reconnect_backoff: Duration,
    pub idle_sleep: Duration,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            notification_timeout: Duration::from_millis(2),
            bridge_timeout: Duration::from_millis(1),
            receive_timeout: Duration::from_millis(10),
            max_reconnect_attempts: 3,
            reconnect_backoff: Duration::from_millis(500),
            idle_sleep: Duration::from_millis(2),
        }
    }
}

/// Optional sleep/wake flags supplied by a platform power monitor.
#[derive(Debug, Clone, Copy, Default)]
pub struct PowerHooks<'a> {
    pub sleeping: Option<&'a AtomicBool>,
    /// Set by the platform callback when the system has resumed.
    pub wake: Option<&'a AtomicBool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineStats {
    pub frames_phone_to_host: u64,
    pub frames_host_to_phone: u64,
    pub bytes_phone_to_host: u64,
    pub bytes_host_to_phone: u64,
    pub notifications: u64,
    pub reconnects: u64,
    pub link_changes: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StepReport {
    pub phone_to_host: usize,
    pub host_to_phone: usize,
    pub notifications: usize,
    pub idle: bool,
}

pub struct TetherEngine<T: UsbTransport, B: FrameIo> {
    session: RndisSession<T>,
    bridge: B,
    config: EngineConfig,
    stats: EngineStats,
    info: Option<SessionInfo>,
}

impl<T: UsbTransport, B: FrameIo> TetherEngine<T, B> {
    pub fn new(session: RndisSession<T>, bridge: B, config: EngineConfig) -> Self {
        Self {
            session,
            bridge,
            config,
            stats: EngineStats::default(),
            info: None,
        }
    }

    pub fn initialize(&mut self) -> Result<SessionInfo> {
        if self.session.initialized() {
            let info =
                self.session.info().cloned().ok_or_else(|| {
                    TetherError::internal("initialized session has no information")
                })?;
            self.bridge.set_link(info.media_connected)?;
            self.info = Some(info.clone());
            return Ok(info);
        }
        let info = self.session.initialize()?;
        self.bridge.set_link(info.media_connected)?;
        self.stats.link_changes += 1;
        self.info = Some(info.clone());
        Ok(info)
    }

    pub fn step(&mut self) -> Result<StepReport> {
        let mut report = StepReport {
            idle: true,
            ..StepReport::default()
        };

        let notifications = self
            .session
            .poll_notifications(self.config.notification_timeout)?;
        for notification in notifications {
            report.notifications += 1;
            self.stats.notifications += 1;
            report.idle = false;
            if let Notification::IndicateStatus { status, buffer } = &notification {
                let _ = (status, buffer);
                self.update_link_from_session()?;
            }
            if matches!(notification, Notification::ConnectionSpeedChange { .. }) {
                // Informational only; the bridge's link state is unchanged.
            }
        }

        // Frames produced by the macOS network stack (DHCP, ARP, IP).
        let mut frame_buffer = [0u8; 2048];
        while let Some(length) = self
            .bridge
            .recv_frame(&mut frame_buffer, self.config.bridge_timeout)?
        {
            self.session.send_frame(&frame_buffer[..length])?;
            report.host_to_phone += 1;
            self.stats.frames_host_to_phone += 1;
            self.stats.bytes_host_to_phone += length as u64;
            report.idle = false;
        }

        // Frames produced by the phone.
        let frames = self.session.receive_frames(self.config.receive_timeout)?;
        for frame in frames {
            let length = frame.len();
            self.bridge.send_frame(&frame)?;
            report.phone_to_host += 1;
            self.stats.frames_phone_to_host += 1;
            self.stats.bytes_phone_to_host += length as u64;
            report.idle = false;
        }

        if self.session.keepalive_if_due()? {
            report.idle = false;
        }

        self.update_link_from_session()?;
        Ok(report)
    }

    fn update_link_from_session(&mut self) -> Result<()> {
        let connected = self.session.media_connected();
        if self.bridge.link_up() != connected {
            self.bridge.set_link(connected)?;
            self.stats.link_changes += 1;
        }
        Ok(())
    }

    /// Recover from a transient USB/protocol failure.  RESET is followed by a
    /// complete re-initialization; the bridge is kept but marked down.
    pub fn recover(&mut self) -> Result<()> {
        self.bridge.set_link(false)?;
        self.info = None;
        self.session.reset()?;
        let info = self.session.initialize()?;
        self.bridge.set_link(info.media_connected)?;
        self.stats.reconnects += 1;
        self.info = Some(info);
        Ok(())
    }

    /// React to a platform sleep/wake event.
    ///
    /// Sleep marks the host link down.  Wake forces a RESET followed by a full
    /// re-initialization, so USB state that did not survive sleep is rebuilt.
    /// A failed wake recovery is returned to the caller, which can hand the
    /// device to the reconnect supervisor.
    pub fn handle_power_event(&mut self, hooks: &PowerHooks<'_>) -> Result<()> {
        if let Some(sleeping) = hooks.sleeping {
            if sleeping.load(Ordering::SeqCst) {
                self.bridge.set_link(false)?;
                self.stats.last_error = Some("system_sleep".to_string());
            }
        }
        if let Some(wake) = hooks.wake {
            if wake.swap(false, Ordering::SeqCst) {
                self.stats.last_error = Some("system_wake".to_string());
                self.recover()?;
            }
        }
        Ok(())
    }

    /// Run until `stop` is set.  Transient failures are retried with a bounded
    /// number of attempts and a fixed backoff; unplug and programming errors
    /// are returned to the caller.
    pub fn run<F>(&mut self, stop: &AtomicBool, on_stats: F) -> Result<()>
    where
        F: FnMut(&EngineStats),
    {
        self.run_with_power(stop, PowerHooks::default(), on_stats)
    }

    /// Like [`run`](Self::run) but also reacts to sleep/wake flags.
    pub fn run_with_power<F>(
        &mut self,
        stop: &AtomicBool,
        power: PowerHooks<'_>,
        mut on_stats: F,
    ) -> Result<()>
    where
        F: FnMut(&EngineStats),
    {
        self.initialize()?;
        let mut last_report = Instant::now();
        on_stats(&self.stats);
        while !stop.load(Ordering::Relaxed) {
            let result = self.handle_power_event(&power).and_then(|_| self.step());
            match result {
                Ok(report) => {
                    if report.idle {
                        thread::sleep(self.config.idle_sleep);
                    }
                    if last_report.elapsed() >= Duration::from_secs(1) {
                        last_report = Instant::now();
                        on_stats(&self.stats);
                    }
                }
                Err(error) => {
                    self.stats.last_error = Some(error.category.as_str().to_string());
                    on_stats(&self.stats);
                    if !is_recoverable(error.category) {
                        let _ = self.bridge.set_link(false);
                        let _ = self.session.halt();
                        return Err(error);
                    }
                    let mut recovered = false;
                    for attempt in 0..self.config.max_reconnect_attempts {
                        thread::sleep(self.config.reconnect_backoff * (attempt + 1));
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        match self.recover() {
                            Ok(()) => {
                                recovered = true;
                                break;
                            }
                            Err(recovery_error) => {
                                self.stats.last_error =
                                    Some(recovery_error.category.as_str().to_string());
                            }
                        }
                    }
                    if !recovered {
                        let _ = self.bridge.set_link(false);
                        return Err(TetherError::new(
                            ErrorCategory::ProtocolTimeout,
                            "RNDIS session could not be recovered after repeated failures",
                        ));
                    }
                }
            }
        }
        let _ = self.session.halt();
        let _ = self.bridge.set_link(false);
        on_stats(&self.stats);
        Ok(())
    }

    pub fn stats(&self) -> &EngineStats {
        &self.stats
    }

    pub fn session(&self) -> &RndisSession<T> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut RndisSession<T> {
        &mut self.session
    }

    pub fn bridge(&self) -> &B {
        &self.bridge
    }

    pub fn bridge_mut(&mut self) -> &mut B {
        &mut self.bridge
    }

    pub fn info(&self) -> Option<&SessionInfo> {
        self.info.as_ref()
    }

    /// Consume the engine and return its session and bridge.
    ///
    /// This is used by the CLI reconnect supervisor: after a USB session
    /// fails, the bridge (feth/BPF/network service) can be kept while a new
    /// USB session is opened.
    pub fn into_parts(self) -> (RndisSession<T>, B) {
        (self.session, self.bridge)
    }
}

fn is_recoverable(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::UsbTimeout
            | ErrorCategory::ProtocolTimeout
            | ErrorCategory::UsbTransfer
            | ErrorCategory::LinkDown
    )
}
