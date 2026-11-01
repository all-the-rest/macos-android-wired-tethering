//! `android-tether` command line interface.
//!
//! The USB/RNDIS protocol process runs unprivileged.  Privileged feth/BPF/
//! network-service setup is delegated to `android-tether-helper`; the helper
//! passes only a configured BPF descriptor over a Unix socket.

mod helper_client;
mod state;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use tether_core::descriptor::{host_mac_from_key, valid_host_mac, DetectedFunction, SupportLevel};
use tether_core::engine::{EngineConfig, EngineStats, TetherEngine};
use tether_core::error::{ErrorCategory, Result, TetherError};
use tether_core::ethernet::{parse_mac, MacAddr};
use tether_core::rndis::session::{RndisSession, SessionConfig, SessionInfo};
use tether_core::testkit::{run_simulation, Scenario};
use tether_macos::bpf::BpfSocket;
use tether_macos::service::SERVICE_ID;
use tether_usb::discover::{list_devices, open_rndis, resolve_selector, DeviceInfo};
use tether_usb::transport::LibusbTransport;

use crate::helper_client::{CreatedBridge, HelperClient, RemoteFrameIo};
use crate::state::{
    clear_state, interface_ipv4, now_unix, process_matches, read_state, write_state, RuntimeState,
};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[derive(Parser, Debug)]
#[command(
    name = "android-tether",
    about = "Android USB tethering for macOS (RNDIS prototype)",
    version
)]
struct Cli {
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Unix socket of the privileged helper.
    #[arg(
        long,
        global = true,
        default_value = "/var/run/android-tether-helper.sock"
    )]
    helper: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// List USB devices and their tethering functions (no root required).
    List {
        /// Include USB devices without a recognised network function.
        #[arg(long)]
        all: bool,
    },
    /// Show the state of a running tethering session.
    Status,
    /// Run environment and USB diagnostics (no root required).
    Diagnose,
    /// Start an RNDIS session in the foreground.
    Connect {
        /// Select a device by bus:address, bus-address, or a unique address.
        #[arg(long)]
        device: Option<String>,
        /// Override the MTU (defaults to the phone's maximum frame size - 14).
        #[arg(long)]
        mtu: Option<u16>,
        /// Override the host MAC address (must be unicast).
        #[arg(long)]
        mac: Option<String>,
        /// Seconds to wait for the device after a disconnect (0 disables).
        #[arg(long, default_value_t = 60)]
        reconnect_timeout: u64,
    },
    /// Ask the running session to shut down and clean up.
    Disconnect,
    /// Remove leftovers after a crashed session (requires the helper).
    Cleanup,
    /// Run a deterministic, hardware-free RNDIS/Ethernet scenario.
    Simulate {
        /// basic, dhcp or faults.
        #[arg(long, default_value = "basic")]
        scenario: String,
    },
    /// Hardware-free macOS feth/BPF/network-service smoke test.
    NetSmoke,
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        if std::env::var_os("ANDROID_TETHER_DEBUG").is_some() {
            eprintln!("debug: {error:?}");
        }
        eprintln!("error [{}]: {}", error.category.as_str(), error.message);
        eprintln!("hint: {}", error.category.hint());
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::List { all } => command_list(cli.json, all),
        Commands::Status => command_status(cli.json),
        Commands::Diagnose => command_diagnose(cli.json, &cli.helper),
        Commands::Connect {
            device,
            mtu,
            mac,
            reconnect_timeout,
        } => command_connect(cli.json, &cli.helper, device, mtu, mac, reconnect_timeout),
        Commands::Disconnect => command_disconnect(cli.json, &cli.helper),
        Commands::Cleanup => command_cleanup(cli.json, &cli.helper),
        Commands::Simulate { scenario } => command_simulate(cli.json, &scenario),
        Commands::NetSmoke => command_net_smoke(cli.json, &cli.helper),
    }
}

// ---------------------------------------------------------------------------
// list

fn command_list(json_output: bool, all: bool) -> Result<()> {
    let devices = list_devices()?;
    let filtered: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|device| all || device.detected_functions().next().is_some())
        .collect();
    if json_output {
        let array: Vec<Value> = filtered.iter().map(|device| device_json(device)).collect();
        print_json(&json!({"devices": array}))?;
        return Ok(());
    }
    if filtered.is_empty() {
        println!("No USB devices with a recognised tethering function found.");
        if !all {
            println!("Use --all to list every USB device.");
        }
        return Ok(());
    }
    for device in filtered {
        println!("{}", device.display_name());
        if let Some(error) = &device.descriptor_error {
            println!("  descriptor warning: {error}");
        }
        for config in &device.configurations {
            println!("  configuration {}", config.value);
            if config.functions.is_empty() {
                println!("    no recognised network function");
            }
            for function in &config.functions {
                print_function(function);
            }
        }
    }
    Ok(())
}

