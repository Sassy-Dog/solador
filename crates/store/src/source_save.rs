//! What a source's guided steps apply when the operator presses Finish: the
//! table of sources, every refusal, the credential write, the plan toggle and
//! the one flag-and-tile save with its rollback.
//!
//! This is the logic behind the shell's `settings_save_source` command, so
//! each source #721 puts on the steps is a row in [`SOURCES`] and a change in
//! this file, testable without the shell. What only the shell can do comes in
//! through [`SourceSaveEnv`]: the store and credential handles, the dashboard
//! rules, the plan opt-in (it resets the shell's in-memory state) and the
//! wakes.
//!
//! **The pure dashboard helpers were passed in, not moved**: `validate`,
//! `check_tile_count` and `with_source_tile` read the shell's panel catalogue
//! (`dashboard::SOURCES`, scopes, view settings), so they stay beside it and
//! reach this module through [`SourceSaveEnv::tile_added`] and
//! [`SourceSaveEnv::persist_dashboard`].

use std::sync::Mutex;

use crate::secrets::{read_credential, Credential};
use crate::{CredentialStore, DashboardLayout, SecretKey, Settings, Store};

/// Everything read about Claude: API spend, the plan's limits and local usage.
pub const ANTHROPIC_TITLE: &str = "Anthropic";
/// Everything read about Codex and the OpenAI API.
pub const OPENAI_TITLE: &str = "OpenAI";
/// This Mac or PC's own metrics and containers.
pub const LOCAL_TITLE: &str = "This machine";
/// The status pages of the sources this store has added.
pub const SERVICES_TITLE: &str = "Service health";

/// A provider whose own sign-in a plan opt-in reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanProvider {
    Claude,
    Codex,
}

/// The in-memory API spend figure an Admin key feeds, which goes with the key
/// when the key is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpendFigure {
    Anthropic,
    OpenAi,
}

/// The poll loop a save wakes. Every save also wakes the status pages, since
/// any part can add or remove a watched page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceWake {
    /// Both halves of the usage loop.
    UsageEverything,
    /// The metered providers only.
    UsageProviders,
    /// The containers loop.
    Containers,
    /// Nothing besides the status pages.
    StatusPagesOnly,
}

/// A source's Admin key and the spend figure it feeds.
#[derive(Debug, Clone, Copy)]
pub struct ApiKey {
    pub secret: SecretKey,
    pub spend: SpendFigure,
}

/// The one flag a source's steps own, and the part that sets it. A source with
/// neither a flag nor a local part is derived from its key and its plan alone.
#[derive(Clone, Copy)]
pub struct SourceFlag {
    pub part: &'static str,
    access: fn(&mut Settings) -> &mut bool,
}

/// One source the steps can add, change or remove.
pub struct SourceSpec {
    pub kind: &'static str,
    pub title: &'static str,
    /// The dashboard tile source its tile is added under.
    pub tile_source: &'static str,
    pub key: Option<ApiKey>,
    pub plan: Option<PlanProvider>,
    pub flag: Option<SourceFlag>,
    pub wake: SourceWake,
}

/// Every source the steps know. A new source is one row here.
pub const SOURCES: [SourceSpec; 4] = [
    SourceSpec {
        kind: "anthropic",
        title: ANTHROPIC_TITLE,
        tile_source: "anthropic",
        key: Some(ApiKey {
            secret: SecretKey::AnthropicAdminKey,
            spend: SpendFigure::Anthropic,
        }),
        plan: Some(PlanProvider::Claude),
        flag: Some(SourceFlag {
            part: "local",
            access: |settings| &mut settings.claude_local_logs_enabled,
        }),
        wake: SourceWake::UsageEverything,
    },
    SourceSpec {
        kind: "openai",
        title: OPENAI_TITLE,
        tile_source: "openai",
        key: Some(ApiKey {
            secret: SecretKey::OpenAiAdminKey,
            spend: SpendFigure::OpenAi,
        }),
        plan: Some(PlanProvider::Codex),
        flag: None,
        wake: SourceWake::UsageProviders,
    },
    SourceSpec {
        kind: "local",
        title: LOCAL_TITLE,
        tile_source: "hosts",
        key: None,
        plan: None,
        flag: Some(SourceFlag {
            part: "local",
            access: |settings| &mut settings.local_monitoring_enabled,
        }),
        wake: SourceWake::Containers,
    },
    SourceSpec {
        kind: "services",
        title: SERVICES_TITLE,
        tile_source: "services",
        key: None,
        plan: None,
        flag: Some(SourceFlag {
            part: "services",
            access: |settings| &mut settings.service_health_enabled,
        }),
        wake: SourceWake::StatusPagesOnly,
    },
];

