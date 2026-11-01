//! Narrowly scoped privileged helper.
//!
//! Only this process runs as root.  It performs exactly three classes of
//! operation: create/destroy a feth pair, create/remove the reserved macOS
//! network service, and open/configure a BPF descriptor for the peer side.  The
//! USB/RNDIS protocol process never runs with elevated privileges.  The BPF fd
//! is handed over with `SCM_RIGHTS`; the helper keeps no copy.

use std::fs;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tether_core::ethernet::{parse_mac, MacAddr};
use tether_macos::bpf::BpfSocket;
use tether_macos::command::{CommandRunner, SystemRunner};
use tether_macos::feth::FethPair;
use tether_macos::ipc::{peer_credentials, FdChannel, PROTOCOL_VERSION};
use tether_macos::service::{NetworkService, PrefsRunner, ScutilRunner, DEFAULT_SERVICE_NAME};

mod install;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn stop_handler(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

#[derive(Parser, Debug)]
#[command(
    name = "android-tether-helper",
    about = "Privileged feth/BPF setup helper for android-tether"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the helper server (requires root).
    Serve {
        /// Unix socket used by the unprivileged CLI.
        #[arg(long, default_value = install::DEFAULT_SOCKET)]
        socket: PathBuf,
        /// UID allowed to connect.  Defaults to SUDO_UID.
        #[arg(long)]
        allow_uid: Option<u32>,
    },
    /// Install the helper as a LaunchDaemon (requires root).
    Install {
        /// UID allowed to connect.  Defaults to SUDO_UID.
        #[arg(long)]
        allow_uid: Option<u32>,
        /// Installed helper binary.
        #[arg(long, default_value = install::DEFAULT_BINARY)]
        binary: PathBuf,
        /// LaunchDaemon property list.
        #[arg(long, default_value = install::DEFAULT_PLIST)]
        plist: PathBuf,
        /// Unix socket used by the unprivileged CLI.
        #[arg(long, default_value = install::DEFAULT_SOCKET)]
        socket: PathBuf,
    },
    /// Remove the LaunchDaemon and installed helper (requires root).
    Uninstall {
        /// Installed helper binary.
        #[arg(long, default_value = install::DEFAULT_BINARY)]
        binary: PathBuf,
        /// LaunchDaemon property list.
        #[arg(long, default_value = install::DEFAULT_PLIST)]
        plist: PathBuf,
        /// Unix socket used by the unprivileged CLI.
        #[arg(long, default_value = install::DEFAULT_SOCKET)]
        socket: PathBuf,
    },
    /// Print environment checks without changing anything.
    Check {
        #[arg(long, default_value = install::DEFAULT_SOCKET)]
        socket: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Serve { socket, allow_uid } => serve(&socket, allow_uid),
        Commands::Install {
            allow_uid,
            binary,
            plist,
            socket,
        } => {
            let allow_uid =
                allow_uid.or_else(|| std::env::var("SUDO_UID").ok().and_then(|v| v.parse().ok()));
            match allow_uid {
                Some(allow_uid) => {
                    let mut options = install::InstallOptions::new(allow_uid);
                    options.binary = binary;
                    options.plist = plist;
                    options.socket = socket;
                    install::install(&options)
                }
                None => {
                    Err("install needs --allow-uid <uid> or SUDO_UID (start with sudo)".to_string())
                }
            }
        }
        Commands::Uninstall {
            binary,
            plist,
            socket,
        } => install::uninstall(&install::UninstallOptions {
            binary,
            plist,
            socket,
        }),
        Commands::Check { socket } => check(&socket),
    };
    if let Err(error) = result {
        eprintln!("android-tether-helper: {error}");
        std::process::exit(1);
    }
}

