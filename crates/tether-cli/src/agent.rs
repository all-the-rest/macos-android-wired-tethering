//! Per-user LaunchAgent that keeps the tether connected while the phone is
//! plugged in.  It runs the unprivileged CLI; the root helper remains the only
//! component that creates feth/BPF and changes network preferences.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const LABEL: &str = "all-the-rest.android-tether.auto";
const PLIST_NAME: &str = "all-the-rest.android-tether.auto.plist";
const LEGACY_LABEL: &str = "io.github.android-tether.auto";
const LEGACY_PLIST_NAME: &str = "io.github.android-tether.auto.plist";
const LAUNCHCTL: &str = "/bin/launchctl";

#[derive(Debug, Clone)]
pub struct AgentPaths {
    pub binary: PathBuf,
    pub plist: PathBuf,
    pub log: PathBuf,
}

impl AgentPaths {
    pub fn new() -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME is not set; cannot install the user agent".to_string())?;
        Ok(Self {
            binary: home.join("Library/Application Support/android-tether/bin/android-tether"),
            plist: home.join(format!("Library/LaunchAgents/{PLIST_NAME}")),
            log: home.join("Library/Logs/android-tether-agent.log"),
        })
    }
}

/// Build the LaunchAgent property list.  Kept pure so the exact program and
/// log paths can be tested without loading anything into launchd.
pub fn launch_agent_plist(binary: &Path, helper: &Path, log: &Path) -> String {
    let binary = xml_escape(&binary.to_string_lossy());
    let helper = xml_escape(&helper.to_string_lossy());
    let log = xml_escape(&log.to_string_lossy());
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
    <string>--helper</string>
    <string>{helper}</string>
    <string>auto</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#
    )
}

pub fn install(helper: &Path) -> Result<(), String> {
    let paths = AgentPaths::new()?;
    if !helper.exists() {
        return Err(format!(
            "helper socket {} does not exist; install/start the helper first",
            helper.display()
        ));
    }
    let source = std::env::current_exe()
        .map_err(|error| format!("cannot locate the running CLI: {error}"))?
        .canonicalize()
        .map_err(|error| format!("cannot resolve the running CLI: {error}"))?;
    if let Some(parent) = paths.binary.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }

    // Stage and sign a copy next to the destination.  The running agent keeps
    // its old inode until the final rename, so it never executes a partially
    // written or unsigned file, and launchd cannot hit OS_REASON_CODESIGNING.
    let staged = paths.binary.with_extension("staged");
    let staged_result = (|| -> Result<(), String> {
        fs::copy(&source, &staged).map_err(|error| {
            format!(
                "cannot copy {} to {}: {error}",
                source.display(),
                staged.display()
            )
        })?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o755))
            .map_err(|error| format!("cannot chmod {}: {error}", staged.display()))?;
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-", "--timestamp=none"])
            .arg(&staged)
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !signed {
            return Err(format!(
                "cannot ad-hoc sign the staged CLI {}",
                staged.display()
            ));
        }
        Ok(())
    })();
    if let Err(error) = staged_result {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }

    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let legacy_plist = paths.plist.with_file_name(LEGACY_PLIST_NAME);

    // Stop the old jobs before publishing anything.  A failed migration must
    // not leave the new and the legacy plist side by side, and the running
    // agent must never see a replaced executable.
    let published = (|| -> Result<(), String> {
        if let Some(parent) = paths.plist.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        stop_agent_label(&format!("{domain}/{LEGACY_LABEL}"))?;
        remove_if_present(&legacy_plist)?;
        stop_agent_label(&format!("{domain}/{LABEL}"))?;
        fs::rename(&staged, &paths.binary).map_err(|error| {
            format!(
                "cannot replace {} with {}: {error}",
                paths.binary.display(),
                staged.display()
            )
        })?;
        write_atomic(
            &paths.plist,
            launch_agent_plist(&paths.binary, helper, &paths.log).as_bytes(),
        )?;
        fs::set_permissions(&paths.plist, fs::Permissions::from_mode(0o644))
            .map_err(|error| format!("cannot chmod {}: {error}", paths.plist.display()))?;
        if !bootstrap_with_retry(&domain, &paths.plist) {
            let loaded = Command::new(LAUNCHCTL)
                .args(["load", "-w"])
                .arg(&paths.plist)
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if !loaded {
                return Err(format!(
                    "launchctl could not load {}; check {}",
                    paths.plist.display(),
                    paths.log.display()
                ));
            }
        }
        Ok(())
    })();
    if let Err(error) = published {
        let _ = fs::remove_file(&staged);
        return Err(error);
    }
    println!("installed {LABEL}");
    println!("  binary: {}", paths.binary.display());
    println!("  plist:  {}", paths.plist.display());
    println!("  log:    {}", paths.log.display());
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let paths = AgentPaths::new()?;
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    stop_agent_label(&format!("{domain}/{LABEL}"))?;
    stop_agent_label(&format!("{domain}/{LEGACY_LABEL}"))?;
    remove_if_present(&paths.plist.with_file_name(LEGACY_PLIST_NAME))?;
    remove_if_present(&paths.plist)?;
    remove_if_present(&paths.binary)?;
    remove_if_present(&paths.log)?;
    println!("uninstalled {LABEL}");
    Ok(())
}

