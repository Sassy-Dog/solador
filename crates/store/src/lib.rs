//! Persistent app state for the cross-platform shell: settings, hosts, tracked
//! repos — plus the credential-store wrappers for the things that must *never*
//! land in that file.
//!
//! The Swift app splits storage three ways — Keychain for secrets,
//! `@AppStorage` for scalar preferences, SwiftData for `MonitoredHost` /
//! `TrackedRepo`. Two of those three are non-secret and small, so this crate
//! collapses them into one JSON file and keeps the third exactly where it was:
//! the OS credential store ([`secrets`]). The split that matters is preserved;
//! the split that was an artifact of three Apple frameworks is not.
//!
//! ```no_run
//! # fn main() -> Result<(), store::StoreError> {
//! let mut store = store::Store::open()?;
//! store.upsert_host(store::Host::new("ubu-3xdv", "100.87.202.125"));
//! store.save()?;
//! # Ok(()) }
//! ```
//!
//! This crate is a pure library: it reads and writes its own file and nothing
//! else. Wiring it into the Tauri shell is a later slice.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod hosts;
pub mod repos;
pub mod secrets;
pub mod settings;

pub use hosts::{Host, DEFAULT_AGENT_PORT};
pub use repos::{seeded_repos, TrackedRepo};
pub use secrets::{CredentialStore, KeyringStore, MemoryCredentialStore, SecretError, SecretKey};
pub use settings::{HostOverflowMode, Settings};

/// Schema version stamped into every file this crate writes.
///
/// The envelope exists so a future migration has something to branch on. A
/// file declaring a *higher* version is refused rather than downgraded — see
/// [`StoreError::UnsupportedVersion`].
pub const STORE_VERSION: u32 = 1;

/// Directory under the platform config dir, named for the app's bundle id:
/// `~/Library/Application Support/com.sassydog.devcanopy.app` on macOS,
/// `%APPDATA%\com.sassydog.devcanopy.app` on Windows.
pub const APP_DIR_NAME: &str = "com.sassydog.devcanopy.app";

/// The one file the whole store lives in.
pub const STORE_FILE_NAME: &str = "store.json";

/// Seconds since the UNIX epoch.
///
/// A clock behind the epoch yields `0` rather than panicking — a nonsense
/// creation date is not worth taking the process down for.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Anything that can go wrong reading or writing the store file.
///
/// Every variant names the path it happened on, because "the store failed" is
/// unactionable when the answer is usually a permissions problem on one file.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// No per-user config directory on this platform (no `HOME`/`%APPDATA%`).
    #[error("this platform has no config directory for the current user")]
    NoConfigDir,
    /// A filesystem operation failed. `action` is a verb phrase, so the
    /// rendered message reads as a sentence.
    #[error("could not {action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The file exists but is not a store. Deliberately an error and not a
    /// silent reset: clobbering a file we failed to understand is how a
    /// configuration disappears.
    #[error("{} is not a valid DevCanopy store: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// Written by a newer build. Fails closed — loading it would mean saving
    /// it back with this build's narrower schema, dropping whatever the newer
    /// build added.
    #[error("{} was written by a newer build (version {found}, this build reads {expected})", path.display())]
    UnsupportedVersion {
        path: PathBuf,
        found: u32,
        expected: u32,
    },
    /// Serialising the in-memory state failed (a non-finite `f64`, say).
    #[error("could not serialize the store: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// The versioned envelope — exactly what lands on disk.
///
/// `#[serde(default)]` on the container is the forward-compatibility rule:
/// a missing section takes its default and an unknown section is ignored, so
/// a file from either side of a schema change still opens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreData {
    /// Schema version — see [`STORE_VERSION`].
    pub version: u32,
    /// Non-secret scalar preferences.
    pub settings: Settings,
    /// Watched hosts, in user-visible order.
    pub hosts: Vec<Host>,
    /// Tracked repo portfolio, in user-visible order.
    pub repos: Vec<TrackedRepo>,
}