/// The source named `kind`, or `None` for one the steps do not know.
#[must_use]
pub fn spec(kind: &str) -> Option<&'static SourceSpec> {
    SOURCES.iter().find(|spec| spec.kind == kind)
}

/// What a save needs from the shell.
pub trait SourceSaveEnv {
    /// The store. The save takes its lock itself, and never while it calls any
    /// other method here except [`Self::persist_dashboard`], which is handed
    /// the locked store.
    fn store(&self) -> &Mutex<Store>;
    /// The OS credential store.
    fn credentials(&self) -> &dyn CredentialStore;
    /// `current` with a tile of `source` added, or `None` when it already has
    /// one. Everything the shell's dashboard rules can refuse is refused here,
    /// so a caller can ask before it writes anything.
    ///
    /// # Errors
    /// The sentence to show when the layout refuses the tile.
    fn tile_added(
        &self,
        current: &DashboardLayout,
        source: &str,
    ) -> Result<Option<DashboardLayout>, String>;
    /// Validates and saves `layout`, which was read at `expected_revision`.
    ///
    /// # Errors
    /// The sentence to show when the layout is refused or the write fails.
    fn persist_dashboard(
        &self,
        store: &mut Store,
        layout: DashboardLayout,
        expected_revision: u64,
    ) -> Result<(), String>;
    /// Whether the plan opt-in for `provider` is on now.
    fn plan_enabled(&self, provider: PlanProvider) -> bool;
    /// Applies one plan opt-in, and says how it went (a status starting with
    /// `Failed` is a failed save).
    fn set_plan(&self, provider: PlanProvider, enabled: bool) -> Option<String>;
    /// Drops the spend figure a removed key fed.
    fn unconfigure_spend(&self, spend: SpendFigure);
    /// Wakes `wake`'s loop and the status pages, after a save.
    fn wake(&self, wake: SourceWake);
}

/// The layout with a tile of `source` in it, and the revision it was read at, or
/// `None` when one is already there. Everything `persist_dashboard` can refuse
/// is refused here, so a caller can ask before it writes anything.
fn layout_with_tile(
    env: &dyn SourceSaveEnv,
    store: &Store,
    source: &str,
) -> Result<Option<(DashboardLayout, u64)>, String> {
    let current = store
        .dashboard()
        .cloned()
        .unwrap_or_else(DashboardLayout::shipped);
    Ok(env
        .tile_added(&current, source)?
        .map(|next| (next, current.revision)))
}

