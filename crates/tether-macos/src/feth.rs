//! macOS `feth` pair lifecycle.
//!
//! macOS ships a pair of fake-Ethernet interfaces (`fethN`).  Packets injected
//! on the peer side are visible on the host side and vice versa; the host side
//! can be backed by a normal macOS network service, so DHCP, DNS and routing
//! are handled by the operating system rather than reimplemented here.
//!
//! Creating/destroying interfaces requires root.  This module is therefore
//! only called by the narrowly scoped helper.  `feth` is undocumented but has
//! been present since macOS 10.13; the implementation also works without SIP
//! changes or kernel extensions.

use crate::command::{CommandRunner, IFCONFIG};
use tether_core::ethernet::MacAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FethPair {
    pub host_iface: String,
    pub peer_iface: String,
    pub mtu: u16,
}

impl FethPair {
    /// Create a pair.  The host interface is left administratively down; the
    /// caller brings it up after the protocol handshake succeeds.
    pub fn create(runner: &dyn CommandRunner, host_mac: MacAddr, mtu: u16) -> Result<Self, String> {
        if !(576..=1500).contains(&mtu) {
            return Err(format!(
                "MTU {mtu} is outside the supported range 576..1500"
            ));
        }
        let existing = runner.run(IFCONFIG, &["-l"]).unwrap_or_default();
        let existing: Vec<&str> = existing.split_whitespace().collect();
        let mut chosen = None;
        // Deterministic scan; feth names are cheap and stale pairs are rare.
        for index in 100u32..900 {
            let host = format!("feth{index}");
            let peer = format!("feth{}", index + 1000);
            if !existing.contains(&host.as_str()) && !existing.contains(&peer.as_str()) {
                chosen = Some((host, peer));
                break;
            }
        }
        let (host_iface, peer_iface) =
            chosen.ok_or_else(|| "no free feth interface pair found".to_string())?;
        let pair = Self {
            host_iface,
            peer_iface,
            mtu,
        };

        let result = (|| {
            runner.run(IFCONFIG, &[&pair.peer_iface, "create"])?;
            runner.run(IFCONFIG, &[&pair.host_iface, "create"])?;
            runner.run(
                IFCONFIG,
                &[&pair.host_iface, "lladdr", &host_mac.to_string()],
            )?;
            runner.run(IFCONFIG, &[&pair.peer_iface, "peer", &pair.host_iface])?;
            runner.run(
                IFCONFIG,
                &[&pair.peer_iface, "mtu", &pair.mtu.to_string(), "up"],
            )?;
            runner.run(
                IFCONFIG,
                &[&pair.host_iface, "mtu", &pair.mtu.to_string(), "down"],
            )?;
            Ok(())
        })();

        if let Err(error) = result {
            pair.destroy(runner);
            return Err(error);
        }
        Ok(pair)
    }

    pub fn set_link(&self, runner: &dyn CommandRunner, up: bool) -> Result<(), String> {
        runner.run(
            IFCONFIG,
            &[&self.host_iface, if up { "up" } else { "down" }],
        )?;
        Ok(())
    }

    pub fn destroy(&self, runner: &dyn CommandRunner) {
        // Both calls are best effort and idempotent.  Destroy the peer first
        // so the pair cannot survive as a half-configured interface.
        let _ = runner.run(IFCONFIG, &[&self.peer_iface, "destroy"]);
        let _ = runner.run(IFCONFIG, &[&self.host_iface, "destroy"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::tests::RecordingRunner;

    fn mac() -> MacAddr {
        MacAddr([0x02, 0, 0, 0, 0, 0x42])
    }

    #[test]
    fn creates_peers_mac_mtu_and_leaves_host_down() {
        let runner = RecordingRunner::new();
        runner.push_output("ifconfig -l", Ok("lo0 en0".to_string()));
        let pair = FethPair::create(&runner, mac(), 1500).unwrap();
        assert_eq!(pair.host_iface, "feth100");
        assert_eq!(pair.peer_iface, "feth1100");
        let calls = runner.joined();
        assert!(calls.iter().any(|c| c == "/sbin/ifconfig feth1100 create"));
        assert!(calls.iter().any(|c| c == "/sbin/ifconfig feth100 create"));
        assert!(calls.iter().any(|c| c.contains("lladdr 02:00:00:00:00:42")));
        assert!(calls
            .iter()
            .any(|c| c == "/sbin/ifconfig feth1100 peer feth100"));
        assert!(calls
            .iter()
            .any(|c| c == "/sbin/ifconfig feth100 mtu 1500 down"));
    }

    #[test]
    fn failure_cleans_up_partial_pair() {
        let runner = RecordingRunner::new();
        runner.push_output("ifconfig -l", Ok("lo0".to_string()));
        runner.push_output("feth1100 peer", Err("peer failed".to_string()));
        assert!(FethPair::create(&runner, mac(), 1500).is_err());
        let calls = runner.joined();
        assert!(calls.iter().any(|c| c == "/sbin/ifconfig feth1100 destroy"));
        assert!(calls.iter().any(|c| c == "/sbin/ifconfig feth100 destroy"));
    }

    #[test]
    fn picks_next_free_pair() {
        let runner = RecordingRunner::new();
        runner.push_output("ifconfig -l", Ok("feth100 feth1100 lo0".to_string()));
        let pair = FethPair::create(&runner, mac(), 1400).unwrap();
        assert_eq!(pair.host_iface, "feth101");
        assert_eq!(pair.peer_iface, "feth1101");
    }

    #[test]
    fn rejects_invalid_mtu() {
        let runner = RecordingRunner::new();
        assert!(FethPair::create(&runner, mac(), 100).is_err());
    }

    #[test]
    fn link_and_destroy_are_best_effort() {
        let runner = RecordingRunner::new();
        let pair = FethPair {
            host_iface: "feth100".into(),
            peer_iface: "feth1100".into(),
            mtu: 1500,
        };
        pair.set_link(&runner, true).unwrap();
        pair.destroy(&runner);
        let calls = runner.joined();
        assert!(calls.contains(&"/sbin/ifconfig feth100 up".to_string()));
        assert!(calls.contains(&"/sbin/ifconfig feth1100 destroy".to_string()));
    }
}