impl Default for StoreData {
    /// An *empty* store — note `repos` is empty, not seeded. Seeding is a
    /// first-run event owned by [`Store::open_in`], not a property of the
    /// default value, or every "reset to defaults" path would resurrect repos
    /// the user deleted.
    fn default() -> Self {
        StoreData {
            version: STORE_VERSION,
            settings: Settings::default(),
            hosts: Vec::new(),
            repos: Vec::new(),
        }
    }
}

/// A store bound to one file, held in memory and written back by [`Store::save`].
///
/// Reads are in-memory and infallible; only [`Store::open_in`] and
/// [`Store::save`] touch the disk. Nothing auto-saves — a caller that mutates
/// and never calls `save` has changed nothing on disk, which is the behaviour
/// a Cancel button wants.
#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    data: StoreData,
}

impl Store {
    /// The directory the app's store lives in on this platform.
    ///
    /// # Errors
    /// [`StoreError::NoConfigDir`] when the platform exposes no per-user
    /// config directory.
    pub fn default_dir() -> Result<PathBuf, StoreError> {
        dirs::config_dir()
            .map(|dir| dir.join(APP_DIR_NAME))
            .ok_or(StoreError::NoConfigDir)
    }

    /// Opens (or first-run creates) the store at its platform default path.
    ///
    /// # Errors
    /// [`StoreError::NoConfigDir`], plus anything [`Store::open_in`] returns.
    pub fn open() -> Result<Self, StoreError> {
        Store::open_in(Store::default_dir()?)
    }

    /// Opens the store in `dir`, creating and seeding it if no store file has
    /// ever existed there.
    ///
    /// The explicit directory is what makes this testable: the platform lookup
    /// in [`Store::default_dir`] is a thin default over this, not a separate
    /// code path.
    ///
    /// **Seed-once**: the portfolio in [`repos::SEED_SLUGS`] is written only on
    /// that first creation. Once the file exists, its repo list is the truth —
    /// an empty list stays empty, matching `PortfolioStore`'s contract in the
    /// Swift app. Editing `SEED_SLUGS` never retro-edits a seeded store.
    ///
    /// # Errors
    /// [`StoreError::Parse`] if the file is not JSON this crate understands,
    /// [`StoreError::UnsupportedVersion`] if a newer build wrote it, or
    /// [`StoreError::Io`] on any filesystem failure.
    pub fn open_in(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = dir.as_ref().join(STORE_FILE_NAME);
        match fs::read_to_string(&path) {
            Ok(text) => {
                let mut data: StoreData =
                    serde_json::from_str(&text).map_err(|source| StoreError::Parse {
                        path: path.clone(),
                        source,
                    })?;
                if data.version > STORE_VERSION {
                    return Err(StoreError::UnsupportedVersion {
                        path,
                        found: data.version,
                        expected: STORE_VERSION,
                    });
                }
                // Older-or-equal files are read as-is and re-stamped, so the
                // next save writes this build's version. There is only one
                // version today; this is where a migration would hook in.
                data.version = STORE_VERSION;
                Ok(Store { path, data })
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                let store = Store {
                    path,
                    data: StoreData {
                        repos: seeded_repos(),
                        ..StoreData::default()
                    },
                };
                // Persisted immediately so "has never existed" is decided by
                // the filesystem exactly once, rather than re-seeding on every
                // open until something happens to save.
                store.save()?;
                Ok(store)
            }
            Err(source) => Err(StoreError::Io {
                action: "read",
                path,
                source,
            }),
        }
    }

    /// Writes the store back, atomically: a full temp file in the same
    /// directory, flushed to disk, then renamed over the target. A reader
    /// therefore sees either the old file or the new one — never a half-written
    /// one, and never a truncated one if the process dies mid-write.
    ///
    /// # Errors
    /// [`StoreError::Serialize`] if the state will not serialise, or
    /// [`StoreError::Io`] if the directory cannot be created, the temp file
    /// cannot be written, or the rename fails.
    pub fn save(&self) -> Result<(), StoreError> {
        let json = serde_json::to_string_pretty(&self.data).map_err(StoreError::Serialize)?;
        write_atomically(&self.path, &json)
    }

