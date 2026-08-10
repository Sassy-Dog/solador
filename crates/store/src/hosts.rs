//! Watched hosts — the Rust mirror of SwiftData's `MonitoredHost`
//! (`DevCanopy/Models/MonitoredHost.swift`).
//!
//! Same split as Swift: connection details are persisted here, the per-host
//! bearer token is not. It lives in the OS credential store under
//! [`crate::SecretKey::HostToken`], keyed by this struct's `id`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::now_unix;

/// The agent's default listen port (`MonitoredHost.port`).
pub const DEFAULT_AGENT_PORT: u16 = 7878;

/// One remote machine running the DevCanopy agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    /// Stable identity, and the key the host's bearer token is stored under.
    /// Deliberately *not* `#[serde(default)]`: a nil-UUID fallback would let
    /// two id-less entries collapse onto one credential.
    pub id: Uuid,
    /// Display name in the cockpit.
    pub name: String,
    /// Tailscale IP or MagicDNS name.
    pub address: String,
    #[serde(default = "default_agent_port")]
    pub port: u16,
    /// Disabled hosts stay configured but are not polled.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    /// Seconds since the UNIX epoch. Absent on disk means "unrecorded", and
    /// the read stamps it with now rather than inventing a 1970 creation date.
    #[serde(default = "now_unix")]
    pub created_at: u64,
    /// Mount paths the user hid from this host's Volumes section.
    #[serde(default)]
    pub hidden_volume_mounts: Vec<String>,
}

impl Host {
    /// A new host with the Swift initialiser's defaults: port 7878, enabled,
    /// created now, nothing hidden.
    #[must_use]
    pub fn new(name: impl Into<String>, address: impl Into<String>) -> Self {
        Host {
            id: Uuid::new_v4(),
            name: name.into(),
            address: address.into(),
            port: DEFAULT_AGENT_PORT,
            enabled: true,
            created_at: now_unix(),
            hidden_volume_mounts: Vec::new(),
        }
    }

    /// This host's agent base URL, e.g. `http://100.100.100.100:7878`.
    ///
    /// Plain HTTP is correct here: the transport is Tailscale, which is what
    /// carries the encryption (see `agent/README.md`).
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.address, self.port)
    }
}

const fn default_agent_port() -> u16 {
    DEFAULT_AGENT_PORT
}

const fn enabled_by_default() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_the_swift_defaults() {
        let host = Host::new("ubu-01", "100.100.100.100");
        assert_eq!(host.port, 7878);
        assert!(host.enabled);
        assert!(host.hidden_volume_mounts.is_empty());
        assert!(host.created_at > 0);
        assert_ne!(host.id, Uuid::nil());
    }

    #[test]
    fn ids_are_unique_per_host() {
        assert_ne!(Host::new("a", "1.1.1.1").id, Host::new("b", "1.1.1.2").id);
    }

    #[test]
    fn base_url_joins_address_and_port() {
        let mut host = Host::new("ubu-01", "100.100.100.100");
        assert_eq!(host.base_url(), "http://100.100.100.100:7878");
        host.port = 9000;
        assert_eq!(host.base_url(), "http://100.100.100.100:9000");
    }

    #[test]
    fn round_trips_through_json() {
        let mut host = Host::new("ubu-01", "100.100.100.100");
        host.hidden_volume_mounts = vec!["/mnt/scratch".into()];
        host.enabled = false;
        let json = serde_json::to_string(&host).expect("serialize");
        assert_eq!(
            serde_json::from_str::<Host>(&json).expect("deserialize"),
            host
        );
    }

    #[test]
    fn optional_fields_fall_back_when_absent() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{"id":"{id}","name":"box","address":"10.0.0.1"}}"#);
        let host: Host = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(host.port, DEFAULT_AGENT_PORT);
        assert!(host.enabled);
        assert!(host.hidden_volume_mounts.is_empty());
        assert!(host.created_at > 0);
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let id = Uuid::new_v4();
        let json = format!(
            r#"{{"id":"{id}","name":"box","address":"10.0.0.1","last_seen":"2026-07-31T00:00:00Z"}}"#
        );
        let host: Host = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(host.name, "box");
    }

    #[test]
    fn an_entry_without_an_id_is_rejected() {
        let err = serde_json::from_str::<Host>(r#"{"name":"box","address":"10.0.0.1"}"#);
        assert!(err.is_err(), "a host with no identity must not load");
    }
}
