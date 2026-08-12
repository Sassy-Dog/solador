//! Persistence for the auto-learned self-hosted runner roster — the Rust
//! mirror of the `ghRunnerRoster` UserDefaults blob the original app writes
//! (`GHRunnerRoster`).
//!
//! Only the *record* lives here. Every rule about it — learning a sighting,
//! the 24h age-out, the absence clocks — is `github::roster`'s, and this crate
//! deliberately does not depend on that one: the store is a file, and a file
//! format should not drag an HTTP client into anything that opens it.
//!
//! The bridge is therefore the app's, and it is a two-field conversion
//! (`app/src-tauri/src/github/`). The one thing that must not drift is the
//! `os` spelling, which is why [`github::RunnerOs::as_raw`]'s doc comment
//! names this file: the value stored here is that raw value, not the display
//! label beside it.

use serde::{Deserialize, Serialize};

/// One remembered runner name, as it lands on disk.
///
/// `last_seen` is unix seconds rather than an RFC 3339 string, matching
/// [`crate::ContainerPresenceRecord`]: both are absence clocks read by the same
/// kind of presence arithmetic, and one file should not carry two spellings of
/// "when we last saw this".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerRosterRecord {
    /// The runner's registered name — the identity, since an ephemeral runner
    /// gets a fresh numeric id on every re-registration.
    pub name: String,
    /// `"macOS"` / `"linux"` / `"other"` — `github::RunnerOs::as_raw`, which is
    /// also the original `RunnerOS` raw value. Stored as a string so this crate
    /// stays free of the GitHub crate; an unrecognised value reads as `other`
    /// on the way back rather than failing the roster.
    pub os: String,
    /// Seconds since the UNIX epoch when this runner was last registered.
    pub last_seen: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let record = RunnerRosterRecord {
            name: "mac-s2".into(),
            os: "macOS".into(),
            last_seen: 1_700_000_000,
        };
        let json = serde_json::to_string(&record).expect("serialize");
        assert_eq!(
            serde_json::from_str::<RunnerRosterRecord>(&json).expect("deserialize"),
            record
        );
    }

    /// The stored key names are a format. A rename here silently forgets every
    /// remembered runner on the next launch, so they are pinned.
    #[test]
    fn the_stored_keys_are_the_documented_ones() {
        let json = serde_json::to_string(&RunnerRosterRecord {
            name: "ubu-1".into(),
            os: "linux".into(),
            last_seen: 42,
        })
        .expect("serialize");
        assert_eq!(json, r#"{"name":"ubu-1","os":"linux","last_seen":42}"#);
    }
}
