//! Local runtime state for `status`/`disconnect`/`cleanup`.
//!
//! The file contains operational metadata only: no USB serial number, no
//! packet payloads and no credentials.  It is written atomically.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tether_core::error::{ErrorCategory, Result, TetherError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeState {
    pub pid: u32,
    pub started_unix: u64,
    pub updated_unix: u64,
    /// Absolute path of the running CLI binary.  Older state files lack this
    /// field; `process_matches` then falls back to the command name.
    #[serde(default)]
    pub exe: String,
    pub protocol: String,
    pub bus: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub host_iface: String,
    pub peer_iface: String,
    pub service: String,
    pub helper_socket: String,
    pub host_mac: String,
    pub mtu: u16,
    /// IPv4 address macOS assigned to the host interface, if any.
    #[serde(default)]
    pub ipv4: Option<String>,
    pub frames_phone_to_host: u64,
    pub frames_host_to_phone: u64,
    pub bytes_phone_to_host: u64,
    pub bytes_host_to_phone: u64,
    pub reconnects: u64,
    pub last_error: Option<String>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub fn state_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| TetherError::new(ErrorCategory::Configuration, "HOME is not set"))?;
    let directory = home
        .join("Library")
        .join("Application Support")
        .join("AndroidUsbTether");
    fs::create_dir_all(&directory).map_err(|error| {
        TetherError::new(
            ErrorCategory::Configuration,
            format!("cannot create state directory: {error}"),
        )
    })?;
    Ok(directory.join("state.json"))
}

pub fn read_state() -> Result<Option<RuntimeState>> {
    let path = state_path()?;
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|error| {
            TetherError::new(
                ErrorCategory::Configuration,
                format!("state file is corrupt: {error}"),
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(TetherError::new(
            ErrorCategory::Configuration,
            format!("cannot read state file: {error}"),
        )),
    }
}

pub fn write_state(state: &RuntimeState) -> Result<()> {
    let path = state_path()?;
    let temporary = path.with_extension(format!("tmp.{}", state.pid));
    let bytes = serde_json::to_vec_pretty(state).map_err(|error| {
        TetherError::new(
            ErrorCategory::Internal,
            format!("cannot serialize state: {error}"),
        )
    })?;
    fs::write(&temporary, bytes).map_err(|error| {
        TetherError::new(
            ErrorCategory::Configuration,
            format!("cannot write state: {error}"),
        )
    })?;
    // The file contains only operational metadata, but there is no reason to
    // expose it to other local users.
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|error| {
        TetherError::new(
            ErrorCategory::Configuration,
            format!("cannot protect state file: {error}"),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        TetherError::new(
            ErrorCategory::Configuration,
            format!("cannot commit state: {error}"),
        )
    })?;
    Ok(())
}

pub fn clear_state() {
    if let Ok(path) = state_path() {
        let _ = fs::remove_file(path);
    }
}

pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Return the executable path/command macOS reports for a live PID.
pub fn process_command(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "comm="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Check that a state file still refers to this project's CLI process.
///
/// A crashed session can leave a stale state file behind; the PID may then be
/// recycled by an unrelated process.  `disconnect` and `cleanup` must never
/// signal such a process.  The executable path written into the state is the
/// primary check, with a command-name fallback for older state files.
pub fn process_matches(state: &RuntimeState) -> bool {
    if !process_alive(state.pid) {
        return false;
    }
    let Some(command) = process_command(state.pid) else {
        return false;
    };
    if !state.exe.is_empty() {
        if command == state.exe {
            return true;
        }
        let expected = Path::new(&state.exe)
            .file_name()
            .and_then(|name| name.to_str());
        let actual = Path::new(&command)
            .file_name()
            .and_then(|name| name.to_str());
        return expected.is_some() && expected == actual;
    }
    command.contains("android-tether")
}

/// Read the IPv4 address macOS assigned to an interface.  This is a read-only
/// observation; DHCP itself remains owned by macOS.
pub fn interface_ipv4(interface: &str) -> Option<String> {
    if interface.is_empty() {
        return None;
    }
    let output = std::process::Command::new("/usr/sbin/ipconfig")
        .args(["getifaddr", interface])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ipv4_output(&output.stdout)
}

fn parse_ipv4_output(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes).trim().to_string();
    text.parse::<std::net::Ipv4Addr>()
        .ok()
        .map(|address| address.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state(pid: u32) -> RuntimeState {
        RuntimeState {
            pid,
            started_unix: 1,
            updated_unix: 2,
            exe: std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            protocol: "RNDIS".into(),
            bus: 1,
            address: 2,
            vendor_id: 0x04e8,
            product_id: 0x6860,
            host_iface: "feth100".into(),
            peer_iface: "feth1100".into(),
            service: "Android USB Tethering (tetherd)".into(),
            helper_socket: "/var/run/android-tether-helper.sock".into(),
            host_mac: "02:00:00:00:00:01".into(),
            mtu: 1500,
            ipv4: Some("192.168.42.2".into()),
            frames_phone_to_host: 0,
            frames_host_to_phone: 0,
            bytes_phone_to_host: 0,
            bytes_host_to_phone: 0,
            reconnects: 0,
            last_error: None,
        }
    }

    #[test]
    fn roundtrip_state_json() {
        let state = sample_state(42);
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("serial"));
        let parsed: RuntimeState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.host_iface, "feth100");
        assert_eq!(parsed.mtu, 1500);
    }

    #[test]
    fn state_without_exe_still_deserializes() {
        let mut value = serde_json::to_value(sample_state(42)).unwrap();
        value.as_object_mut().unwrap().remove("exe");
        let parsed: RuntimeState = serde_json::from_value(value).unwrap();
        assert!(parsed.exe.is_empty());
    }

    #[test]
    fn dead_pid_is_not_alive() {
        assert!(!process_alive(0));
        assert!(!process_matches(&sample_state(0)));
    }

    #[test]
    fn current_process_matches_its_own_state() {
        assert!(process_matches(&sample_state(std::process::id())));
    }

    #[test]
    fn parses_ipv4_output_conservatively() {
        assert_eq!(
            parse_ipv4_output(b"192.168.42.2\n").as_deref(),
            Some("192.168.42.2")
        );
        assert_eq!(parse_ipv4_output(b"fe80::1\n"), None);
        assert_eq!(parse_ipv4_output(b""), None);
        assert_eq!(parse_ipv4_output(b"not-an-address\n"), None);
    }
}
