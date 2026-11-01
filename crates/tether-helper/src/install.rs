//! Install and remove the privileged helper as a macOS LaunchDaemon.
//!
//! Installation copies the current helper binary to
//! `/Library/PrivilegedHelperTools`, writes a plist below
//! `/Library/LaunchDaemons`, and asks `launchctl` to start it at boot.  The
//! daemon still exposes only the narrow socket protocol used by the CLI.
//!
//! Uninstallation stops the daemon and removes the plist, binary, socket and
//! the reserved network service.  A live feth pair should be removed with
//! `android-tether cleanup` before uninstalling; the helper cannot safely guess
//! which feth interfaces belong to other software.

use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tether_macos::service::ScutilRunner;

pub const LABEL: &str = "io.github.android-tether.helper";
pub const DEFAULT_BINARY: &str = "/Library/PrivilegedHelperTools/android-tether-helper";
pub const DEFAULT_PLIST: &str = "/Library/LaunchDaemons/io.github.android-tether.helper.plist";
pub const DEFAULT_SOCKET: &str = "/var/run/android-tether-helper.sock";
pub const LOG_PATH: &str = "/var/log/android-tether-helper.log";
const LAUNCHCTL: &str = "/bin/launchctl";

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub allow_uid: u32,
    pub binary: PathBuf,
    pub plist: PathBuf,
    pub socket: PathBuf,
}

impl InstallOptions {
    pub fn new(allow_uid: u32) -> Self {
        Self {
            allow_uid,
            binary: PathBuf::from(DEFAULT_BINARY),
            plist: PathBuf::from(DEFAULT_PLIST),
            socket: PathBuf::from(DEFAULT_SOCKET),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UninstallOptions {
    pub binary: PathBuf,
    pub plist: PathBuf,
    pub socket: PathBuf,
}

impl Default for UninstallOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from(DEFAULT_BINARY),
            plist: PathBuf::from(DEFAULT_PLIST),
            socket: PathBuf::from(DEFAULT_SOCKET),
        }
    }
}

/// Build the LaunchDaemon property list.  Kept pure and public so the exact
/// arguments can be inspected and tested without root.
pub fn launchd_plist(binary: &Path, socket: &Path, allow_uid: u32) -> String {
    let binary = xml_escape(&binary.to_string_lossy());
    let socket = xml_escape(&socket.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>serve</string>
    <string>--socket</string>
    <string>{socket}</string>
    <string>--allow-uid</string>
    <string>{allow_uid}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>StandardOutPath</key>
  <string>{LOG_PATH}</string>
  <key>StandardErrorPath</key>
  <string>{LOG_PATH}</string>
</dict>
</plist>
"#
    )
}

pub fn install(options: &InstallOptions) -> Result<(), String> {
    require_root()?;
    if options.allow_uid == 0 {
        return Err("refusing to allow uid 0 as the unprivileged client".to_string());
    }
    validate_absolute(&options.binary, "binary")?;
    validate_absolute(&options.plist, "plist")?;
    validate_absolute(&options.socket, "socket")?;

    let source = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running helper binary: {error}"))?;
    let source = source
        .canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", source.display()))?;
    if let Some(parent) = options.binary.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let destination = options
        .binary
        .canonicalize()
        .unwrap_or_else(|_| options.binary.clone());
    if source != destination {
        fs::copy(&source, &options.binary).map_err(|error| {
            format!(
                "cannot install {} to {}: {error}",
                source.display(),
                options.binary.display()
            )
        })?;
    }
    set_owner_and_mode(&options.binary, 0o755)?;

    let plist = launchd_plist(&options.binary, &options.socket, options.allow_uid);
    write_atomic(&options.plist, plist.as_bytes())?;
    set_owner_and_mode(&options.plist, 0o644)?;

    // Remove a previous instance so a changed --allow-uid takes effect.
    let _ = Command::new(LAUNCHCTL)
        .args(["bootout", &format!("system/{LABEL}")])
        .status();
    let bootstrapped = Command::new(LAUNCHCTL)
        .args(["bootstrap", "system"])
        .arg(&options.plist)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !bootstrapped {
        let loaded = Command::new(LAUNCHCTL)
            .args(["load", "-w"])
            .arg(&options.plist)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !loaded {
            return Err(format!(
                "launchctl could not load {}; inspect the plist and /var/log/android-tether-helper.log",
                options.plist.display()
            ));
        }
    }

    println!("installed {LABEL}");
    println!("  binary: {}", options.binary.display());
    println!("  plist:  {}", options.plist.display());
    println!("  socket: {}", options.socket.display());
    println!("  log:    {LOG_PATH}");
    Ok(())
}