fn print_function(function: &DetectedFunction) {
    let support = match function.support {
        SupportLevel::Usable => "usable",
        SupportLevel::DelegateNative => "native delegation",
        SupportLevel::Unsupported => "unsupported",
    };
    println!("    {} ({support})", function.protocol.as_str());
    println!("      reason: {}", function.reason);
    if let Some(function) = &function.function {
        println!(
            "      control interface {} alt {} class {:02x}/{:02x}/{:02x}",
            function.control.number,
            function.control.alt,
            function.control.class,
            function.control.subclass,
            function.control.protocol
        );
        if let Some(interrupt) = &function.control.interrupt_in {
            println!(
                "      interrupt endpoint 0x{:02x} max packet {}",
                interrupt.address, interrupt.max_packet_size
            );
        }
        println!(
            "      data interface {} alt {} bulk in 0x{:02x} out 0x{:02x}",
            function.data.number,
            function.data.alt,
            function.data.bulk_in.address,
            function.data.bulk_out.address
        );
        if let Some(ethernet) = &function.ethernet {
            println!("      max segment size {}", ethernet.max_segment_size);
        }
        if !function.notes.is_empty() {
            println!("      notes: {}", function.notes.join("; "));
        }
    }
}

fn device_json(device: &DeviceInfo) -> Value {
    let configurations: Vec<Value> = device
        .configurations
        .iter()
        .map(|config| {
            let functions: Vec<Value> = config.functions.iter().map(function_json).collect();
            json!({"value": config.value, "functions": functions})
        })
        .collect();
    json!({
        "bus": device.bus,
        "address": device.address,
        "vendor_id": format!("{:04x}", device.vendor_id),
        "product_id": format!("{:04x}", device.product_id),
        "speed": device.speed,
        "configurations": configurations,
        "descriptor_error": device.descriptor_error,
    })
}

fn function_json(function: &DetectedFunction) -> Value {
    let support = match function.support {
        SupportLevel::Usable => "usable",
        SupportLevel::DelegateNative => "delegate_native",
        SupportLevel::Unsupported => "unsupported",
    };
    let details = function.function.as_ref().map(|function| {
        json!({
            "control_interface": function.control.number,
            "control_alt": function.control.alt,
            "control_class": format!("{:02x}/{:02x}/{:02x}", function.control.class, function.control.subclass, function.control.protocol),
            "interrupt_endpoint": function.control.interrupt_in.as_ref().map(|endpoint| endpoint.address),
            "data_interface": function.data.number,
            "data_alt": function.data.alt,
            "bulk_in": function.data.bulk_in.address,
            "bulk_out": function.data.bulk_out.address,
            "max_segment_size": function.ethernet.as_ref().map(|ethernet| ethernet.max_segment_size),
        })
    });
    json!({
        "protocol": function.protocol.as_str(),
        "support": support,
        "reason": function.reason,
        "details": details,
    })
}

// ---------------------------------------------------------------------------
// status / disconnect / cleanup

fn command_status(json_output: bool) -> Result<()> {
    let Some(state) = read_state()? else {
        if json_output {
            print_json(&json!({"connected": false, "reason": "no state file"}))?;
        } else {
            println!("Not connected (no state file).");
        }
        return Ok(());
    };
    let alive = process_matches(&state);
    if json_output {
        let mut value = serde_json::to_value(&state)
            .map_err(|error| TetherError::internal(format!("cannot serialize state: {error}")))?;
        value["connected"] = json!(alive);
        print_json(&value)?;
        return Ok(());
    }
    if !alive {
        println!(
            "Stale state for pid {} ({}); run 'android-tether cleanup'.",
            state.pid, state.host_iface
        );
        return Ok(());
    }
    println!(
        "Connected: {} over {}, host {} peer {}",
        state.protocol, state.host_iface, state.host_iface, state.peer_iface
    );
    println!(
        "  device bus {:03} address {:03} {:04x}:{:04x}",
        state.bus, state.address, state.vendor_id, state.product_id
    );
    println!("  service: {}", state.service);
    println!("  mtu: {}  host MAC: {}", state.mtu, state.host_mac);
    println!(
        "  IPv4: {}",
        state
            .ipv4
            .as_deref()
            .unwrap_or("none (DHCP pending or no peer)")
    );
    println!(
        "  frames: phone->host {}  host->phone {}",
        state.frames_phone_to_host, state.frames_host_to_phone
    );
    println!(
        "  bytes:  phone->host {}  host->phone {}",
        state.bytes_phone_to_host, state.bytes_host_to_phone
    );
    if let Some(error) = &state.last_error {
        println!("  last error: {error}");
    }
    Ok(())
}