fn stop_agent_label(target: &str) -> Result<(), String> {
    if matches!(agent_state(target), AgentState::NotLoaded) {
        return Ok(());
    }
    if let AgentState::Unknown(error) = agent_state(target) {
        return Err(format!(
            "cannot determine whether {target} is loaded; refusing to continue: {error}"
        ));
    }
    let bootout = Command::new(LAUNCHCTL).args(["bootout", target]).output();
    for _ in 0..5 {
        if matches!(agent_state(target), AgentState::NotLoaded) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let detail = bootout
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stderr).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "launchctl did not stop the agent".to_string());
    match agent_state(target) {
        AgentState::Loaded => Err(format!("{target} is still loaded: {detail}")),
        AgentState::Unknown(error) => Err(format!("cannot verify that {target} stopped: {error}")),
        AgentState::NotLoaded => Ok(()),
    }
}

enum AgentState {
    Loaded,
    NotLoaded,
    Unknown(String),
}

fn agent_state(target: &str) -> AgentState {
    match Command::new(LAUNCHCTL).args(["print", target]).output() {
        Ok(output) if output.status.success() => AgentState::Loaded,
        Ok(output) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if text.contains("Could not find service") || text.contains("No such process") {
                AgentState::NotLoaded
            } else {
                AgentState::Unknown(text.trim().to_string())
            }
        }
        Err(error) => AgentState::Unknown(error.to_string()),
    }
}

fn bootstrap_with_retry(domain: &str, plist: &Path) -> bool {
    for attempt in 0..5 {
        let loaded = Command::new(LAUNCHCTL)
            .args(["bootstrap", domain])
            .arg(plist)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if loaded {
            return true;
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    false
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
    fn plist_contains_the_auto_program_and_paths() {
        let plist = launch_agent_plist(
            Path::new("/Users/me/Library/Application Support/android-tether/bin/android-tether"),
            Path::new("/var/run/android-tether-helper.sock"),
            Path::new("/Users/me/Library/Logs/android-tether-agent.log"),
        );
        assert!(plist.contains("<string>all-the-rest.android-tether.auto</string>"));
        assert!(plist.contains("<string>--helper</string>"));
        assert!(plist.contains("<string>/var/run/android-tether-helper.sock</string>"));
        assert!(plist.contains("<string>auto</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("/Users/me/Library/Logs/android-tether-agent.log"));
    }

    #[test]
    fn plist_escapes_xml_metacharacters() {
        let plist = launch_agent_plist(
            Path::new("/tmp/a&b"),
            Path::new("/tmp/c<d"),
            Path::new("/tmp/e>f"),
        );
        assert!(plist.contains("/tmp/a&amp;b"));
        assert!(plist.contains("/tmp/c&lt;d"));
        assert!(plist.contains("/tmp/e&gt;f"));
        assert!(!plist.contains("/tmp/a&b"));
    }

    #[test]
    fn generated_plist_is_valid_property_list() {
        let path = std::env::temp_dir().join(format!(
            "android-tether-agent-{}-{}.plist",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let plist = launch_agent_plist(
            Path::new("/tmp/android-tether"),
            Path::new("/var/run/android-tether-helper.sock"),
            Path::new("/tmp/android-tether-agent.log"),
        );
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