    /// The file this store reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The whole in-memory envelope.
    #[must_use]
    pub fn data(&self) -> &StoreData {
        &self.data
    }

    /// The non-secret preferences.
    #[must_use]
    pub fn settings(&self) -> &Settings {
        &self.data.settings
    }

    /// Mutable preferences. Call [`Store::save`] to persist.
    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.data.settings
    }

    /// Replaces the preferences wholesale.
    pub fn set_settings(&mut self, settings: Settings) {
        self.data.settings = settings;
    }

    /// Every watched host, in user-visible order.
    #[must_use]
    pub fn hosts(&self) -> &[Host] {
        &self.data.hosts
    }

    /// One host by id.
    #[must_use]
    pub fn host(&self, id: Uuid) -> Option<&Host> {
        self.data.hosts.iter().find(|host| host.id == id)
    }

    /// One host by id, mutable. Call [`Store::save`] to persist.
    pub fn host_mut(&mut self, id: Uuid) -> Option<&mut Host> {
        self.data.hosts.iter_mut().find(|host| host.id == id)
    }

    /// Creates or updates a host, keyed by [`Host::id`].
    ///
    /// One entry point for both, so a caller cannot land two rows sharing an
    /// id — which would mean two hosts pointing at one credential. An update
    /// keeps the host's position in the list.
    pub fn upsert_host(&mut self, host: Host) {
        match self.host_mut(host.id) {
            Some(existing) => *existing = host,
            None => self.data.hosts.push(host),
        }
    }

    /// Removes a host, returning it.
    ///
    /// Does **not** touch the credential store: the caller owns deleting
    /// [`SecretKey::HostToken`] for the same id. Keeping this crate's file I/O
    /// and the OS keychain in separate calls is deliberate — a failed keychain
    /// delete must not decide whether the host row goes away.
    pub fn remove_host(&mut self, id: Uuid) -> Option<Host> {
        let index = self.data.hosts.iter().position(|host| host.id == id)?;
        Some(self.data.hosts.remove(index))
    }

    /// Every tracked repo, in user-visible order.
    #[must_use]
    pub fn repos(&self) -> &[TrackedRepo] {
        &self.data.repos
    }

    /// One repo by `owner/name` slug.
    #[must_use]
    pub fn repo(&self, slug: &str) -> Option<&TrackedRepo> {
        self.data.repos.iter().find(|repo| repo.slug == slug)
    }

    /// One repo by slug, mutable. Call [`Store::save`] to persist.
    pub fn repo_mut(&mut self, slug: &str) -> Option<&mut TrackedRepo> {
        self.data.repos.iter_mut().find(|repo| repo.slug == slug)
    }

    /// Creates or updates a repo, keyed by slug (the identity, as in Swift).
    pub fn upsert_repo(&mut self, repo: TrackedRepo) {
        match self.repo_mut(&repo.slug) {
            Some(existing) => *existing = repo,
            None => self.data.repos.push(repo),
        }
    }

    /// Removes a repo by slug, returning it.
    pub fn remove_repo(&mut self, slug: &str) -> Option<TrackedRepo> {
        let index = self.data.repos.iter().position(|repo| repo.slug == slug)?;
        Some(self.data.repos.remove(index))
    }
}