pub fn uninstall(options: &UninstallOptions) -> Result<(), String> {
    require_root()?;
    let _ = Command::new(LAUNCHCTL)
        .args(["bootout", &format!("system/{LABEL}")])
        .status();
    let _ = Command::new(LAUNCHCTL)
        .args(["unload", "-w"])
        .arg(&options.plist)
        .status();

    remove_if_present(&options.plist)?;
    remove_if_present(&options.binary)?;
    remove_if_present(&options.socket)?;

    // Remove the reserved network service even when the CLI state file was
    // lost.  This never touches services that do not carry our fixed UUID.
    let runner = ScutilRunner;
    if let Err(error) = tether_macos::service::remove_reserved(&runner) {
        eprintln!("warning: could not remove the reserved network service: {error}");
    }

    println!("uninstalled {LABEL}");
    println!("Run 'android-tether cleanup' first if a feth pair is still present.");
    Ok(())
}

fn require_root() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("install/uninstall requires root (run with sudo)".to_string());
    }
    Ok(())
}

fn validate_absolute(path: &Path, name: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{name} path must be absolute: {}", path.display()));
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&temporary, bytes)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot commit {}: {error}", path.display()))?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

fn set_owner_and_mode(path: &Path, mode: u32) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot chmod {}: {error}", path.display()))?;
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("path contains a NUL byte: {}", path.display()))?;
    let result = unsafe { libc::chown(c_path.as_ptr(), 0, 0) };
    if result != 0 {
        return Err(format!(
            "cannot chown {} to root:wheel: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plist_contains_the_fixed_label_and_arguments() {
        let plist = launchd_plist(
            Path::new("/Library/PrivilegedHelperTools/android-tether-helper"),
            Path::new("/var/run/android-tether-helper.sock"),
            501,
        );
        assert!(plist.contains("<string>io.github.android-tether.helper</string>"));
        assert!(
            plist.contains("<string>/Library/PrivilegedHelperTools/android-tether-helper</string>")
        );
        assert!(plist.contains("<string>--allow-uid</string>"));
        assert!(plist.contains("<string>501</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<string>/var/log/android-tether-helper.log</string>"));
    }

    #[test]
    fn plist_escapes_xml_metacharacters() {
        let plist = launchd_plist(Path::new("/tmp/a&b"), Path::new("/tmp/c<d"), 7);
        assert!(plist.contains("/tmp/a&amp;b"));
        assert!(plist.contains("/tmp/c&lt;d"));
        assert!(!plist.contains("/tmp/a&b"));
    }

    #[test]
    fn install_options_use_safe_defaults() {
        let options = InstallOptions::new(501);
        assert_eq!(options.binary, Path::new(DEFAULT_BINARY));
        assert_eq!(options.plist, Path::new(DEFAULT_PLIST));
        assert_eq!(options.socket, Path::new(DEFAULT_SOCKET));
    }

    #[test]
    fn generated_plist_is_valid_property_list() {
        let path = std::env::temp_dir().join(format!(
            "android-tether-plist-{}-{}.plist",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let plist = launchd_plist(Path::new(DEFAULT_BINARY), Path::new(DEFAULT_SOCKET), 501);
        fs::write(&path, plist).expect("write plist");
        let status = Command::new("/usr/bin/plutil")
            .args(["-lint"])
            .arg(&path)
            .status()
            .expect("run plutil");
        let _ = fs::remove_file(&path);
        assert!(status.success(), "plutil rejected the generated plist");
    }
}
