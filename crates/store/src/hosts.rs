//! Watched hosts — the Rust mirror of the original persistence layer's `MonitoredHost`
//! (`MonitoredHost`).
//!
//! Same split as the original: connection details are persisted here, the per-host
//! bearer token is not. It lives in the OS credential store under
//! [`crate::SecretKey::HostToken`], keyed by this struct's `id`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::now_unix;

/// The agent's default listen port (`MonitoredHost.port`).
pub const DEFAULT_AGENT_PORT: u16 = 7878;

/// One remote machine running the Solador agent.
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
    /// The SHA-256 fingerprint of the one certificate this host's agent may
    /// present (#448), in `crates/certpin`'s canonical form. `None` means the
    /// host is dialled over plain HTTP, exactly as every host was before.
    ///
    /// A non-secret preference that lives in `store.json` beside the address:
    /// it is a public certificate's hash, and the only thing it can do is make
    /// the client *refuse* more. It is set only by the operator clicking
    /// **Trust** on a fingerprint the cockpit just fetched, never typed in and
    /// never adopted from a poll — a pin learned silently is not a pin.
    ///
    /// Absent on disk is `None`, and omitted when `None`, so a store written
    /// before this field existed loads unchanged and one that never pins a
    /// host stays byte-for-byte what it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tls_fingerprint: Option<String>,
}

impl Host {
    /// A new host with the original initialiser's defaults: port 7878, enabled,
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
            tls_fingerprint: None,
        }
    }

    /// This host's agent base URL: `https://100.100.100.100:7878` when it is
    /// pinned to a certificate (#448), `http://100.100.100.100:7878` otherwise.
    ///
    /// **The scheme follows the pin and nothing else.** A host with a
    /// fingerprint is *never* `http://` — not after a failure, not because the
    /// agent stopped answering TLS — because a client that falls back to plain
    /// HTTP when the pinned handshake fails has only made the pin advisory. An
    /// agent that has stopped speaking TLS is reported as such
    /// (`fault::Fault::NoTls`), not followed down.
    ///
    /// A host with no pin stays plain HTTP: over Tailscale the transport is
    /// what carries the encryption (see `agent/README.md`), and a store from
    /// before pinning existed must keep polling the agents it always polled.
    /// `Some("")` is still a pin — an unusable one, which no certificate
    /// matches — so a hand-edited store cannot turn pinning *off* by blanking
    /// the string.
    #[must_use]
    pub fn base_url(&self) -> String {
        let scheme = if self.tls_fingerprint.is_some() {
            "https"
        } else {
            "http"
        };
        format!("{scheme}://{}:{}", self.address, self.port)
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
    fn new_uses_the_original_defaults() {
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
    fn a_pinned_host_is_dialled_over_https_and_only_https() {
        let mut host = Host::new("ubu-01", "100.100.100.100");
        host.tls_fingerprint = Some("AB:CD".into());
        assert_eq!(host.base_url(), "https://100.100.100.100:7878");
        host.port = 9000;
        assert_eq!(host.base_url(), "https://100.100.100.100:9000");
        // A blanked pin is an unusable pin, not the absence of one.
        host.tls_fingerprint = Some(String::new());
        assert!(host.base_url().starts_with("https://"));
    }

    /// The downgrade guard at the store layer: whatever else changes about a
    /// pinned host, none of it turns its URL back into `http://`.
    #[test]
    fn nothing_but_removing_the_pin_makes_a_pinned_host_plain_http() {
        let mut host = Host::new("ubu-01", "100.100.100.100");
        host.tls_fingerprint = Some("AB:CD".into());
        for (address, port) in [("localhost", 1), ("[::1]", 65535), ("a.b.c", 7878)] {
            host.address = address.into();
            host.port = port;
            host.enabled = !host.enabled;
            assert!(host.base_url().starts_with("https://"), "{address}:{port}");
        }
        host.tls_fingerprint = None;
        assert!(host.base_url().starts_with("http://"));
    }

    #[test]
    fn a_store_from_before_pinning_loads_unpinned_and_saves_unchanged() {
        let id = Uuid::new_v4();
        let json = format!(r#"{{"id":"{id}","name":"box","address":"10.0.0.1","port":7878}}"#);
        let host: Host = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(host.tls_fingerprint, None);
        assert_eq!(host.base_url(), "http://10.0.0.1:7878");
        // Omitted, not `null`: an unpinned host's entry gains no key.
        let saved = serde_json::to_string(&host).expect("serialize");
        assert!(!saved.contains("tls_fingerprint"), "{saved}");
    }

    #[test]
    fn a_pin_round_trips_through_json() {
        let mut host = Host::new("ubu-01", "100.100.100.100");
        host.tls_fingerprint = Some("45:39:AF".into());
        let json = serde_json::to_string(&host).expect("serialize");
        assert!(json.contains(r#""tls_fingerprint":"45:39:AF""#), "{json}");
        assert_eq!(
            serde_json::from_str::<Host>(&json).expect("deserialize"),
            host
        );
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
