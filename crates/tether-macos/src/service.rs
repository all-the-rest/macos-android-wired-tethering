//! macOS network service integration through `scutil --prefs`.
//!
//! A `feth` interface is a normal BSD Ethernet interface but it has no IOKit
//! hardware port, so `networksetup -createnetworkservice` cannot attach a
//! service to it.  We write the SystemConfiguration preference schema directly
//! with `scutil --prefs`: a service dictionary with `DeviceName`, `Hardware
//! Ethernet`, `Type Ethernet` and IPv4 `ConfigMethod DHCP`.
//!
//! macOS then runs DHCP, installs routes and resolves DNS itself.  A fixed
//! service UUID makes leftovers recognizable and lets cleanup remove exactly
//! this project's service, never an unrelated user service.  The service is
//! appended to the current location's service order by default, so an existing
//! uplink keeps its precedence.  `connect --prefer` moves it to the front
//! instead; removing the service on disconnect restores the previous order.

use std::io::Write;
use std::process::{Command, Stdio};

pub const SERVICE_ID: &str = "A0D4C0DE-7E7E-4A4A-8D8D-0000000A7070";
pub const DEFAULT_SERVICE_NAME: &str = "Android USB Tethering (tetherd)";
pub const SCUTIL: &str = "/usr/sbin/scutil";