/// Monotonic per-write suffix, so two saves racing in one directory cannot
/// pick the same temp file and truncate each other's payload.
static WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Temp file + rename, the standard "no torn file" write.
fn write_atomically(path: &Path, contents: &str) -> Result<(), StoreError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir).map_err(|source| StoreError::Io {
        action: "create",
        path: dir.to_path_buf(),
        source,
    })?;

    // Same directory as the target: `rename` is only atomic within one
    // filesystem, and the platform temp dir is routinely a different one.
    let seq = WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(
        ".{STORE_FILE_NAME}.{}.{seq}.tmp",
        std::process::id()
    ));

    if let Err(source) = write_and_sync(&temp, contents) {
        // Best-effort: the write already failed, and a leftover temp file is
        // not worth masking that error with a second one.
        let _ = fs::remove_file(&temp);
        return Err(StoreError::Io {
            action: "write",
            path: temp,
            source,
        });
    }

    if let Err(source) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(StoreError::Io {
            action: "replace",
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// Writes and flushes to the physical disk before returning, so the rename
/// cannot publish a file whose contents are still only in the page cache.
fn write_and_sync(path: &Path, contents: &str) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use super::*;
    use tempfile::TempDir;

    fn temp_dir() -> TempDir {
        TempDir::new().expect("create temp dir")
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn first_open_seeds_the_portfolio_and_writes_the_file() {
        let dir = temp_dir();
        let store = Store::open_in(dir.path()).expect("open");

        assert_eq!(
            store
                .repos()
                .iter()
                .map(|repo| repo.slug.as_str())
                .collect::<Vec<_>>(),
            repos::SEED_SLUGS.to_vec()
        );
        assert!(store.hosts().is_empty());
        assert_eq!(store.settings(), &Settings::default());
        assert!(store.path().exists(), "first open must create the file");
        assert_eq!(entries(dir.path()), vec![STORE_FILE_NAME.to_owned()]);
    }

    #[test]
    fn seeding_happens_only_once() {
        let dir = temp_dir();

        let mut store = Store::open_in(dir.path()).expect("first open");
        assert_eq!(store.repos().len(), repos::SEED_SLUGS.len());
        // The user deletes every seeded repo...
        for slug in repos::SEED_SLUGS {
            assert!(store.remove_repo(slug).is_some(), "remove {slug}");
        }
        store.save().expect("save");

        // ...and they stay deleted across a reopen. An empty list is a
        // decision, not an unseeded store.
        let reopened = Store::open_in(dir.path()).expect("reopen");
        assert!(
            reopened.repos().is_empty(),
            "a store that exists must never be re-seeded"
        );
    }

    #[test]
    fn a_store_that_exists_with_edits_keeps_them() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("first open");
        store.remove_repo("Sassy-Dog/platform").expect("remove");
        store.upsert_repo(TrackedRepo::new("Sassy-Dog/openclaw"));
        store.save().expect("save");

        let reopened = Store::open_in(dir.path()).expect("reopen");
        let slugs: Vec<&str> = reopened.repos().iter().map(|r| r.slug.as_str()).collect();
        assert!(!slugs.contains(&"Sassy-Dog/platform"));
        assert!(slugs.contains(&"Sassy-Dog/openclaw"));
        assert_eq!(slugs.len(), repos::SEED_SLUGS.len());
    }

    #[test]
    fn settings_hosts_and_repos_round_trip_through_the_file() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("open");

        store.set_settings(Settings {
            refresh_interval_secs: 300,
            core_row_span: 3,
            host_overflow_mode: HostOverflowMode::Tabs,
            azure_monthly_budget_usd: 250.0,
            neon_org_id: "org-123".into(),
            sentry_org_slug: "sassy-dog".into(),
            sentry_monthly_event_quota: 100_000,
            openclaw_gateway_url: "https://gateway.example".into(),
            local_hidden_volume_mounts: vec!["/Volumes/Time Machine".into()],
        });

        let mut host = Host::new("ubu-3xdv", "100.87.202.125");
        host.hidden_volume_mounts = vec!["/mnt/scratch".into()];
        let host_id = host.id;
        store.upsert_host(host);
        store.upsert_host(Host::new("mac-mini", "100.64.0.2"));

        store
            .repo_mut("Sassy-Dog/devcanopy")
            .expect("seeded repo")
            .watched_workflows = Some(vec!["Release".into()]);

        store.save().expect("save");

        let reopened = Store::open_in(dir.path()).expect("reopen");
        assert_eq!(reopened.data(), store.data());
        assert_eq!(reopened.settings().refresh_interval_secs, 300);
        assert_eq!(reopened.hosts().len(), 2);
        assert_eq!(
            reopened.host(host_id).expect("host").hidden_volume_mounts,
            vec!["/mnt/scratch".to_owned()]
        );
        assert_eq!(
            reopened
                .repo("Sassy-Dog/devcanopy")
                .expect("repo")
                .watched_workflows
                .as_deref(),
            Some(["Release".to_owned()].as_slice())
        );
    }

    #[test]
    fn the_file_carries_the_version_envelope() {
        let dir = temp_dir();
        let store = Store::open_in(dir.path()).expect("open");
        let raw = fs::read_to_string(store.path()).expect("read");
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");

        assert_eq!(json["version"], serde_json::json!(STORE_VERSION));
        for section in ["settings", "hosts", "repos"] {
            assert!(!json[section].is_null(), "missing section {section}");
        }
    }

    #[test]
    fn a_newer_version_is_refused_rather_than_downgraded() {
        let dir = temp_dir();
        let path = dir.path().join(STORE_FILE_NAME);
        fs::write(
            &path,
            r#"{"version":99,"settings":{},"hosts":[],"repos":[]}"#,
        )
        .expect("write");

        let err = Store::open_in(dir.path()).expect_err("must refuse a newer file");
        assert!(
            matches!(
                err,
                StoreError::UnsupportedVersion {
                    found: 99,
                    expected: STORE_VERSION,
                    ..
                }
            ),
            "unexpected error: {err}"
        );
        // The file the newer build wrote is still there, byte for byte.
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            r#"{"version":99,"settings":{},"hosts":[],"repos":[]}"#,
            "refusing to load must never rewrite the file"
        );
    }

    #[test]
    fn a_malformed_file_is_an_error_not_a_silent_reset() {
        let dir = temp_dir();
        let path = dir.path().join(STORE_FILE_NAME);
        fs::write(&path, "{ this is not json").expect("write");

        let err = Store::open_in(dir.path()).expect_err("must not swallow a broken file");
        assert!(matches!(err, StoreError::Parse { .. }), "got {err}");
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            "{ this is not json",
            "a file we could not parse must not be overwritten"
        );
    }

    #[test]
    fn unknown_top_level_fields_are_tolerated() {
        let dir = temp_dir();
        let path = dir.path().join(STORE_FILE_NAME);
        fs::write(
            &path,
            r#"{"version":1,"hosts":[],"repos":[],"panels_from_a_newer_build":{"a":1}}"#,
        )
        .expect("write");

        let store = Store::open_in(dir.path()).expect("open");
        assert_eq!(store.settings(), &Settings::default());
        assert!(store.hosts().is_empty());
        assert!(
            store.repos().is_empty(),
            "an existing file with an explicit empty list must not be re-seeded"
        );
    }

    #[test]
    fn a_missing_version_reads_as_the_current_one() {
        let dir = temp_dir();
        fs::write(
            dir.path().join(STORE_FILE_NAME),
            r#"{"hosts":[],"repos":[]}"#,
        )
        .expect("write");
        let store = Store::open_in(dir.path()).expect("open");
        assert_eq!(store.data().version, STORE_VERSION);
    }

    #[test]
    fn save_leaves_no_temp_file_behind() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("open");
        for index in 0..25 {
            store.upsert_host(Host::new(
                format!("host-{index}"),
                format!("100.64.0.{index}"),
            ));
            store.save().expect("save");
            // Every intermediate state on disk is a complete, parseable file,
            // and the temp file is never left behind.
            assert_eq!(
                entries(dir.path()),
                vec![STORE_FILE_NAME.to_owned()],
                "temp file left behind after save {index}"
            );
            let raw = fs::read_to_string(store.path()).expect("read");
            serde_json::from_str::<StoreData>(&raw).expect("every save must leave valid JSON");
        }
        assert_eq!(
            Store::open_in(dir.path()).expect("reopen").hosts().len(),
            25
        );
    }

    #[test]
    fn a_concurrent_reader_never_sees_a_torn_file() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("open");
        // Big enough that a non-atomic write is torn for a meaningful stretch:
        // a plain `fs::write` truncates the target and then streams into it,
        // and everything in between is a document no reader can use.
        store.settings_mut().local_hidden_volume_mounts = (0..8_000)
            .map(|index| format!("/Volumes/scratch-{index:060}"))
            .collect();
        store.save().expect("seed the file");
        let full_len = fs::metadata(store.path()).expect("stat").len();

        let path = store.path().to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop);
        let reader = std::thread::spawn(move || {
            let mut complete_reads = 0u64;
            while !reader_stop.load(Ordering::Relaxed) {
                // Bytes, and only the shape — not a full parse. The torn window
                // is sub-millisecond, so a reader that spends milliseconds
                // deserialising would stroll straight past it and report a
                // green test that proves nothing (verified: it did).
                let Ok(raw) = fs::read(&path) else {
                    // The rename replaces in place, so the path is never absent
                    // on Unix; a Windows sharing violation is a failed read,
                    // not a torn one.
                    continue;
                };
                assert!(
                    raw.first() == Some(&b'{') && raw.last() == Some(&b'}'),
                    "a reader observed a half-written store ({} bytes)",
                    raw.len()
                );
                assert!(
                    raw.len() as u64 >= full_len / 2,
                    "a reader observed a truncated store ({} of ~{full_len} bytes)",
                    raw.len()
                );
                complete_reads += 1;
            }
            complete_reads
        });

        for index in 0..100 {
            store.settings_mut().neon_org_id = format!("org-{index}");
            store.save().expect("save");
        }
        stop.store(true, Ordering::Relaxed);

        let complete_reads = reader.join().expect("reader thread");
        assert!(
            complete_reads > 0,
            "the reader never actually read the file, so it proved nothing"
        );
        // And the final state is still a real store, not just well-shaped bytes.
        assert_eq!(
            Store::open_in(dir.path())
                .expect("reopen")
                .settings()
                .neon_org_id,
            "org-99"
        );
    }

    #[test]
    fn save_creates_missing_directories() {
        let dir = temp_dir();
        let nested = dir.path().join("Application Support").join(APP_DIR_NAME);
        let store = Store::open_in(&nested).expect("open");
        assert!(store.path().exists());
        assert_eq!(
            Store::open_in(&nested).expect("reopen").repos().len(),
            repos::SEED_SLUGS.len()
        );
    }

    #[test]
    fn host_crud_round_trips() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("open");
        assert!(store.hosts().is_empty());

        let host = Host::new("ubu-3xdv", "100.87.202.125");
        let id = host.id;
        store.upsert_host(host);
        assert_eq!(store.hosts().len(), 1);
        assert_eq!(store.host(id).expect("read").name, "ubu-3xdv");

        store.host_mut(id).expect("update").enabled = false;
        assert!(!store.host(id).expect("read back").enabled);

        // Upserting the same id updates in place instead of duplicating it —
        // two rows sharing an id would mean two hosts on one credential.
        let mut renamed = store.host(id).expect("clone source").clone();
        renamed.name = "ubu-3xdv (spare)".into();
        store.upsert_host(renamed);
        assert_eq!(store.hosts().len(), 1);
        assert_eq!(store.host(id).expect("read").name, "ubu-3xdv (spare)");

        assert_eq!(store.remove_host(id).expect("delete").id, id);
        assert!(store.hosts().is_empty());
        assert!(store.host(id).is_none());
        assert!(store.remove_host(id).is_none(), "deleting twice is a no-op");
    }

    #[test]
    fn repo_crud_round_trips() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("open");
        let seeded = store.repos().len();

        store.upsert_repo(TrackedRepo::new("Sassy-Dog/openclaw"));
        assert_eq!(store.repos().len(), seeded + 1);

        store
            .repo_mut("Sassy-Dog/openclaw")
            .expect("update")
            .enabled = false;
        assert!(!store.repo("Sassy-Dog/openclaw").expect("read").enabled);

        store.upsert_repo(TrackedRepo::new("Sassy-Dog/openclaw"));
        assert_eq!(store.repos().len(), seeded + 1, "slug is the identity");
        assert!(store.repo("Sassy-Dog/openclaw").expect("read").enabled);

        assert_eq!(
            store
                .remove_repo("Sassy-Dog/openclaw")
                .expect("delete")
                .slug,
            "Sassy-Dog/openclaw"
        );
        assert_eq!(store.repos().len(), seeded);
        assert!(store.remove_repo("Sassy-Dog/openclaw").is_none());
    }

    #[test]
    fn the_file_holds_no_secret_material() {
        let dir = temp_dir();
        let mut store = Store::open_in(dir.path()).expect("open");
        let host = Host::new("ubu-3xdv", "100.87.202.125");
        let host_id = host.id;
        store.upsert_host(host);
        store.settings_mut().neon_org_id = "org-123".into();
        store.save().expect("save");

        // Credentials are set for the very host and providers configured
        // above — the moment they would leak into the file if the two stores
        // were ever wired together.
        let credentials = MemoryCredentialStore::new();
        credentials
            .set_secret(SecretKey::HostToken(host_id), "agent-token-value")
            .expect("set host token");
        credentials
            .set_secret(SecretKey::GitHubAccessToken, "ghp_value")
            .expect("set github token");

        let raw = fs::read_to_string(store.path()).expect("read");
        for value in ["agent-token-value", "ghp_value"] {
            assert!(
                !raw.contains(value),
                "a credential value reached {}",
                store.path().display()
            );
        }

        // Nor is there a *field* a credential could be smuggled into. Checked
        // on keys, not the raw text: the org slug `Sassy-Dog` contains "sas",
        // and a substring scan of the whole file would fail on that forever
        // while proving nothing.
        let json: serde_json::Value = serde_json::from_str(&raw).expect("parse");
        let keys = json_keys(&json);
        for account in [
            SecretKey::HostToken(host_id).account(),
            SecretKey::GitHubAccessToken.account(),
            SecretKey::NeonApiKey.account(),
            SecretKey::SentryUsageToken.account(),
            SecretKey::AzureCostSasUrl.account(),
            SecretKey::OpenClawBearerToken.account(),
        ] {
            assert!(
                !keys.contains(&account),
                "{account} must live in the credential store, not the store file"
            );
        }
        for word in [
            "token",
            "secret",
            "password",
            "credential",
            "api_key",
            "sas",
        ] {
            assert!(
                !keys.iter().any(|key| key.contains(word)),
                "store file has a {word}-shaped field: {keys:?}"
            );
        }
    }

    /// Every object key in the document, at any depth.
    fn json_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys = Vec::new();
        collect_keys(value, &mut keys);
        keys
    }

    fn collect_keys(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    out.push(key.clone());
                    collect_keys(child, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_keys(item, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn debug_of_a_store_prints_no_file_contents_beyond_config() {
        // A `Store` is all non-secret config by construction, so `Debug` is
        // safe — this pins that by asserting the credential wrapper is not
        // reachable from it.
        let dir = temp_dir();
        let store = Store::open_in(dir.path()).expect("open");
        let rendered = format!("{store:?}");
        assert!(rendered.contains("Store"));
        assert!(!rendered.to_lowercase().contains("token"));
    }

    #[test]
    fn default_dir_sits_under_the_platform_config_dir() {
        let Ok(dir) = Store::default_dir() else {
            return; // No per-user config dir on this machine; nothing to pin.
        };
        assert!(dir.ends_with(APP_DIR_NAME), "{}", dir.display());
        assert_eq!(dir.parent(), dirs::config_dir().as_deref());
    }
}
