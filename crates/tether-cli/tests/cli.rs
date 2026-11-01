use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_android-tether")
}

#[test]
fn simulate_produces_machine_readable_bidirectional_evidence() {
    let output = Command::new(binary())
        .args(["simulate", "--scenario", "dhcp", "--json"])
        .output()
        .expect("run android-tether");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(value["protocol"], "RNDIS");
    assert_eq!(value["frames_host_to_phone"], 1);
    assert_eq!(value["frames_phone_to_host"], 1);
    assert_eq!(value["delivered_to_host"], 1);
    assert_eq!(value["warnings"].as_array().unwrap().len(), 0);
}

#[test]
fn list_json_does_not_require_root_and_is_valid_json() {
    let output = Command::new(binary())
        .args(["list", "--all", "--json"])
        .output()
        .expect("run android-tether");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert!(value["devices"].is_array());
}

#[test]
fn help_lists_the_supported_commands() {
    let output = Command::new(binary())
        .arg("--help")
        .output()
        .expect("run android-tether");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    for command in [
        "list",
        "status",
        "diagnose",
        "connect",
        "disconnect",
        "cleanup",
        "simulate",
        "net-smoke",
    ] {
        assert!(text.contains(command), "missing command {command}");
    }
}