pub trait PrefsRunner {
    fn prefs(&self, script: &str) -> Result<String, String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ScutilRunner;

impl PrefsRunner for ScutilRunner {
    fn prefs(&self, script: &str) -> Result<String, String> {
        let mut child = Command::new(SCUTIL)
            .arg("--prefs")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run scutil: {error}"))?;
        {
            let stdin = child
                .stdin
                .as_mut()
                .ok_or_else(|| "scutil stdin unavailable".to_string())?;
            stdin
                .write_all(format!("{script}quit\n").as_bytes())
                .map_err(|error| format!("cannot write scutil script: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("scutil failed: {error}"))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !output.status.success() {
            return Err(format!(
                "scutil exited with {}: {}",
                output.status,
                text.trim()
            ));
        }
        if text.to_ascii_lowercase().contains("failed") {
            return Err(format!("scutil reported a failure: {}", text.trim()));
        }
        Ok(text)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkService {
    pub name: String,
    pub interface: String,
}

impl NetworkService {
    /// Create the reserved service and append it to the current location's
    /// service order.  A stale service with the same fixed ID is removed first.
    pub fn create(runner: &dyn PrefsRunner, name: &str, interface: &str) -> Result<Self, String> {
        validate_name(name)?;
        validate_interface(interface)?;
        if exists(runner)? {
            remove(runner)?;
        }
        let set = current_set(runner)?;
        let mut order = service_order(runner, &set)?;
        order.retain(|id| id != SERVICE_ID);
        order.push(SERVICE_ID.to_string());
        let script = format!(
            "d.init\n\
             d.add UserDefinedName \"{name}\"\n\
             set /NetworkServices/{SERVICE_ID}\n\
             d.init\n\
             d.add DeviceName {interface}\n\
             d.add Hardware Ethernet\n\
             d.add Type Ethernet\n\
             d.add UserDefinedName \"{name}\"\n\
             set /NetworkServices/{SERVICE_ID}/Interface\n\
             d.init\n\
             d.add ConfigMethod DHCP\n\
             set /NetworkServices/{SERVICE_ID}/IPv4\n\
             set {set}/Network/Service/{SERVICE_ID} /NetworkServices/{SERVICE_ID}\n\
             {order_script}\
             commit\n\
             apply\n",
            order_script = order_script(&set, &order),
        );
        let mut last_error = None;
        for _ in 0..3 {
            match runner.prefs(&script) {
                Ok(_) if exists(runner)? => {
                    return Ok(Self {
                        name: name.to_string(),
                        interface: interface.to_string(),
                    })
                }
                Ok(_) => last_error = Some("service not present after commit".to_string()),
                Err(error) => last_error = Some(error),
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        Err(format!(
            "could not create network service: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        ))
    }

    pub fn remove(&self, runner: &dyn PrefsRunner) -> Result<(), String> {
        remove(runner)
    }

    pub fn exists(&self, runner: &dyn PrefsRunner) -> Result<bool, String> {
        exists(runner)
    }

    /// Move the reserved service to the front of the current location's
    /// service order.  Used by `connect --prefer` when the tether should be
    /// the primary uplink.  The service is removed on disconnect anyway, so
    /// the original order returns without a separate restore step.
    pub fn prefer(&self, runner: &dyn PrefsRunner) -> Result<(), String> {
        prefer(runner)
    }

    /// Place the reserved service directly behind the current default-route
    /// service, or first when there is no default route.  This is the normal
    /// "priority 2" behavior: an existing internet uplink stays preferred,
    /// while the tether takes over when no other default route exists.
    pub fn fallback(
        &self,
        runner: &dyn PrefsRunner,
        default_interface: Option<&str>,
    ) -> Result<(), String> {
        fallback(runner, default_interface)
    }
}

fn prefer(runner: &dyn PrefsRunner) -> Result<(), String> {
    if !exists(runner)? {
        return Err("reserved network service is not present".to_string());
    }
    let set = current_set(runner)?;
    let mut order = service_order(runner, &set)?;
    order.retain(|id| id != SERVICE_ID);
    order.insert(0, SERVICE_ID.to_string());
    write_order(runner, &set, &order, "move network service to the front")
}

fn fallback(runner: &dyn PrefsRunner, default_interface: Option<&str>) -> Result<(), String> {
    if !exists(runner)? {
        return Err("reserved network service is not present".to_string());
    }
    let set = current_set(runner)?;
    let order = service_order(runner, &set)?;
    let mut place_last = false;
    let anchor = match default_interface {
        None => None,
        Some(interface) => match service_id_for_interface(runner, &order, interface)? {
            Some(id) => Some(id),
            None => {
                // The default route may belong to a VPN/utun that has no
                // matching network service.  The tether must still not stay
                // ahead of it, so place it at the end.
                place_last = true;
                None
            }
        },
    };
    let mut updated = order;
    updated.retain(|id| id != SERVICE_ID);
    let insert_at = if place_last {
        updated.len()
    } else {
        match anchor {
            // Find the anchor again after removing our own entry: the old index
            // shifts when the tether was already ahead of the default service.
            Some(id) => updated
                .iter()
                .position(|candidate| candidate == &id)
                .map(|index| index + 1)
                .unwrap_or(updated.len()),
            None => 0,
        }
    };
    updated.insert(insert_at, SERVICE_ID.to_string());
    write_order(
        runner,
        &set,
        &updated,
        "place network service behind the default uplink",
    )
}

fn service_id_for_interface(
    runner: &dyn PrefsRunner,
    order: &[String],
    interface: &str,
) -> Result<Option<String>, String> {
    for id in order {
        if id == SERVICE_ID {
            continue;
        }
        if service_device(runner, id)?.as_deref() == Some(interface) {
            return Ok(Some(id.clone()));
        }
    }
    Ok(None)
}

fn service_device(runner: &dyn PrefsRunner, id: &str) -> Result<Option<String>, String> {
    let text = runner.prefs(&format!("get /NetworkServices/{id}/Interface\nd.show\n"))?;
    Ok(text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("DeviceName : ")
            .map(|value| value.trim().to_string())
    }))
}

fn write_order(
    runner: &dyn PrefsRunner,
    set: &str,
    order: &[String],
    description: &str,
) -> Result<(), String> {
    let script = format!("{}commit\napply\n", order_script(set, order));
    let mut last_error = None;
    for _ in 0..3 {
        match runner.prefs(&script) {
            Ok(_) => {
                let updated = service_order(runner, set).unwrap_or_default();
                if updated == order {
                    return Ok(());
                }
                last_error = Some("service order did not change".to_string());
            }
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Err(format!(
        "could not {description}: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
}

fn exists(runner: &dyn PrefsRunner) -> Result<bool, String> {
    let text = runner.prefs("list /NetworkServices\n")?;
    Ok(text.lines().any(|line| line.contains(SERVICE_ID)))
}

fn current_set(runner: &dyn PrefsRunner) -> Result<String, String> {
    let text = runner.prefs("get /\nd.show\n")?;
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("CurrentSet : ")
                .map(|value| value.trim().to_string())
        })
        .ok_or_else(|| "scutil reported no current network location".to_string())
}

fn sets(runner: &dyn PrefsRunner) -> Result<Vec<String>, String> {
    let text = runner.prefs("list /Sets\n")?;
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some((_, path)) = line.split_once("= ") {
            let path = path.trim();
            if path.matches('/').count() == 2 && !out.contains(&path.to_string()) {
                out.push(path.to_string());
            }
        }
    }
    Ok(out)
}

fn service_order(runner: &dyn PrefsRunner, set: &str) -> Result<Vec<String>, String> {
    let text = runner
        .prefs(&format!("get {set}/Network/Global/IPv4\nd.show\n"))
        .map_err(|error| format!("cannot read network service order: {error}"))?;
    let mut in_order = false;
    let mut closed = false;
    let mut order = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("ServiceOrder") {
            in_order = true;
        } else if in_order && line == "}" {
            closed = true;
            break;
        } else if in_order && !line.is_empty() {
            let Some((_, id)) = line.split_once(" : ") else {
                return Err(format!("malformed service order entry: {line}"));
            };
            let id = id.trim();
            if id.is_empty() {
                return Err("malformed service order entry: empty service ID".to_string());
            }
            order.push(id.to_string());
        }
    }
    if !in_order {
        return Err("scutil response has no ServiceOrder".to_string());
    }
    if !closed {
        return Err("scutil returned a truncated service order".to_string());
    }
    Ok(order)
}

fn order_script(set: &str, order: &[String]) -> String {
    format!(
        "d.init\n\
         get {set}/Network/Global/IPv4\n\
         d.add ServiceOrder * {}\n\
         set {set}/Network/Global/IPv4\n",
        order.join(" ")
    )
}

fn remove(runner: &dyn PrefsRunner) -> Result<(), String> {
    if !exists(runner)? {
        return Ok(());
    }
    let nsets = sets(runner)?;
    let mut last_error = None;
    for _ in 0..3 {
        let mut script = String::new();
        for set in &nsets {
            let order = service_order(runner, set).unwrap_or_default();
            let had_our_id = order.iter().any(|id| id == SERVICE_ID);
            let filtered: Vec<String> = order.into_iter().filter(|id| id != SERVICE_ID).collect();
            if had_our_id {
                script.push_str(&order_script(set, &filtered));
            }
            script.push_str(&format!("remove {set}/Network/Service/{SERVICE_ID}\n"));
        }
        script.push_str(&format!(
            "remove /NetworkServices/{SERVICE_ID}\ncommit\napply\n"
        ));
        // "remove" of a missing key is harmless and may be printed by scutil.
        if let Err(error) = runner.prefs(&script) {
            last_error = Some(error);
        }
        if !exists(runner)? {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Err(format!(
        "network service could not be removed: {}",
        last_error.unwrap_or_else(|| "still present".to_string())
    ))
}

/// Remove only the service identified by the fixed project UUID.  Used by
/// uninstall when no CLI state file is available.
pub fn remove_reserved(runner: &dyn PrefsRunner) -> Result<(), String> {
    remove(runner)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 128 {
        return Err("network service name is empty or too long".to_string());
    }
    if name
        .chars()
        .any(|character| character.is_control() || character == '"' || character == '\\')
    {
        return Err("network service name contains unsafe characters".to_string());
    }
    Ok(())
}

fn validate_interface(interface: &str) -> Result<(), String> {
    let suffix = interface
        .strip_prefix("feth")
        .ok_or_else(|| "network service interface must be a feth interface".to_string())?;
    if suffix.is_empty() || !suffix.chars().all(|character| character.is_ascii_digit()) {
        return Err("invalid feth interface name".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakePrefs {
        scripts: RefCell<Vec<String>>,
        present: RefCell<bool>,
        preferred: RefCell<bool>,
        fail_order: RefCell<bool>,
        omit_order: RefCell<bool>,
        malformed_order: RefCell<bool>,
    }

    impl PrefsRunner for FakePrefs {
        fn prefs(&self, script: &str) -> Result<String, String> {
            self.scripts.borrow_mut().push(script.to_string());
            if *self.fail_order.borrow() && script.contains("get /Sets/ABCD/Network/Global/IPv4") {
                return Err("simulated read failure".to_string());
            }
            let mut present = self.present.borrow_mut();
            let mut preferred = self.preferred.borrow_mut();
            if script.contains(&format!("set /NetworkServices/{SERVICE_ID}"))
                && script.contains("commit")
            {
                *present = true;
                *preferred = false;
            }
            if script.contains(&format!("d.add ServiceOrder * {SERVICE_ID} OTHER-SERVICE")) {
                *preferred = true;
            }
            if script.contains(&format!("d.add ServiceOrder * OTHER-SERVICE {SERVICE_ID}")) {
                *preferred = false;
            }
            if script.contains(&format!("remove /NetworkServices/{SERVICE_ID}")) {
                *present = false;
                *preferred = false;
            }
            let mut output = String::new();
            if script.contains("get /") && script.contains("d.show") {
                output.push_str("CurrentSet : /Sets/ABCD\n");
            }
            if script.contains("get /Sets/ABCD/Network/Global/IPv4") {
                if *self.omit_order.borrow() {
                    return Ok(String::new());
                }
                if *self.malformed_order.borrow() {
                    return Ok("ServiceOrder : <array> {\n  not-an-entry\n}\n".to_string());
                }
                if *present && *preferred {
                    output.push_str(&format!(
                        "ServiceOrder : <array> {{\n  0 : {SERVICE_ID}\n  1 : OTHER-SERVICE\n}}\n"
                    ));
                } else if *present {
                    output.push_str(&format!(
                        "ServiceOrder : <array> {{\n  0 : OTHER-SERVICE\n  1 : {SERVICE_ID}\n}}\n"
                    ));
                } else {
                    output.push_str("ServiceOrder : <array> {\n  0 : OTHER-SERVICE\n}\n");
                }
            }
            if script.contains("get /NetworkServices/OTHER-SERVICE/Interface") {
                output.push_str("DeviceName : en0\n");
            }
            if script.contains("list /NetworkServices") && *present {
                output.push_str(&format!("subKey = {SERVICE_ID}\n"));
            }
            if script.contains("list /Sets") {
                output.push_str("subKey = /Sets/ABCD\n");
            }
            Ok(output)
        }
    }

    #[test]
    fn create_writes_dhcp_service_and_appends_order() {
        let runner = FakePrefs::default();
        NetworkService::create(&runner, DEFAULT_SERVICE_NAME, "feth100").unwrap();
        let scripts = runner.scripts.borrow();
        let combined = scripts.join("\n---\n");
        assert!(combined.contains(&format!("set /NetworkServices/{SERVICE_ID}")));
        assert!(combined.contains("d.add DeviceName feth100"));
        assert!(combined.contains("d.add Hardware Ethernet"));
        assert!(combined.contains("d.add Type Ethernet"));
        assert!(combined.contains("d.add ConfigMethod DHCP"));
        assert!(combined.contains(&format!("d.add ServiceOrder * OTHER-SERVICE {SERVICE_ID}")));
        assert!(combined.contains("commit"));
        assert!(combined.contains("apply"));
    }

    #[test]
    fn prefer_moves_service_to_front_without_dropping_others() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        service.prefer(&runner).unwrap();
        assert!(*runner.preferred.borrow());
        let combined = runner.scripts.borrow().join("\n---\n");
        assert!(combined.contains(&format!("d.add ServiceOrder * {SERVICE_ID} OTHER-SERVICE")));
        assert!(!combined.contains("d.add ServiceOrder * OTHER-SERVICE\n"));
    }

    #[test]
    fn prefer_requires_the_reserved_service() {
        let runner = FakePrefs::default();
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        assert!(service.prefer(&runner).is_err());
    }

    #[test]
    fn fallback_places_service_after_the_default_interface() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        service.fallback(&runner, Some("en0")).unwrap();
        let combined = runner.scripts.borrow().join("\n---\n");
        assert!(combined.contains(&format!("d.add ServiceOrder * OTHER-SERVICE {SERVICE_ID}")));
    }

    #[test]
    fn fallback_moves_tether_directly_behind_default_when_tether_was_first() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        *runner.preferred.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        service.fallback(&runner, Some("en0")).unwrap();
        assert!(!*runner.preferred.borrow());
        let combined = runner.scripts.borrow().join("\n---\n");
        assert!(combined.contains(&format!("d.add ServiceOrder * OTHER-SERVICE {SERVICE_ID}")));
    }

    #[test]
    fn fallback_promotes_service_when_no_default_route_exists() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        service.fallback(&runner, None).unwrap();
        let combined = runner.scripts.borrow().join("\n---\n");
        assert!(combined.contains(&format!("d.add ServiceOrder * {SERVICE_ID} OTHER-SERVICE")));
    }

    #[test]
    fn fallback_moves_tether_to_the_end_for_an_unknown_default_interface() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        *runner.preferred.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        service.fallback(&runner, Some("utun9")).unwrap();
        assert!(!*runner.preferred.borrow());
        let combined = runner.scripts.borrow().join("\n---\n");
        assert!(combined.contains(&format!("d.add ServiceOrder * OTHER-SERVICE {SERVICE_ID}")));
    }

    #[test]
    fn prefer_does_not_write_when_order_read_fails() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        *runner.fail_order.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        assert!(service.prefer(&runner).is_err());
        assert!(!runner
            .scripts
            .borrow()
            .iter()
            .any(|script| script.contains("d.add ServiceOrder")));
    }

    #[test]
    fn prefer_rejects_missing_service_order_without_writing() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        *runner.omit_order.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        assert!(service.prefer(&runner).is_err());
        assert!(!runner
            .scripts
            .borrow()
            .iter()
            .any(|script| script.contains("d.add ServiceOrder")));
    }

    #[test]
    fn prefer_rejects_malformed_service_order_without_writing() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        *runner.malformed_order.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        assert!(service.prefer(&runner).is_err());
        assert!(!runner
            .scripts
            .borrow()
            .iter()
            .any(|script| script.contains("d.add ServiceOrder")));
    }

    #[test]
    fn remove_deletes_service_and_keeps_other_order_entries() {
        let runner = FakePrefs::default();
        *runner.present.borrow_mut() = true;
        let service = NetworkService {
            name: DEFAULT_SERVICE_NAME.into(),
            interface: "feth100".into(),
        };
        service.remove(&runner).unwrap();
        let combined = runner.scripts.borrow().join("\n---\n");
        assert!(combined.contains(&format!("remove /NetworkServices/{SERVICE_ID}")));
        assert!(combined.contains("d.add ServiceOrder * OTHER-SERVICE\n"));
        assert!(!*runner.present.borrow());
    }

    #[test]
    fn validates_names_and_interfaces() {
        let runner = FakePrefs::default();
        assert!(NetworkService::create(&runner, "", "feth100").is_err());
        assert!(NetworkService::create(&runner, "bad\"name", "feth100").is_err());
        assert!(NetworkService::create(&runner, DEFAULT_SERVICE_NAME, "en0").is_err());
        assert!(NetworkService::create(&runner, DEFAULT_SERVICE_NAME, "fethabc").is_err());
    }

    #[test]
    fn parses_service_order_array() {
        let runner = FakePrefs::default();
        let order = service_order(&runner, "/Sets/ABCD").unwrap();
        assert_eq!(order, vec!["OTHER-SERVICE"]);
    }
}