fn command_disconnect(json_output: bool, helper_path: &Path) -> Result<()> {
    let Some(state) = read_state()? else {
        let (helper_cleaned, helper_note) = destroy_helper_session(helper_path);
        if json_output {
            print_json(&json!({
                "disconnected": helper_cleaned,
                "reason": "no state",
                "helper_cleaned": helper_cleaned,
                "helper": helper_note,
            }))?;
        } else {
            println!("Not running.");
            if helper_cleaned {
                println!("Asked the helper to destroy its session.");
            }
            if let Some(note) = helper_note {
                println!("Helper note: {note}");
            }
        }
        return Ok(());
    };
    if !process_matches(&state) {
        clear_state();
        let (helper_cleaned, helper_note) = destroy_helper_session(helper_path);
        if json_output {
            print_json(&json!({
                "disconnected": true,
                "reason": "process already gone",
                "helper_cleaned": helper_cleaned,
                "helper": helper_note,
            }))?;
        } else {
            println!(
                "Process {} is already gone or no longer belongs to android-tether; stale state removed.",
                state.pid
            );
            if helper_cleaned {
                println!("Asked the helper to destroy its session.");
            }
            if let Some(note) = helper_note {
                println!("Helper note: {note}");
            }
        }
        return Ok(());
    }
    let result = unsafe { libc::kill(state.pid as libc::pid_t, libc::SIGTERM) };
    if result != 0 {
        return Err(TetherError::new(
            ErrorCategory::PermissionRequired,
            format!(
                "cannot signal pid {}: {}",
                state.pid,
                std::io::Error::last_os_error()
            ),
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !process_matches(&state) {
            clear_state();
            if json_output {
                print_json(&json!({"disconnected": true, "pid": state.pid}))?;
            } else {
                println!("Disconnected.");
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(TetherError::new(
        ErrorCategory::ProtocolTimeout,
        format!("process {} did not exit within 10 seconds", state.pid),
    ))
}

fn command_cleanup(json_output: bool, helper_path: &Path) -> Result<()> {
    let Some(state) = read_state()? else {
        let (helper_cleaned, helper_note) = destroy_helper_session(helper_path);
        if json_output {
            print_json(&json!({
                "cleaned": true,
                "reason": "no state",
                "helper_removed_reserved_service": helper_cleaned,
                "helper": helper_note,
            }))?;
        } else {
            println!("Nothing to clean up (no state).");
            if helper_cleaned {
                println!("Asked the helper to remove the reserved network service.");
            }
            if let Some(note) = helper_note {
                println!("Helper note: {note}");
            }
        }
        return Ok(());
    };
    if process_matches(&state) {
        return Err(TetherError::new(
            ErrorCategory::Configuration,
            format!(
                "session pid {} is still running; use 'android-tether disconnect' first",
                state.pid
            ),
        ));
    }
    let (helper_cleaned, helper_note) = destroy_helper_session(helper_path);

    let interfaces = ifconfig_list().unwrap_or_default();
    let mut leftovers = Vec::new();
    if interfaces.iter().any(|name| name == &state.host_iface) {
        leftovers.push(format!("interface {}", state.host_iface));
    }
    if interfaces.iter().any(|name| name == &state.peer_iface) {
        leftovers.push(format!("interface {}", state.peer_iface));
    }
    if scutil_service_exists() {
        leftovers.push(format!("network service '{}'", state.service));
    }

    if leftovers.is_empty() {
        clear_state();
        if json_output {
            print_json(&json!({
                "cleaned": true,
                "helper_cleaned": helper_cleaned,
                "helper": helper_note,
            }))?;
        } else {
            println!("Cleanup complete; no leftover interfaces or services.");
            if let Some(note) = helper_note {
                println!("Helper note: {note}");
            }
        }
        return Ok(());
    }
    let manual = format!(
        "start android-tether-helper as root (allow uid {}), then rerun 'android-tether cleanup'; if interfaces remain: sudo /sbin/ifconfig {} destroy; sudo /sbin/ifconfig {} destroy",
        unsafe { libc::getuid() },
        state.host_iface, state.peer_iface
    );
    if json_output {
        print_json(&json!({
            "cleaned": false,
            "leftovers": leftovers,
            "helper": helper_note,
            "manual": manual,
        }))?;
    } else {
        println!("Cleanup incomplete. Leftovers: {}", leftovers.join(", "));
        if let Some(note) = helper_note {
            println!("Helper: {note}");
        }
        println!("Manual command (review names first): {manual}");
    }
    Err(TetherError::new(
        ErrorCategory::Network,
        "leftovers remain; run the printed manual cleanup after reviewing it",
    ))
}

/// Ask a running helper to destroy its session without a valid state file.
/// Returns whether the helper accepted the request and an optional note.
fn destroy_helper_session(helper_path: &Path) -> (bool, Option<String>) {
    match HelperClient::connect(helper_path) {
        Ok(mut helper) => match helper.hello() {
            Ok(()) => match helper.destroy() {
                Ok(()) => (true, None),
                Err(error) => (false, Some(error.message)),
            },
            Err(error) => (false, Some(error.message)),
        },
        Err(error) => (false, Some(error.message)),
    }
}

// ---------------------------------------------------------------------------
// diagnose

fn command_diagnose(json_output: bool, helper_path: &Path) -> Result<()> {
    let mut checks = Vec::new();
    checks.push(check("architecture", std::env::consts::ARCH, "pass"));
    checks.push(check(
        "os",
        &command_output("sw_vers", &["-productVersion"]).unwrap_or_else(|| "unknown".into()),
        "pass",
    ));
    let root = unsafe { libc::geteuid() } == 0;
    checks.push(check("root", if root { "yes" } else { "no" }, "pass"));
    checks.push(check(
        "ifconfig",
        if Path::new("/sbin/ifconfig").exists() {
            "present"
        } else {
            "missing"
        },
        if Path::new("/sbin/ifconfig").exists() {
            "pass"
        } else {
            "fail"
        },
    ));
    checks.push(check(
        "scutil",
        if Path::new("/usr/sbin/scutil").exists() {
            "present"
        } else {
            "missing"
        },
        if Path::new("/usr/sbin/scutil").exists() {
            "pass"
        } else {
            "fail"
        },
    ));
    checks.push(check(
        "feth_interfaces",
        &ifconfig_list()
            .map(|names| {
                names
                    .iter()
                    .filter(|name| name.starts_with("feth"))
                    .count()
                    .to_string()
            })
            .unwrap_or_else(|error| error.message),
        "pass",
    ));
    match OpenOptions::new().read(true).write(true).open("/dev/bpf0") {
        Ok(_) => checks.push(check("bpf0", "openable", "pass")),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => checks.push(check(
            "bpf0",
            "permission denied (helper/root required)",
            "skip",
        )),
        Err(error) => checks.push(check("bpf0", &error.to_string(), "warn")),
    }
    match helper_client::probe(helper_path) {
        Ok(status) if status == "none" => checks.push(check(
            "helper",
            "reachable and authenticated; no active session",
            "pass",
        )),
        Ok(status) => checks.push(check(
            "helper",
            &format!("reachable and authenticated; active session: {status}"),
            "pass",
        )),
        Err(error) => checks.push(check("helper", &error.message, "warn")),
    }

    let devices = list_devices()?;
    if json_output {
        let device_values: Vec<Value> = devices.iter().map(device_json).collect();
        let check_values: Vec<Value> = checks
            .iter()
            .map(
                |check| json!({"name": check.name, "detail": check.detail, "status": check.status}),
            )
            .collect();
        print_json(&json!({"checks": check_values, "devices": device_values}))?;
        return Ok(());
    }
    println!("Environment:");
    for check in &checks {
        println!(
            "  [{}] {}: {}",
            check.status.to_uppercase(),
            check.name,
            check.detail
        );
    }
    println!("USB devices:");
    if devices.is_empty() {
        println!("  none");
    }
    for device in &devices {
        println!("  {}", device.display_name());
        if let Some(error) = &device.descriptor_error {
            println!("    descriptor warning: {error}");
        }
        for function in device.detected_functions() {
            println!(
                "    {}: {} ({})",
                function.protocol.as_str(),
                function.reason,
                support_label(function.support)
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct Check {
    name: String,
    detail: String,
    status: &'static str,
}

fn check(name: &str, detail: &str, status: &'static str) -> Check {
    Check {
        name: name.to_string(),
        detail: detail.to_string(),
        status,
    }
}

fn support_label(support: SupportLevel) -> &'static str {
    match support {
        SupportLevel::Usable => "usable",
        SupportLevel::DelegateNative => "delegate to macOS native driver",
        SupportLevel::Unsupported => "unsupported",
    }
}

// ---------------------------------------------------------------------------
// connect

fn command_connect(
    json_output: bool,
    helper_path: &Path,
    selector: Option<String>,
    mtu_override: Option<u16>,
    mac_override: Option<String>,
    reconnect_timeout_secs: u64,
) -> Result<()> {
    let devices = list_devices()?;
    let mut device = choose_device(&devices, selector.as_deref())?.clone();
    let mut helper = HelperClient::connect(helper_path)?;
    helper.hello()?;

    let initial_config = SessionConfig {
        host_mac_fallback: host_mac_fallback(&device),
        ..SessionConfig::default()
    };
    let (session, info) = open_session(&device, initial_config)?;
    let max_mtu = info.max_frame_size.saturating_sub(14).clamp(576, 1500) as u16;
    let mtu = mtu_override.unwrap_or(max_mtu);
    if !(576..=max_mtu).contains(&mtu) {
        return Err(TetherError::new(
            ErrorCategory::Configuration,
            format!("MTU {mtu} is outside the device-supported range 576..={max_mtu}"),
        ));
    }
    let host_mac = match mac_override {
        Some(text) => parse_mac(&text)?,
        None => MacAddr(info.host_mac),
    };
    if !valid_host_mac(&host_mac.0) || host_mac.is_multicast() {
        return Err(TetherError::new(
            ErrorCategory::Configuration,
            "host MAC must be a non-zero unicast address",
        ));
    }

    let created = helper.create(mtu, &host_mac.to_string())?;
    let bpf = match BpfSocket::from_raw_fd(created.fd, &created.peer_iface) {
        Ok(bpf) => bpf,
        Err(error) => {
            let _ = helper.destroy();
            return Err(error);
        }
    };
    let bridge = RemoteFrameIo::new(bpf, helper);
    let mut engine = TetherEngine::new(session, bridge, EngineConfig::default());
    let engine_info = match engine.initialize() {
        Ok(info) => info,
        Err(error) => {
            engine.bridge_mut().shutdown();
            return Err(error);
        }
    };
    let started = now_unix();
    let mut runtime = RuntimeState {
        pid: std::process::id(),
        started_unix: started,
        updated_unix: started,
        exe: std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        protocol: engine_info.protocol.as_str().to_string(),
        bus: device.bus,
        address: device.address,
        vendor_id: device.vendor_id,
        product_id: device.product_id,
        host_iface: created.host_iface.clone(),
        peer_iface: created.peer_iface.clone(),
        service: created.service.clone(),
        helper_socket: helper_path.display().to_string(),
        host_mac: host_mac.to_string(),
        mtu,
        ipv4: None,
        frames_phone_to_host: 0,
        frames_host_to_phone: 0,
        bytes_phone_to_host: 0,
        bytes_host_to_phone: 0,
        reconnects: 0,
        last_error: None,
    };
    install_signal_handlers()?;
    write_state(&runtime)?;
    if !json_output {
        println!(
            "Connected: {} over {} (peer {}, service '{}', MTU {})",
            runtime.protocol, runtime.host_iface, runtime.peer_iface, runtime.service, runtime.mtu
        );
        println!("Press Ctrl-C to disconnect.");
    }

    let mut total_reconnects = 0u64;
    let result = engine.run(&STOP, |stats| {
        update_runtime(&mut runtime, stats);
        runtime.reconnects = total_reconnects + stats.reconnects;
        if let Err(error) = write_state(&runtime) {
            eprintln!("warning: cannot update state: {}", error.message);
        }
    });
    let (session, bridge_back) = engine.into_parts();
    drop(session);
    let mut bridge = bridge_back;

    match result {
        Ok(()) => {
            bridge.shutdown();
            clear_state();
            if json_output {
                print_json(&json!({"connected": false, "cleanup": true}))?;
            }
            return Ok(());
        }
        Err(error)
            if !STOP.load(Ordering::Relaxed)
                && reconnect_timeout_secs > 0
                && is_reconnectable(error.category) =>
        {
            total_reconnects += 1;
            runtime.reconnects = total_reconnects;
            runtime.last_error = Some(error.category.as_str().to_string());
            runtime.updated_unix = now_unix();
            let _ = write_state(&runtime);
            if !json_output {
                eprintln!(
                    "Connection lost ({}); waiting up to {}s for the device...",
                    error.category.as_str(),
                    reconnect_timeout_secs
                );
            }
        }
        Err(error) => {
            bridge.shutdown();
            clear_state();
            return Err(error);
        }
    }

    let reconnect_timeout = Duration::from_secs(reconnect_timeout_secs);
    loop {
        let next = match wait_for_device(selector.as_deref(), &STOP, reconnect_timeout) {
            Ok(Some(device)) => device,
            Ok(None) => break,
            Err(error) => {
                bridge.shutdown();
                clear_state();
                return Err(error);
            }
        };
        device = next;

        let config = SessionConfig {
            host_mac_fallback: host_mac_fallback(&device),
            host_mac_override: Some(host_mac.0),
            ..SessionConfig::default()
        };
        let (session, info) = match open_session(&device, config) {
            Ok(value) => value,
            Err(error) if !STOP.load(Ordering::Relaxed) && is_reconnectable(error.category) => {
                continue;
            }
            Err(error) => {
                bridge.shutdown();
                clear_state();
                return Err(error);
            }
        };
        let max_mtu = info.max_frame_size.saturating_sub(14).clamp(576, 1500) as u16;
        if runtime.mtu > max_mtu {
            bridge.shutdown();
            clear_state();
            return Err(TetherError::new(
                ErrorCategory::Configuration,
                format!(
                    "reconnected device supports MTU {max_mtu}, but the bridge uses {}; run disconnect and connect again",
                    runtime.mtu
                ),
            ));
        }
        runtime.bus = device.bus;
        runtime.address = device.address;
        runtime.vendor_id = device.vendor_id;
        runtime.product_id = device.product_id;
        runtime.protocol = info.protocol.as_str().to_string();
        runtime.updated_unix = now_unix();

        let mut engine = TetherEngine::new(session, bridge, EngineConfig::default());
        if let Err(error) = engine.initialize() {
            let (session, bridge_back) = engine.into_parts();
            drop(session);
            bridge = bridge_back;
            if !STOP.load(Ordering::Relaxed) && is_reconnectable(error.category) {
                continue;
            }
            bridge.shutdown();
            clear_state();
            return Err(error);
        }
        if let Err(error) = write_state(&runtime) {
            eprintln!("warning: cannot update state: {}", error.message);
        }
        let base = total_reconnects;
        let result = engine.run(&STOP, |stats| {
            update_runtime(&mut runtime, stats);
            runtime.reconnects = base + stats.reconnects;
            if let Err(error) = write_state(&runtime) {
                eprintln!("warning: cannot update state: {}", error.message);
            }
        });
        let (session, bridge_back) = engine.into_parts();
        drop(session);
        bridge = bridge_back;
        match result {
            Ok(()) => break,
            Err(error) if !STOP.load(Ordering::Relaxed) && is_reconnectable(error.category) => {
                total_reconnects += 1;
                runtime.reconnects = total_reconnects;
                runtime.last_error = Some(error.category.as_str().to_string());
                runtime.updated_unix = now_unix();
                let _ = write_state(&runtime);
                if !json_output {
                    eprintln!(
                        "Connection lost ({}); waiting up to {}s for the device...",
                        error.category.as_str(),
                        reconnect_timeout_secs
                    );
                }
            }
            Err(error) => {
                bridge.shutdown();
                clear_state();
                return Err(error);
            }
        }
    }

    bridge.shutdown();
    clear_state();
    if json_output {
        print_json(&json!({"connected": false, "cleanup": true}))?;
    }
    Ok(())
}

fn open_session(
    device: &DeviceInfo,
    config: SessionConfig,
) -> Result<(RndisSession<LibusbTransport>, SessionInfo)> {
    let opened = open_rndis(device.bus, device.address)?;
    let function = opened.function.clone();
    let mut session = RndisSession::new(LibusbTransport::new(opened), &function, config)?;
    let info = session.initialize()?;
    Ok((session, info))
}

fn host_mac_fallback(device: &DeviceInfo) -> [u8; 6] {
    host_mac_from_key(
        format!(
            "{:04x}:{:04x}:{}:{}",
            device.vendor_id, device.product_id, device.bus, device.address
        )
        .as_bytes(),
    )
}

fn wait_for_device(
    selector: Option<&str>,
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<Option<DeviceInfo>> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if let Ok(devices) = list_devices() {
            match choose_device(&devices, selector) {
                Ok(device) => return Ok(Some(device.clone())),
                Err(error) if error.category == ErrorCategory::UsbNotFound => {}
                Err(error) => return Err(error),
            }
        }
        if Instant::now() >= deadline {
            return Err(TetherError::new(
                ErrorCategory::UsbNotFound,
                format!(
                    "no usable RNDIS device reappeared within {} seconds",
                    timeout.as_secs()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn is_reconnectable(category: ErrorCategory) -> bool {
    matches!(
        category,
        ErrorCategory::UsbNotFound
            | ErrorCategory::UsbBusy
            | ErrorCategory::UsbTimeout
            | ErrorCategory::UsbTransfer
            | ErrorCategory::ProtocolTimeout
            | ErrorCategory::ProtocolViolation
            | ErrorCategory::LinkDown
    )
}

fn update_runtime(runtime: &mut RuntimeState, stats: &EngineStats) {
    runtime.updated_unix = now_unix();
    runtime.frames_phone_to_host = stats.frames_phone_to_host;
    runtime.frames_host_to_phone = stats.frames_host_to_phone;
    runtime.bytes_phone_to_host = stats.bytes_phone_to_host;
    runtime.bytes_host_to_phone = stats.bytes_host_to_phone;
    runtime.reconnects = stats.reconnects;
    runtime.last_error = stats.last_error.clone();
    runtime.ipv4 = interface_ipv4(&runtime.host_iface);
}

fn choose_device<'a>(devices: &'a [DeviceInfo], selector: Option<&str>) -> Result<&'a DeviceInfo> {
    if let Some(selector) = selector {
        let (bus, address) = resolve_selector(selector, devices)?;
        let device = devices
            .iter()
            .find(|device| device.bus == bus && device.address == address)
            .ok_or_else(|| TetherError::unsupported("selected device disappeared"))?;
        if device.usable_rndis().is_none() {
            return Err(unsupported_device_error(device));
        }
        return Ok(device);
    }
    if let Some(device) = devices
        .iter()
        .find(|device| device.usable_rndis().is_some())
    {
        return Ok(device);
    }
    let detected: Vec<&DeviceInfo> = devices
        .iter()
        .filter(|device| device.detected_functions().next().is_some())
        .collect();
    if let Some(device) = detected.first() {
        return Err(unsupported_device_error(device));
    }
    Err(TetherError::new(
        ErrorCategory::UsbNotFound,
        "no USB tethering device found; enable USB tethering on the phone",
    ))
}

fn unsupported_device_error(device: &DeviceInfo) -> TetherError {
    let mut protocols = Vec::new();
    for function in device.detected_functions() {
        protocols.push(function.protocol.as_str());
    }
    if protocols
        .iter()
        .any(|protocol| *protocol == "CDC-NCM" || *protocol == "CDC-ECM")
    {
        TetherError::unsupported(
            "detected CDC-NCM/CDC-ECM; use the native macOS driver (this prototype drives RNDIS)",
        )
    } else if protocols.is_empty() {
        TetherError::unsupported(format!(
            "{} has no recognised CDC network function{}",
            device.display_name(),
            device
                .descriptor_error
                .as_ref()
                .map(|error| format!(" ({error})"))
                .unwrap_or_default()
        ))
    } else {
        TetherError::unsupported(format!(
            "{} exposes only unsupported tethering functions: {}",
            device.display_name(),
            protocols.join(", ")
        ))
    }
}

// ---------------------------------------------------------------------------
// simulate

fn command_simulate(json_output: bool, scenario: &str) -> Result<()> {
    let scenario = Scenario::parse(scenario)?;
    let report = run_simulation(scenario)?;
    if json_output {
        print_json(&json!({
            "scenario": report.scenario.as_str(),
            "protocol": report.info.protocol.as_str(),
            "host_mac": MacAddr(report.info.host_mac).to_string(),
            "host_mac_from_device": report.info.host_mac_from_device,
            "max_frame_size": report.info.max_frame_size,
            "max_transfer_size": report.info.max_transfer_size,
            "packet_filter": format!("0x{:08x}", report.info.packet_filter),
            "frames_host_to_phone": report.frames_host_to_phone,
            "frames_phone_to_host": report.frames_phone_to_host,
            "bytes_host_to_phone": report.bytes_host_to_phone,
            "bytes_phone_to_host": report.bytes_phone_to_host,
            "received_by_device": report.received_by_device,
            "delivered_to_host": report.delivered_to_host,
            "link_changes": report.link_changes,
            "reconnects": report.reconnects,
            "warnings": report.warnings,
        }))?;
        return Ok(());
    }
    println!("Synthetic scenario: {}", report.scenario.as_str());
    println!(
        "  protocol={} host_mac={} mtu={} max_transfer={}",
        report.info.protocol.as_str(),
        MacAddr(report.info.host_mac),
        report.info.max_frame_size.saturating_sub(14),
        report.info.max_transfer_size
    );
    println!(
        "  frames host->phone={} phone->host={}",
        report.frames_host_to_phone, report.frames_phone_to_host
    );
    println!(
        "  bytes  host->phone={} phone->host={}",
        report.bytes_host_to_phone, report.bytes_phone_to_host
    );
    println!(
        "  device received={} host received={} link changes={} reconnects={}",
        report.received_by_device, report.delivered_to_host, report.link_changes, report.reconnects
    );
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// net-smoke

fn command_net_smoke(json_output: bool, helper_path: &Path) -> Result<()> {
    let mut checks = vec![
        check("architecture", std::env::consts::ARCH, "pass"),
        check(
            "os",
            &command_output("sw_vers", &["-productVersion"]).unwrap_or_else(|| "unknown".into()),
            "pass",
        ),
    ];
    let mut skipped = false;
    let mut passed = true;
    match smoke_with_helper(helper_path) {
        Ok(mut smoke_checks) => checks.append(&mut smoke_checks),
        Err(error)
            if matches!(
                error.category,
                ErrorCategory::HelperUnavailable | ErrorCategory::PermissionRequired
            ) =>
        {
            skipped = true;
            passed = false;
            checks.push(check(
                "helper_smoke",
                &format!(
                    "skipped: {} (start the helper as root to exercise feth/BPF/service)",
                    error.message
                ),
                "skip",
            ));
        }
        Err(error) => {
            passed = false;
            checks.push(check("helper_smoke", &error.message, "fail"));
        }
    }
    for check in &checks {
        if check.status == "fail" {
            passed = false;
        }
    }
    if json_output {
        let values: Vec<Value> = checks
            .iter()
            .map(
                |check| json!({"name": check.name, "detail": check.detail, "status": check.status}),
            )
            .collect();
        print_json(&json!({"passed": passed, "skipped": skipped, "checks": values}))?;
    } else {
        println!("macOS network integration smoke test:");
        for check in &checks {
            println!(
                "  [{}] {}: {}",
                check.status.to_uppercase(),
                check.name,
                check.detail
            );
        }
        if skipped {
            println!("Result: SKIPPED (privileged helper not running).");
        } else if passed {
            println!("Result: PASS (no phone attached).");
        } else {
            println!("Result: FAIL.");
        }
    }
    // Exit 3 is distinct from a normal error (1) so that automation cannot
    // mistake a skipped privileged smoke test for a pass.
    if skipped {
        std::process::exit(3);
    }
    if !passed {
        return Err(TetherError::new(
            ErrorCategory::Network,
            "macOS network integration smoke test failed",
        ));
    }
    Ok(())
}

fn smoke_with_helper(helper_path: &Path) -> Result<Vec<Check>> {
    let mut client = HelperClient::connect(helper_path)?;
    client.hello()?;
    let mac = MacAddr(host_mac_from_key(
        format!("smoke:{}:{}", std::process::id(), now_unix()).as_bytes(),
    ));
    let created: CreatedBridge = client.create(1500, &mac.to_string())?;
    let mut guard = SmokeGuard {
        client: Some(client),
        bpf: None,
    };
    let bpf = BpfSocket::from_raw_fd(created.fd, &created.peer_iface)?;
    let mut checks = Vec::new();
    checks.push(check(
        "feth_pair",
        &format!("{} <-> {}", created.host_iface, created.peer_iface),
        "pass",
    ));
    checks.push(check(
        "bpf_descriptor",
        "passed via SCM_RIGHTS and usable",
        "pass",
    ));
    let interfaces = ifconfig_list()?;
    let host_present = interfaces.iter().any(|name| name == &created.host_iface);
    checks.push(check(
        "host_interface",
        if host_present { "present" } else { "missing" },
        if host_present { "pass" } else { "fail" },
    ));
    guard.bpf = Some(bpf);
    let up = guard
        .client
        .as_mut()
        .expect("client present")
        .link(true)
        .is_ok();
    checks.push(check(
        "link_up",
        if up { "ok" } else { "failed" },
        if up { "pass" } else { "fail" },
    ));
    let link_state = wait_interface_up(&created.host_iface, Duration::from_secs(2))?;
    checks.push(check(
        "link_state",
        if link_state {
            "host interface reports UP"
        } else {
            "host interface did not report UP"
        },
        if link_state { "pass" } else { "fail" },
    ));
    let down = guard
        .client
        .as_mut()
        .expect("client present")
        .link(false)
        .is_ok();
    checks.push(check(
        "link_down",
        if down { "ok" } else { "failed" },
        if down { "pass" } else { "fail" },
    ));
    let service_present = scutil_service_exists();
    checks.push(check(
        "network_service",
        if service_present {
            "present while session active"
        } else {
            "missing"
        },
        if service_present { "pass" } else { "fail" },
    ));
    guard.cleanup()?;
    let removed = wait_interface_absent(&created.host_iface, Duration::from_secs(3))?;
    checks.push(check(
        "cleanup",
        if removed {
            "interface removed"
        } else {
            "interface still present"
        },
        if removed { "pass" } else { "fail" },
    ));
    Ok(checks)
}

struct SmokeGuard {
    client: Option<HelperClient>,
    bpf: Option<BpfSocket>,
}

impl SmokeGuard {
    fn cleanup(&mut self) -> Result<()> {
        self.bpf.take();
        if let Some(client) = self.client.as_mut() {
            client.destroy()?;
        }
        self.client.take();
        Ok(())
    }
}

impl Drop for SmokeGuard {
    fn drop(&mut self) {
        self.bpf.take();
        if let Some(client) = self.client.as_mut() {
            let _ = client.link(false);
            let _ = client.destroy();
        }
    }
}

// ---------------------------------------------------------------------------
// helpers

fn install_signal_handlers() -> Result<()> {
    STOP.store(false, Ordering::SeqCst);
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let previous =
            unsafe { libc::signal(signal, handle_signal as *const () as libc::sighandler_t) };
        if previous == libc::SIG_ERR {
            return Err(TetherError::internal(format!(
                "cannot install signal handler: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

fn ifconfig_list() -> Result<Vec<String>> {
    let output = Command::new("/sbin/ifconfig")
        .arg("-l")
        .output()
        .map_err(|error| TetherError::network(format!("cannot run ifconfig: {error}")))?;
    if !output.status.success() {
        return Err(TetherError::network("ifconfig -l failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

fn wait_interface_absent(name: &str, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let interfaces = ifconfig_list()?;
        if !interfaces.iter().any(|interface| interface == name) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn ifconfig_reports_up(output: &str) -> bool {
    output
        .lines()
        .next()
        .map(|line| line.contains("<UP,") || line.contains("<UP>"))
        .unwrap_or(false)
}

fn wait_interface_up(name: &str, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        let output = Command::new("/sbin/ifconfig").arg(name).output();
        if let Ok(output) = output {
            if output.status.success()
                && ifconfig_reports_up(&String::from_utf8_lossy(&output.stdout))
            {
                return Ok(true);
            }
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// Read-only check for our fixed service UUID.  No root privileges needed.
fn scutil_service_exists() -> bool {
    use std::io::Write;
    let mut child = match Command::new("/usr/sbin/scutil")
        .arg("--prefs")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(b"list /NetworkServices\nquit\n");
    }
    match child.wait_with_output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).contains(SERVICE_ID),
        Err(_) => false,
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn print_json(value: &Value) -> Result<()> {
    println!(
        "{}",
        serde_json::to_string_pretty(value)
            .map_err(|error| TetherError::internal(format!("cannot format JSON: {error}")))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_up_flag_from_ifconfig() {
        assert!(ifconfig_reports_up(
            "feth100: flags=8863<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n"
        ));
        assert!(ifconfig_reports_up("feth100: flags=1<UP> mtu 1500\n"));
        assert!(!ifconfig_reports_up(
            "feth100: flags=8862<BROADCAST,SIMPLEX,MULTICAST> mtu 1500\n"
        ));
        assert!(!ifconfig_reports_up(""));
    }

    #[test]
    fn reconnectable_categories_are_explicit() {
        assert!(is_reconnectable(ErrorCategory::UsbNotFound));
        assert!(is_reconnectable(ErrorCategory::UsbTransfer));
        assert!(is_reconnectable(ErrorCategory::ProtocolTimeout));
        assert!(!is_reconnectable(ErrorCategory::UsbPermission));
        assert!(!is_reconnectable(ErrorCategory::Configuration));
    }
}