fn serve(socket_path: &Path, allow_uid: Option<u32>) -> Result<(), String> {
    require_root()?;
    let allow_uid = allow_uid
        .or_else(|| std::env::var("SUDO_UID").ok().and_then(|v| v.parse().ok()))
        .ok_or_else(|| {
            "no allowed UID: pass --allow-uid or start via sudo (SUDO_UID)".to_string()
        })?;
    if allow_uid == 0 {
        eprintln!("android-tether-helper: warning: allowing root as the unprivileged client UID");
    }
    install_signal_handlers()?;
    prepare_socket_path(socket_path)?;
    let listener = UnixListener::bind(socket_path)
        .map_err(|error| format!("cannot bind {}: {error}", socket_path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("cannot make listener non-blocking: {error}"))?;
    restrict_socket(socket_path, allow_uid)?;
    eprintln!(
        "android-tether-helper: listening on {} for uid {}",
        socket_path.display(),
        allow_uid
    );

    let runner = SystemRunner;
    let prefs = ScutilRunner;
    while !STOP.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _address)) => {
                if let Err(error) = handle_client(stream, allow_uid, &runner, &prefs) {
                    eprintln!("android-tether-helper: client error: {error}");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => {
                eprintln!("android-tether-helper: accept failed: {error}");
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
    let _ = fs::remove_file(socket_path);
    Ok(())
}

fn handle_client(
    stream: UnixStream,
    allow_uid: u32,
    runner: &dyn CommandRunner,
    prefs: &dyn PrefsRunner,
) -> Result<(), String> {
    let (uid, _gid) =
        peer_credentials(&stream).map_err(|error| format!("peer credentials: {error}"))?;
    let mut channel = FdChannel::new(stream);
    // Wake up periodically so SIGINT/SIGTERM can tear down an idle session
    // and so a vanished client is noticed without waiting forever.
    let _ = channel.set_read_timeout(Some(Duration::from_secs(1)));
    if uid != allow_uid {
        let _ = channel.send_line("ERR permission peer uid is not allowed");
        return Err(format!("rejected connection from uid {uid}"));
    }
    let Some(hello) = channel
        .recv_line()
        .map_err(|error| format!("read HELLO: {error}"))?
    else {
        return Ok(());
    };
    let Some(version) = hello.strip_prefix("HELLO ") else {
        let _ = channel.send_line("ERR protocol expected HELLO <version>");
        return Ok(());
    };
    if version.trim() != PROTOCOL_VERSION.to_string() {
        let _ = channel.send_line("ERR protocol version mismatch");
        return Ok(());
    }
    channel
        .send_line(&format!("OK {PROTOCOL_VERSION} android-tether-helper"))
        .map_err(|error| format!("write welcome: {error}"))?;

    let mut session: Option<HelperSession> = None;
    loop {
        let line = match channel.recv_line() {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if STOP.load(Ordering::SeqCst) {
                    break;
                }
                continue;
            }
            Err(error) => {
                let _ = channel.send_line(&format!("ERR protocol {error}"));
                break;
            }
        };
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap_or_default();
        let result = match command {
            "PING" => channel.send_line("OK pong").map_err(|e| e.to_string()),
            "CREATE" => handle_create(&mut session, &mut channel, &mut parts, runner, prefs),
            "LINK" => handle_link(&session, &mut channel, &mut parts, runner),
            "DESTROY" => {
                if let Some(resources) = session.take() {
                    resources.cleanup(runner, prefs);
                }
                channel.send_line("OK destroyed").map_err(|e| e.to_string())
            }
            "STATUS" => match &session {
                Some(resources) => channel
                    .send_line(&format!(
                        "OK {} {} {}",
                        resources.pair.host_iface,
                        resources.pair.peer_iface,
                        resources.service.name
                    ))
                    .map_err(|e| e.to_string()),
                None => channel.send_line("OK none").map_err(|e| e.to_string()),
            },
            other => {
                let _ = channel.send_line(&format!("ERR protocol unknown command {other}"));
                continue;
            }
        };
        if let Err(error) = result {
            let _ = channel.send_line(&format!("ERR internal {error}"));
            break;
        }
    }
    if let Some(resources) = session.take() {
        resources.cleanup(runner, prefs);
    }
    Ok(())
}

fn handle_create(
    session: &mut Option<HelperSession>,
    channel: &mut FdChannel,
    parts: &mut std::str::SplitWhitespace<'_>,
    runner: &dyn CommandRunner,
    prefs: &dyn PrefsRunner,
) -> Result<(), String> {
    if session.is_some() {
        channel
            .send_line("ERR busy a session already exists")
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    let mtu: u16 = parts
        .next()
        .ok_or_else(|| "CREATE needs <mtu>".to_string())
        .and_then(|value| value.parse().map_err(|_| "invalid MTU".to_string()))?;
    let mac_text = parts
        .next()
        .ok_or_else(|| "CREATE needs <host-mac>".to_string())?;
    let host_mac: MacAddr = parse_mac(mac_text).map_err(|error| error.message)?;

    let pair = match FethPair::create(runner, host_mac, mtu) {
        Ok(pair) => pair,
        Err(error) => {
            channel
                .send_line(&format!("ERR network {error}"))
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    };
    let service = match NetworkService::create(prefs, DEFAULT_SERVICE_NAME, &pair.host_iface) {
        Ok(service) => service,
        Err(error) => {
            pair.destroy(runner);
            channel
                .send_line(&format!("ERR network {error}"))
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    };
    let bpf = match BpfSocket::open(&pair.peer_iface) {
        Ok(bpf) => bpf,
        Err(error) => {
            let _ = service.remove(prefs);
            pair.destroy(runner);
            channel
                .send_line(&format!(
                    "ERR {} {}",
                    error.category.as_str(),
                    error.message
                ))
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    };
    let fd = bpf.raw_fd();
    let response = format!(
        "OK {} {} {}",
        pair.host_iface, pair.peer_iface, service.name
    );
    let send_result = channel.send_line_with_fd(&response, Some(fd));
    drop(bpf); // the CLI owns the only remaining descriptor
    if let Err(error) = send_result {
        let _ = service.remove(prefs);
        pair.destroy(runner);
        return Err(format!("send CREATE response: {error}"));
    }
    *session = Some(HelperSession { pair, service });
    Ok(())
}

fn handle_link(
    session: &Option<HelperSession>,
    channel: &mut FdChannel,
    parts: &mut std::str::SplitWhitespace<'_>,
    runner: &dyn CommandRunner,
) -> Result<(), String> {
    let state = parts.next().unwrap_or_default();
    let up = match state {
        "up" => true,
        "down" => false,
        _ => {
            channel
                .send_line("ERR protocol LINK expects up or down")
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    };
    let Some(resources) = session else {
        channel
            .send_line("ERR configuration no active session")
            .map_err(|e| e.to_string())?;
        return Ok(());
    };
    match resources.pair.set_link(runner, up) {
        Ok(()) => channel
            .send_line(if up { "OK link-up" } else { "OK link-down" })
            .map_err(|e| e.to_string()),
        Err(error) => channel
            .send_line(&format!("ERR network {error}"))
            .map_err(|e| e.to_string()),
    }
}

struct HelperSession {
    pair: FethPair,
    service: NetworkService,
}

impl HelperSession {
    fn cleanup(self, runner: &dyn CommandRunner, prefs: &dyn PrefsRunner) {
        if let Err(error) = self.service.remove(prefs) {
            eprintln!(
                "android-tether-helper: service cleanup failed for {}: {error}",
                self.service.name
            );
        }
        self.pair.destroy(runner);
    }
}

fn require_root() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("serve requires root (start it with sudo or via launchd)".to_string());
    }
    Ok(())
}

fn install_signal_handlers() -> Result<(), String> {
    let handler = stop_handler as extern "C" fn(libc::c_int);
    for signal in [libc::SIGINT, libc::SIGTERM] {
        let previous = unsafe { libc::signal(signal, handler as libc::sighandler_t) };
        if previous == libc::SIG_ERR {
            return Err(format!(
                "cannot install signal handler: {}",
                io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn prepare_socket_path(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            fs::remove_file(path)
                .map_err(|error| format!("cannot remove stale socket: {error}"))?;
        }
        Ok(_) => {
            return Err(format!(
                "{} exists and is not a socket; refusing to remove it",
                path.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
    }
    Ok(())
}

fn restrict_socket(path: &Path, uid: u32) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "socket path contains a NUL byte".to_string())?;
    let result = unsafe { libc::chown(c_path.as_ptr(), uid, u32::MAX) };
    if result != 0 {
        return Err(format!(
            "cannot chown socket to uid {uid}: {}",
            io::Error::last_os_error()
        ));
    }
    let result = unsafe { libc::chmod(c_path.as_ptr(), 0o600) };
    if result != 0 {
        return Err(format!(
            "cannot chmod socket: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn check(socket_path: &Path) -> Result<(), String> {
    println!("euid={}", unsafe { libc::geteuid() });
    println!("socket={}", socket_path.display());
    println!("ifconfig={}", Path::new("/sbin/ifconfig").exists());
    println!(
        "networksetup={}",
        Path::new("/usr/sbin/networksetup").exists()
    );
    println!("scutil={}", Path::new("/usr/sbin/scutil").exists());
    let bpf_count = (0..256)
        .filter(|index| Path::new(&format!("/dev/bpf{index}")).exists())
        .count();
    println!("bpf_devices={bpf_count}");
    println!("protocol_version={PROTOCOL_VERSION}");
    Ok(())
}
