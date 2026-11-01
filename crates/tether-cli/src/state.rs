//! Local runtime state for `status`/`disconnect`/`cleanup`.
//!
//! The file contains operational metadata only: no USB serial number, no
//! packet payloads and no credentials.  It is written atomically.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tether_core::error::{ErrorCategory, Result, TetherError};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeState {
    pub pid: u32,
    pub started_unix: u64,
    pub updated_unix: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_state_json() {
        let state = RuntimeState {
            pid: 42,
            started_unix: 1,
            updated_unix: 2,
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
            frames_phone_to_host: 0,
            frames_host_to_phone: 0,
            bytes_phone_to_host: 0,
            bytes_host_to_phone: 0,
            reconnects: 0,
            last_error: None,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("serial"));
        let parsed: RuntimeState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.host_iface, "feth100");
        assert_eq!(parsed.mtu, 1500);
    }

    #[test]
    fn dead_pid_is_not_alive() {
        assert!(!process_alive(0));
    }
}
