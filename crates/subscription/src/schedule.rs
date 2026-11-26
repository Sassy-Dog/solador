//! Each provider's read schedule, kept across a relaunch.
//!
//! The shell backs a provider off after a 429 (`plan_usage::record_outcome`),
//! and a relaunch, an app update included, must not be a way round that wait: a
//! launch inside it waits. So the facts the wait is made of are written to one
//! small file beside `store.json`, [`FILE_NAME`]:
//!
//! - when the provider's last read started,
//! - how many 429s in a row it has had,
//! - the instant before which no read is made,
//!
//! plus whether the latest read's outcome was a 429, which is what shows the
//! Claude card Claude Code's status-line figures until a read recovers
//! ([`crate::statusline`]).
//!
//! # Nothing from the service, no token
//!
//! A [`Schedule`] is three numbers and a flag. It holds no token, no header and
//! nothing from a response, and a file that names more is read for those fields
//! alone. It is its own file rather than a section of `store.json` because it is
//! rewritten after every read, and `store.json` is the operator's settings:
//! Cancel in Settings discards unsaved edits to those, and nothing here should
//! ride a save of them or race one.
//!
//! # Unknown is not a wait
//!
//! An absent, unreadable or unparseable file is "no schedule known": the
//! default, which reads at once. A wait that cannot be shown to be owed is not
//! invented.
//!
//! No wall clock: every instant in the file was passed in by the caller.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::Provider;

/// The file's name, in the directory `store.json` lives in.
pub const FILE_NAME: &str = "plan-schedule.json";

/// One provider's schedule. Unix seconds throughout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    /// When the last read was started.
    pub last_attempt: Option<u64>,
    /// Consecutive 429s since the last success. Other failures leave it alone.
    pub rate_limited: u32,
    /// No read before this instant.
    pub not_before: Option<u64>,
    /// Whether the latest read's outcome was a 429.
    pub last_was_rate_limited: bool,
}

/// Both providers' schedules, as the file holds them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedules {
    pub claude: Schedule,
    pub codex: Schedule,
}

impl Schedules {
    #[must_use]
    pub const fn get(&self, provider: Provider) -> Schedule {
        match provider {
            Provider::Claude => self.claude,
            Provider::Codex => self.codex,
        }
    }

    pub fn set(&mut self, provider: Provider, schedule: Schedule) {
        match provider {
            Provider::Claude => self.claude = schedule,
            Provider::Codex => self.codex = schedule,
        }
    }
}

/// Reads the schedules in `dir`. An absent, unreadable or malformed file is the
/// default (see the module docs).
#[must_use]
pub fn load(dir: &Path) -> Schedules {
    std::fs::read_to_string(dir.join(FILE_NAME))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Monotonic per-write suffix, so two writes in one process cannot pick the same
/// temporary file.
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Writes `schedules` into `dir`: a temporary file in the same directory, then a
/// rename over [`FILE_NAME`], so a reader never sees a half-written file.
///
/// # Errors
/// Whatever creating the directory, writing the temporary file or renaming it
/// returns. The temporary file is removed on failure.
pub fn save(dir: &Path, schedules: &Schedules) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_string(schedules)?;
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{FILE_NAME}.{}.{seq}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, dir.join(FILE_NAME))
    })();
    if written.is_err() {
        // Best effort: the write already failed, and a second error would mask it.
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting() -> Schedule {
        Schedule {
            last_attempt: Some(1_000),
            rate_limited: 2,
            not_before: Some(5_000),
            last_was_rate_limited: true,
        }
    }

    #[test]
    fn a_saved_schedule_loads_back_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let mut saved = Schedules::default();
        saved.set(Provider::Claude, waiting());
        save(dir.path(), &saved).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded, saved);
        assert_eq!(loaded.get(Provider::Claude), waiting());
        assert_eq!(loaded.get(Provider::Codex), Schedule::default());
    }

    #[test]
    fn an_absent_or_malformed_file_is_no_schedule_rather_than_a_wait() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()), Schedules::default());
        for junk in ["", "not json", "[1,2]", "{\"claude\":\"soon\"}"] {
            std::fs::write(dir.path().join(FILE_NAME), junk).unwrap();
            assert_eq!(load(dir.path()), Schedules::default(), "{junk:?}");
        }
    }

    #[test]
    fn the_file_holds_the_schedule_facts_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut saved = Schedules::default();
        saved.set(Provider::Codex, waiting());
        save(dir.path(), &saved).unwrap();
        let text = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys: Vec<_> = value["codex"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "last_attempt",
                "last_was_rate_limited",
                "not_before",
                "rate_limited"
            ]
        );
        assert_eq!(
            value.as_object().unwrap().len(),
            2,
            "one entry per provider"
        );
    }

    #[test]
    fn a_save_replaces_the_file_whole_and_leaves_no_temporary_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = Schedules::default();
        first.set(Provider::Claude, waiting());
        save(dir.path(), &first).unwrap();
        save(dir.path(), &Schedules::default()).unwrap();
        assert_eq!(load(dir.path()), Schedules::default());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [FILE_NAME], "no .tmp file is left beside it");
    }

    #[test]
    fn a_save_into_a_missing_directory_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b");
        save(&nested, &Schedules::default()).unwrap();
        assert!(nested.join(FILE_NAME).is_file());
    }
}