/// Everything a source's steps apply, all at once on Finish.
///
/// `Err` is the sentence to show, and the steps stay open with the operator's
/// choices. Every refusal is checked before the first write, so it changes
/// nothing. Only a failure can follow a write: an I/O error, or a dashboard edit
/// that filled the last slot while Finish ran, because the tile is derived again
/// inside the save's lock. Finish is safe to repeat after either: the flag and
/// the tile go back, and a key or plan already written stays.
///
/// `secret` is the Admin key the operator typed, if any. It goes straight to
/// the credential store and is never logged.
///
/// # Errors
/// The refusal or failure sentence, as above.
///
/// # Panics
/// If the store's lock is poisoned.
pub fn save_source(
    env: &dyn SourceSaveEnv,
    kind: &str,
    parts: &[String],
    secret: Option<&str>,
    add_tile: bool,
) -> Result<String, String> {
    let wants = |part: &str| parts.iter().any(|id| id == part);
    let Some(spec) = spec(kind) else {
        return Err("Skipped \u{2014} unknown source.".to_owned());
    };

    // Whatever can refuse goes first.
    let secret = secret.map(str::trim).filter(|value| !value.is_empty());
    let credential = spec
        .key
        .map(|key| read_credential(env.credentials(), key.secret));
    if wants("api") && secret.is_none() && matches!(credential, Some(Credential::Absent)) {
        return Err("Enter an Admin API key, or untick API key.".to_owned());
    }
    // Unticked means delete, and a store that won't answer can't say whether
    // there is anything to delete. "Removed" would be a guess.
    if !wants("api") && matches!(credential, Some(Credential::Unreadable)) {
        return Err(
            "Couldn't read the credential store, so a stored API key can't be removed. \
             Nothing changed."
                .to_owned(),
        );
    }
    let wants_tile = add_tile && !parts.is_empty();
    if wants_tile {
        layout_with_tile(
            env,
            &env.store().lock().expect("store poisoned"),
            spec.tile_source,
        )?;
    }

    if let Some(key) = spec.key {
        let stored = matches!(credential, Some(Credential::Present(_)));
        let written = match (wants("api"), secret) {
            (true, Some(value)) => env.credentials().set_secret(key.secret, value),
            (false, _) if stored => env.credentials().delete_secret(key.secret),
            _ => Ok(()),
        };
        if let Err(e) = written {
            // `SecretError` carries the account name and never the value.
            eprintln!("could not store a credential: {e}");
            return Err(
                "Failed to save \u{2014} the credential store rejected the write.".to_owned(),
            );
        }
        // The figure goes with the key, rather than when a pass reaches it.
        if stored && !wants("api") {
            env.unconfigure_spend(key.spend);
        }
    }

    // Only on a change: a toggle resets the provider, which would un-park a
    // refused keychain and re-read a sign-in because a walk was reopened.
    if let Some(provider) = spec.plan {
        if env.plan_enabled(provider) != wants("subscription") {
            let status = env.set_plan(provider, wants("subscription"));
            if let Some(failed) = status.filter(|status| status.starts_with("Failed")) {
                return Err(failed);
            }
        }
    }

    // One lock and one save, with the tile derived inside it: no dashboard edit
    // can land between the read and the write.
    {
        let mut store = env.store().lock().expect("store poisoned");
        let previous = spec
            .flag
            .map(|flag| std::mem::replace((flag.access)(store.settings_mut()), wants(flag.part)));
        let tile = if wants_tile {
            layout_with_tile(env, &store, spec.tile_source)
        } else {
            Ok(None)
        };
        let written = tile.and_then(|tile| match tile {
            Some((next, revision)) => env.persist_dashboard(&mut store, next, revision),
            None => store.save().map_err(|error| format!("Failed: {error}")),
        });
        if let Err(failed) = written {
            if let (Some(previous), Some(flag)) = (previous, spec.flag) {
                *(flag.access)(store.settings_mut()) = previous;
            }
            return Err(failed);
        }
    }

    env.wake(spec.wake);
    let word = if parts.is_empty() { "removed" } else { "saved" };
    Ok(format!("{} {word}.", spec.title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryCredentialStore;
    use std::cell::{Cell, RefCell};

    /// A shell with nothing in it: plan toggles and wakes are recorded, and the
    /// dashboard rules accept any tile unless told otherwise.
    struct Fake {
        store: Mutex<Store>,
        credentials: MemoryCredentialStore,
        plans: RefCell<Vec<(PlanProvider, bool)>>,
        woken: RefCell<Vec<SourceWake>>,
        spend_dropped: RefCell<Vec<SpendFigure>>,
        refuse_tile: Cell<bool>,
        _dir: tempfile::TempDir,
    }

    impl Fake {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_in(dir.path(), false).unwrap();
            Self {
                store: Mutex::new(store),
                credentials: MemoryCredentialStore::new(),
                plans: RefCell::default(),
                woken: RefCell::default(),
                spend_dropped: RefCell::default(),
                refuse_tile: Cell::new(false),
                _dir: dir,
            }
        }
    }

    impl SourceSaveEnv for Fake {
        fn store(&self) -> &Mutex<Store> {
            &self.store
        }
        fn credentials(&self) -> &dyn CredentialStore {
            &self.credentials
        }
        fn tile_added(
            &self,
            current: &DashboardLayout,
            source: &str,
        ) -> Result<Option<DashboardLayout>, String> {
            if self.refuse_tile.get() {
                return Err("full".to_owned());
            }
            if current.tiles.iter().any(|tile| tile.source == source) {
                return Ok(None);
            }
            let mut next = current.clone();
            next.tiles.push(crate::DashboardTile::new(
                format!("added-{source}-1"),
                source,
                source,
                "all",
            ));
            Ok(Some(next))
        }
        fn persist_dashboard(
            &self,
            store: &mut Store,
            mut layout: DashboardLayout,
            _expected_revision: u64,
        ) -> Result<(), String> {
            layout.revision += 1;
            store
                .save_dashboard(layout)
                .map_err(|error| format!("Could not save the dashboard: {error}"))
        }
        fn plan_enabled(&self, provider: PlanProvider) -> bool {
            self.plans
                .borrow()
                .iter()
                .rev()
                .find(|(p, _)| *p == provider)
                .is_some_and(|(_, on)| *on)
        }
        fn set_plan(&self, provider: PlanProvider, enabled: bool) -> Option<String> {
            self.plans.borrow_mut().push((provider, enabled));
            None
        }
        fn unconfigure_spend(&self, spend: SpendFigure) {
            self.spend_dropped.borrow_mut().push(spend);
        }
        fn wake(&self, wake: SourceWake) {
            self.woken.borrow_mut().push(wake);
        }
    }

    fn parts(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn every_kind_is_listed_once_and_found_by_name() {
        let kinds: Vec<_> = SOURCES.iter().map(|spec| spec.kind).collect();
        assert_eq!(kinds, ["anthropic", "openai", "local", "services"]);
        for kind in kinds {
            assert_eq!(spec(kind).map(|spec| spec.kind), Some(kind));
        }
        assert!(spec("not-a-source").is_none());
    }

    #[test]
    fn an_unknown_source_is_skipped_before_anything_else() {
        let env = Fake::new();
        let refusal = save_source(&env, "not-a-source", &parts(&["api"]), Some("k"), true);
        assert_eq!(refusal.unwrap_err(), "Skipped \u{2014} unknown source.");
        assert!(env.woken.borrow().is_empty());
    }

    #[test]
    fn a_walk_saves_the_flag_and_the_tile_and_wakes_its_loop() {
        let env = Fake::new();
        let status = save_source(&env, "anthropic", &parts(&["local"]), None, true).unwrap();
        assert_eq!(status, "Anthropic saved.");
        let store = env.store.lock().unwrap();
        assert!(store.settings().claude_local_logs_enabled);
        assert!(store
            .dashboard()
            .unwrap()
            .tiles
            .iter()
            .any(|tile| tile.source == "anthropic"));
        drop(store);
        assert_eq!(*env.woken.borrow(), [SourceWake::UsageEverything]);
    }

    #[test]
    fn a_refused_tile_changes_nothing() {
        let env = Fake::new();
        env.refuse_tile.set(true);
        let refusal = save_source(&env, "anthropic", &parts(&["subscription"]), None, true);
        assert_eq!(refusal.unwrap_err(), "full");
        assert!(env.plans.borrow().is_empty());
        assert!(env.woken.borrow().is_empty());
    }

    #[test]
    fn the_api_part_with_no_key_is_refused_before_a_write() {
        let env = Fake::new();
        let refusal = save_source(&env, "openai", &parts(&["api"]), Some("  "), false);
        assert_eq!(
            refusal.unwrap_err(),
            "Enter an Admin API key, or untick API key."
        );
        assert!(env
            .credentials
            .secret(SecretKey::OpenAiAdminKey)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_key_is_stored_and_removing_the_source_drops_it_and_its_figure() {
        let env = Fake::new();
        save_source(&env, "openai", &parts(&["api"]), Some(" sk-x "), false).unwrap();
        assert_eq!(
            env.credentials
                .secret(SecretKey::OpenAiAdminKey)
                .unwrap()
                .as_deref(),
            Some("sk-x")
        );
        let status = save_source(&env, "openai", &parts(&[]), None, false).unwrap();
        assert_eq!(status, "OpenAI removed.");
        assert!(env
            .credentials
            .secret(SecretKey::OpenAiAdminKey)
            .unwrap()
            .is_none());
        assert_eq!(*env.spend_dropped.borrow(), [SpendFigure::OpenAi]);
    }

    #[test]
    fn the_plan_toggles_only_on_a_change() {
        let env = Fake::new();
        save_source(&env, "anthropic", &parts(&["subscription"]), None, false).unwrap();
        save_source(&env, "anthropic", &parts(&["subscription"]), None, false).unwrap();
        assert_eq!(*env.plans.borrow(), [(PlanProvider::Claude, true)]);
    }

    #[test]
    fn a_failed_save_puts_the_flag_back() {
        let env = Fake::new();
        // A directory where the file goes makes the write fail.
        let file = env.store.lock().unwrap().path().to_owned();
        std::fs::remove_file(&file).ok();
        std::fs::create_dir_all(&file).unwrap();
        let refusal = save_source(&env, "services", &parts(&["services"]), None, false);
        assert!(refusal.unwrap_err().starts_with("Failed"));
        assert!(!env.store.lock().unwrap().settings().service_health_enabled);
        assert!(env.woken.borrow().is_empty());
    }
}
