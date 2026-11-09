//! What a source's guided steps apply when the operator presses Finish: the
//! table of sources, every refusal, the credential write, the plan toggle and
//! the one flag, input and tile save with its rollback.
//!
//! This is the logic behind the shell's `settings_save_source` command, so
//! each source #721 puts on the steps starts as a row in [`SOURCES`] and is
//! testable without the shell. A row describes its parts as data (a key, a
//! plan, a flag, typed non-secret inputs); [`SOURCES`] says what a row still
//! cannot hold. What only the shell can do comes in
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
/// Neon's database usage and estimated charges.
pub const NEON_TITLE: &str = "Neon";
/// Vercel's billing usage.
pub const VERCEL_TITLE: &str = "Vercel";
/// Sentry's error usage and its scheduled-job monitors.
pub const SENTRY_TITLE: &str = "Sentry";

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
    /// The metered providers and the Sentry Crons loop, which read one token.
    UsageProvidersAndCrons,
    /// The containers loop.
    Containers,
    /// Nothing besides the status pages.
    StatusPagesOnly,
}

/// The credential a part of a source holds, the sentences that refuse it, and
/// the spend figure it feeds, if any.
///
/// Several parts of one source may name the same `secret` (Sentry's token feeds
/// Usage and Scheduled jobs). It is then one key: asked once, kept while any of
/// those parts is ticked, and deleted only when none is. Their sentences and
/// spend figure must agree; the first such part's are the ones used.
#[derive(Debug, Clone, Copy)]
pub struct PartKey {
    pub secret: SecretKey,
    /// Refuses a ticked part that has no key typed and none stored.
    pub missing: &'static str,
    /// Refuses unticking a part whose stored key cannot be read, because a store
    /// that won't answer can't say whether there is anything to delete.
    pub unreadable: &'static str,
    /// The in-memory figure that goes with the key when it is removed.
    pub spend: Option<SpendFigure>,
}

/// Writes one typed, non-secret value into the settings. `Err` is the sentence
/// refusing the value, and it is raised before anything is written.
type ApplyInput = fn(&mut Settings, &str) -> Result<(), &'static str>;

/// A non-secret value a part writes at Finish, such as an organization id.
///
/// The value arrives trimmed. An empty value is the empty setting, and a part
/// left unticked writes empty too, so removing a source forgets what it was
/// told. A value the walk did not send is left as it is.
#[derive(Clone, Copy)]
pub struct SourceInput {
    pub id: &'static str,
    /// A number rather than text, and kept behind Advanced options. How the
    /// steps draw the field; the catalog is tested against them so a drawn
    /// field cannot drift from the input that saves it.
    pub number: bool,
    pub advanced: bool,
    apply: ApplyInput,
}

/// One step of a source's walk: the part's id, as the frontend names it, and
/// what ticking it sets. Every field is optional, so a part is any mix of a key,
/// a plan opt-in, a flag and typed values.
#[derive(Clone, Copy)]
pub struct SourcePart {
    pub id: &'static str,
    pub key: Option<PartKey>,
    pub plan: Option<PlanProvider>,
    /// The flag this part sets: on when ticked, off when not.
    flag: Option<fn(&mut Settings) -> &mut bool>,
    pub inputs: &'static [SourceInput],
    /// The dashboard tile source this part's tile is added under, when it is not
    /// the source's own [`SourceSpec::tile_source`] (Sentry's Scheduled jobs).
    pub tile_source: Option<&'static str>,
}

impl SourcePart {
    const fn new(id: &'static str) -> Self {
        Self {
            id,
            key: None,
            plan: None,
            flag: None,
            inputs: &[],
            tile_source: None,
        }
    }
}

/// One source the steps can add, change or remove.
pub struct SourceSpec {
    pub kind: &'static str,
    pub title: &'static str,
    /// The dashboard tile source its tile is added under, unless a part names
    /// its own.
    pub tile_source: &'static str,
    pub parts: &'static [SourcePart],
    pub wake: SourceWake,
}

const ADMIN_KEY_MISSING: &str = "Enter an Admin API key, or untick API key.";
const ADMIN_KEY_UNREADABLE: &str = "Couldn't read the credential store, so a stored API key \
    can't be removed. Nothing changed.";

const NO_RATE: &str = "Enter a rate of zero or more.";
const NO_QUOTA: &str = "Enter a whole number of events, or leave the quota empty.";

/// Sentry's one token, named by both of its parts.
const SENTRY_KEY: PartKey = PartKey {
    secret: SecretKey::SentryUsageToken,
    missing: "Enter a Sentry API token.",
    unreadable: "Couldn't read the credential store, so a stored Sentry token can't be \
        removed. Nothing changed.",
    spend: None,
};

/// Sentry's organization slug, which both of its parts read.
const SENTRY_SLUG: SourceInput = SourceInput {
    id: "orgSlug",
    number: false,
    advanced: false,
    apply: |settings, value| {
        value.clone_into(&mut settings.sentry_org_slug);
        Ok(())
    },
};

/// A monthly event quota: empty is zero (no quota, which hides the bar), and
/// anything else must be a whole number.
fn quota(value: &str) -> Result<u64, &'static str> {
    if value.is_empty() {
        return Ok(0);
    }
    value.parse::<u64>().map_err(|_| NO_QUOTA)
}

/// A rate in dollars: empty is zero (the shipped default, which hides the row),
/// and anything else must be a finite number of zero or more.
fn rate(value: &str) -> Result<f64, &'static str> {
    if value.is_empty() {
        return Ok(0.0);
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|rate| rate.is_finite() && *rate >= 0.0)
        .ok_or(NO_RATE)
}

/// Every source the steps know.
///
/// A row is a source's whole description, as parts: each part names its id, and
/// may hold a key (with its refusal sentences and the spend figure it feeds), a
/// plan opt-in, a flag and typed non-secret inputs. [`save_source_with`] reads
/// nothing else. Parts may share a key (Sentry's token feeds Usage and
/// Scheduled jobs) or an input id (its organization slug): a shared one is
/// kept while any part that names it is ticked, and forgotten only when none
/// is. What a later source still has to add is a new wake, spend or plan
/// variant, which needs an arm in the shell's `match` on it, and anything the
/// shape cannot say: several different credentials in one source (GitHub's
/// per-account tokens), since the typed secret goes to the source's first keyed
/// part, and an input a part must have (Azure's), since an input is never
/// required.
pub const SOURCES: [SourceSpec; 7] = [
    SourceSpec {
        kind: "anthropic",
        title: ANTHROPIC_TITLE,
        tile_source: "anthropic",
        parts: &[
            SourcePart {
                key: Some(PartKey {
                    secret: SecretKey::AnthropicAdminKey,
                    missing: ADMIN_KEY_MISSING,
                    unreadable: ADMIN_KEY_UNREADABLE,
                    spend: Some(SpendFigure::Anthropic),
                }),
                ..SourcePart::new("api")
            },
            SourcePart {
                plan: Some(PlanProvider::Claude),
                ..SourcePart::new("subscription")
            },
            SourcePart {
                flag: Some(|settings| &mut settings.claude_local_logs_enabled),
                ..SourcePart::new("local")
            },
        ],
        wake: SourceWake::UsageEverything,
    },
    SourceSpec {
        kind: "openai",
        title: OPENAI_TITLE,
        tile_source: "openai",
        parts: &[
            SourcePart {
                key: Some(PartKey {
                    secret: SecretKey::OpenAiAdminKey,
                    missing: ADMIN_KEY_MISSING,
                    unreadable: ADMIN_KEY_UNREADABLE,
                    spend: Some(SpendFigure::OpenAi),
                }),
                ..SourcePart::new("api")
            },
            SourcePart {
                plan: Some(PlanProvider::Codex),
                ..SourcePart::new("subscription")
            },
        ],
        wake: SourceWake::UsageProviders,
    },
    SourceSpec {
        kind: "local",
        title: LOCAL_TITLE,
        tile_source: "hosts",
        parts: &[SourcePart {
            flag: Some(|settings| &mut settings.local_monitoring_enabled),
            ..SourcePart::new("local")
        }],
        wake: SourceWake::Containers,
    },
    SourceSpec {
        kind: "services",
        title: SERVICES_TITLE,
        tile_source: "services",
        parts: &[SourcePart {
            flag: Some(|settings| &mut settings.service_health_enabled),
            ..SourcePart::new("services")
        }],
        wake: SourceWake::StatusPagesOnly,
    },
    SourceSpec {
        kind: "neon",
        title: NEON_TITLE,
        tile_source: "usage",
        parts: &[SourcePart {
            key: Some(PartKey {
                secret: SecretKey::NeonApiKey,
                missing: "Enter an organization API key.",
                unreadable: "Couldn't read the credential store, so a stored organization API \
                    key can't be removed. Nothing changed.",
                spend: None,
            }),
            inputs: &[
                SourceInput {
                    id: "orgId",
                    number: false,
                    advanced: false,
                    apply: |settings, value| {
                        value.clone_into(&mut settings.neon_org_id);
                        Ok(())
                    },
                },
                SourceInput {
                    id: "usdPerCuHour",
                    number: true,
                    advanced: true,
                    apply: |settings, value| {
                        settings.neon_usd_per_cu_hour = rate(value)?;
                        Ok(())
                    },
                },
                SourceInput {
                    id: "usdPerGibMonth",
                    number: true,
                    advanced: true,
                    apply: |settings, value| {
                        settings.neon_usd_per_gib_month = rate(value)?;
                        Ok(())
                    },
                },
            ],
            ..SourcePart::new("neon")
        }],
        wake: SourceWake::UsageProviders,
    },
    SourceSpec {
        kind: "vercel",
        title: VERCEL_TITLE,
        tile_source: "usage",
        parts: &[SourcePart {
            key: Some(PartKey {
                secret: SecretKey::VercelApiToken,
                missing: "Enter an API token.",
                unreadable: "Couldn't read the credential store, so a stored API token can't \
                    be removed. Nothing changed.",
                spend: None,
            }),
            inputs: &[SourceInput {
                id: "teamId",
                number: false,
                advanced: false,
                apply: |settings, value| {
                    value.clone_into(&mut settings.vercel_team_id);
                    Ok(())
                },
            }],
            ..SourcePart::new("vercel")
        }],
        wake: SourceWake::UsageProviders,
    },
    SourceSpec {
        kind: "sentry",
        title: SENTRY_TITLE,
        tile_source: "usage",
        parts: &[
            SourcePart {
                key: Some(SENTRY_KEY),
                flag: Some(|settings| &mut settings.sentry_usage_enabled),
                inputs: &[
                    SENTRY_SLUG,
                    SourceInput {
                        id: "quota",
                        number: true,
                        advanced: true,
                        apply: |settings, value| {
                            settings.sentry_monthly_event_quota = quota(value)?;
                            Ok(())
                        },
                    },
                ],
                ..SourcePart::new("usage")
            },
            SourcePart {
                key: Some(SENTRY_KEY),
                flag: Some(|settings| &mut settings.sentry_crons_enabled),
                inputs: &[SENTRY_SLUG],
                tile_source: Some("sentryCrons"),
                ..SourcePart::new("crons")
            },
        ],
        wake: SourceWake::UsageProvidersAndCrons,
    },
];

/// The source named `kind`, or `None` for one the steps do not know.
#[must_use]
pub fn spec(kind: &str) -> Option<&'static SourceSpec> {
    SOURCES.iter().find(|spec| spec.kind == kind)
}

/// What a save needs from the shell.
pub trait SourceSaveEnv {
    /// The store. The save takes its lock itself. [`Self::tile_added`] and
    /// [`Self::persist_dashboard`] run under it (the latter is handed the
    /// locked store), so neither may touch the store; every other method here
    /// is called with the lock free.
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

/// The tile sources a save adds tiles for: each ticked part's own, or the
/// source's, once each.
fn tile_sources<'a>(spec: &'a SourceSpec, wants: &dyn Fn(&str) -> bool) -> Vec<&'a str> {
    let mut sources: Vec<&str> = Vec::new();
    for part in spec.parts.iter().filter(|part| wants(part.id)) {
        let source = part.tile_source.unwrap_or(spec.tile_source);
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    // A part id the table does not know still asks for the source's tile.
    if sources.is_empty() {
        sources.push(spec.tile_source);
    }
    sources
}

/// The layout with a tile of each of `sources` in it, and the revision it was
/// read at, or `None` when every one is already there. Everything
/// `persist_dashboard` can refuse is refused here, so a caller can ask before it
/// writes anything.
fn layout_with_tiles(
    env: &dyn SourceSaveEnv,
    store: &Store,
    sources: &[&str],
) -> Result<Option<(DashboardLayout, u64)>, String> {
    let current = store
        .dashboard()
        .cloned()
        .unwrap_or_else(DashboardLayout::shipped);
    let mut next: Option<DashboardLayout> = None;
    for source in sources {
        if let Some(added) = env.tile_added(next.as_ref().unwrap_or(&current), source)? {
            next = Some(added);
        }
    }
    Ok(next.map(|next| (next, current.revision)))
}

/// Every typed input the parts a save leaves ticked were sent, written into
/// `settings`; an unticked part writes empty, and an input not sent is left as
/// it is.
///
/// # Errors
/// The sentence refusing a value, or an input no part of the source declares.
fn apply_inputs(
    spec: &SourceSpec,
    wants: &dyn Fn(&str) -> bool,
    inputs: &[(String, String)],
    settings: &mut Settings,
) -> Result<(), &'static str> {
    let declared = |id: &str| {
        spec.parts
            .iter()
            .any(|part| part.inputs.iter().any(|input| input.id == id))
    };
    if inputs.iter().any(|(id, _)| !declared(id)) {
        return Err("Skipped \u{2014} unknown field.");
    }
    // An input several parts name is one value: written while any of them is
    // ticked, and forgotten only when none is.
    let mut done: Vec<&str> = Vec::new();
    for part in spec.parts {
        for input in part.inputs {
            if done.contains(&input.id) {
                continue;
            }
            done.push(input.id);
            let ticked = spec
                .parts
                .iter()
                .any(|other| wants(other.id) && other.inputs.iter().any(|i| i.id == input.id));
            let sent = inputs.iter().find(|(id, _)| id == input.id);
            let value = match (ticked, sent) {
                (true, Some((_, value))) => value.trim(),
                (true, None) => continue,
                (false, _) => "",
            };
            (input.apply)(settings, value)?;
        }
    }
    Ok(())
}

/// [`save_source_with`] for a source that is sent no typed input.
///
/// # Errors
/// As [`save_source_with`].
pub fn save_source(
    env: &dyn SourceSaveEnv,
    kind: &str,
    parts: &[String],
    secret: Option<&str>,
    add_tile: bool,
) -> Result<String, String> {
    save_source_with(env, kind, parts, secret, &[], add_tile)
}

/// Everything a source's steps apply, all at once on Finish.
///
/// `Err` is the sentence to show, and the steps stay open with the operator's
/// choices. Every refusal is checked before the first write, so it changes
/// nothing. Only a failure can follow a write: an I/O error, or a dashboard edit
/// that filled the last slot while Finish ran, because the tile is derived again
/// inside the save's lock. Finish is safe to repeat after either: the flags, the
/// typed inputs and the tile go back, and a key or plan already written stays.
///
/// `secret` is the key the operator typed, if any, for the source's first keyed
/// part. It goes straight to the credential store and is never logged.
/// `inputs` are the non-secret values typed on the parts, as `(input id, value)`;
/// they are checked first, like every refusal, and written under the same lock
/// and in the same save as the flags and the tile.
///
/// # Errors
/// The refusal or failure sentence, as above.
///
/// # Panics
/// If the store's lock is poisoned.
pub fn save_source_with(
    env: &dyn SourceSaveEnv,
    kind: &str,
    parts: &[String],
    secret: Option<&str>,
    inputs: &[(String, String)],
    add_tile: bool,
) -> Result<String, String> {
    let wants = |part: &str| parts.iter().any(|id| id == part);
    let Some(spec) = spec(kind) else {
        return Err("Skipped \u{2014} unknown source.".to_owned());
    };

    // Whatever can refuse goes first.
    let secret = secret.map(str::trim).filter(|value| !value.is_empty());
    // Each distinct key, its credential as read now, the key typed for it, and
    // whether any part that uses it is ticked. Parts that share a key are one
    // entry, so unticking one of them cannot delete what another still needs.
    let mut keyed: Vec<(PartKey, Credential, Option<&str>, bool)> = Vec::new();
    for key in spec.parts.iter().filter_map(|part| part.key) {
        if keyed.iter().any(|(seen, ..)| seen.secret == key.secret) {
            continue;
        }
        let ticked = spec
            .parts
            .iter()
            .any(|part| wants(part.id) && part.key.is_some_and(|k| k.secret == key.secret));
        let credential = read_credential(env.credentials(), key.secret);
        let typed = secret.filter(|_| keyed.is_empty());
        keyed.push((key, credential, typed, ticked));
    }
    for (key, credential, typed, ticked) in &keyed {
        if *ticked && typed.is_none() && matches!(credential, Credential::Absent) {
            return Err(key.missing.to_owned());
        }
    }
    // Unticked means delete, and a store that won't answer can't say whether
    // there is anything to delete. "Removed" would be a guess.
    for (key, credential, _, ticked) in &keyed {
        if !*ticked && matches!(credential, Credential::Unreadable) {
            return Err(key.unreadable.to_owned());
        }
    }
    {
        // A scratch copy takes the values first, so a refused one changes nothing.
        let mut scratch = env
            .store()
            .lock()
            .expect("store poisoned")
            .settings()
            .clone();
        apply_inputs(spec, &wants, inputs, &mut scratch).map_err(str::to_owned)?;
    }
    let wants_tile = add_tile && !parts.is_empty();
    let tiles = tile_sources(spec, &wants);
    if wants_tile {
        layout_with_tiles(env, &env.store().lock().expect("store poisoned"), &tiles)?;
    }

    for (key, credential, typed, ticked) in &keyed {
        let stored = matches!(credential, Credential::Present(_));
        let written = match (ticked, typed) {
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
        if let (true, false, Some(spend)) = (stored, ticked, key.spend) {
            env.unconfigure_spend(spend);
        }
    }

    // Only on a change: a toggle resets the provider, which would un-park a
    // refused keychain and re-read a sign-in because a walk was reopened.
    for part in spec.parts {
        if let Some(provider) = part.plan {
            if env.plan_enabled(provider) != wants(part.id) {
                let status = env.set_plan(provider, wants(part.id));
                if let Some(failed) = status.filter(|status| status.starts_with("Failed")) {
                    return Err(failed);
                }
            }
        }
    }

    // One lock and one save, with the tile derived inside it: no dashboard edit
    // can land between the read and the write.
    {
        let mut store = env.store().lock().expect("store poisoned");
        // Taken under the lock and put back under it, so nothing else is undone.
        let before = store.settings().clone();
        for part in spec.parts {
            if let Some(flag) = part.flag {
                *flag(store.settings_mut()) = wants(part.id);
            }
        }
        let applied =
            apply_inputs(spec, &wants, inputs, store.settings_mut()).map_err(str::to_owned);
        let tile = if wants_tile {
            layout_with_tiles(env, &store, &tiles)
        } else {
            Ok(None)
        };
        let written = applied.and(tile).and_then(|tile| match tile {
            Some((next, revision)) => env.persist_dashboard(&mut store, next, revision),
            None => store.save().map_err(|error| format!("Failed: {error}")),
        });
        if let Err(failed) = written {
            *store.settings_mut() = before;
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
    use crate::{MemoryCredentialStore, SecretError};
    use std::cell::{Cell, RefCell};

    /// A credential store that can be told to refuse a read or a write.
    #[derive(Default)]
    struct Flaky {
        inner: MemoryCredentialStore,
        fail_read: Cell<bool>,
        fail_write: Cell<bool>,
    }

    impl Flaky {
        fn refusal(key: SecretKey) -> SecretError {
            SecretError::CorruptBlob {
                account: key.account(),
            }
        }
    }

    impl CredentialStore for Flaky {
        fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
            if self.fail_read.get() {
                return Err(Self::refusal(key));
            }
            self.inner.secret(key)
        }
        fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
            if self.fail_write.get() {
                return Err(Self::refusal(key));
            }
            self.inner.set_secret(key, value)
        }
        fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError> {
            if self.fail_write.get() {
                return Err(Self::refusal(key));
            }
            self.inner.delete_secret(key)
        }
        fn secret_bytes(&self, key: SecretKey) -> Result<Option<Vec<u8>>, SecretError> {
            self.inner.secret_bytes(key)
        }
        fn set_secret_bytes(&self, key: SecretKey, value: &[u8]) -> Result<(), SecretError> {
            self.inner.set_secret_bytes(key, value)
        }
    }

    /// A shell with nothing in it: plan toggles and wakes are recorded, and the
    /// dashboard rules accept any tile unless told otherwise.
    struct Fake {
        store: Mutex<Store>,
        credentials: Flaky,
        plans: RefCell<Vec<(PlanProvider, bool)>>,
        woken: RefCell<Vec<SourceWake>>,
        spend_dropped: RefCell<Vec<SpendFigure>>,
        refuse_tile: Cell<bool>,
        /// Every tile source the save asked the dashboard rules about.
        tiles_asked: RefCell<Vec<String>>,
        _dir: tempfile::TempDir,
    }

    impl Fake {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open_in(dir.path(), false).unwrap();
            Self {
                store: Mutex::new(store),
                credentials: Flaky::default(),
                plans: RefCell::default(),
                woken: RefCell::default(),
                spend_dropped: RefCell::default(),
                refuse_tile: Cell::new(false),
                tiles_asked: RefCell::default(),
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
            let mut asked = self.tiles_asked.borrow_mut();
            if !asked.iter().any(|seen| seen == source) {
                asked.push(source.to_owned());
            }
            drop(asked);
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
        assert_eq!(
            kinds,
            [
                "anthropic",
                "openai",
                "local",
                "services",
                "neon",
                "vercel",
                "sentry"
            ]
        );
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

    #[test]
    fn an_unreadable_credential_store_refuses_a_removal_and_changes_nothing() {
        let env = Fake::new();
        env.credentials.fail_read.set(true);
        let refusal = save_source(&env, "openai", &parts(&[]), None, true);
        assert_eq!(
            refusal.unwrap_err(),
            "Couldn't read the credential store, so a stored API key can't be removed. \
             Nothing changed."
        );
        env.credentials.fail_read.set(false);
        assert!(env
            .credentials
            .secret(SecretKey::OpenAiAdminKey)
            .unwrap()
            .is_none());
        assert!(env.store.lock().unwrap().dashboard().is_none());
        assert!(env.plans.borrow().is_empty());
        assert!(env.spend_dropped.borrow().is_empty());
        assert!(env.woken.borrow().is_empty());
    }

    #[test]
    fn a_credential_write_the_store_refuses_answers_failed_and_changes_nothing_else() {
        let env = Fake::new();
        env.credentials.fail_write.set(true);
        let refusal = save_source(&env, "openai", &parts(&["api"]), Some("sk-x"), true);
        assert_eq!(
            refusal.unwrap_err(),
            "Failed to save \u{2014} the credential store rejected the write."
        );
        env.credentials.fail_write.set(false);
        assert!(env
            .credentials
            .secret(SecretKey::OpenAiAdminKey)
            .unwrap()
            .is_none());
        assert!(env.store.lock().unwrap().dashboard().is_none());
        assert!(
            !env.store
                .lock()
                .unwrap()
                .settings()
                .claude_local_logs_enabled
        );
        assert!(env.plans.borrow().is_empty());
        assert!(env.woken.borrow().is_empty());
    }

    #[test]
    fn a_credential_delete_the_store_refuses_keeps_the_key_and_its_figure() {
        let env = Fake::new();
        env.credentials
            .set_secret(SecretKey::OpenAiAdminKey, "sk-x")
            .unwrap();
        env.credentials.fail_write.set(true);
        let refusal = save_source(&env, "openai", &parts(&[]), None, false);
        assert_eq!(
            refusal.unwrap_err(),
            "Failed to save \u{2014} the credential store rejected the write."
        );
        env.credentials.fail_write.set(false);
        assert!(env
            .credentials
            .secret(SecretKey::OpenAiAdminKey)
            .unwrap()
            .is_some());
        assert!(env.spend_dropped.borrow().is_empty());
        assert!(env.woken.borrow().is_empty());
    }

    fn typed(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(id, value)| ((*id).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn a_walk_with_typed_inputs_writes_the_key_and_the_values_only_at_finish() {
        let env = Fake::new();
        let status = save_source_with(
            &env,
            "neon",
            &parts(&["neon"]),
            Some(" napi_x "),
            &typed(&[
                ("orgId", " org-1 "),
                ("usdPerCuHour", "0.106"),
                ("usdPerGibMonth", ""),
            ]),
            true,
        )
        .unwrap();
        assert_eq!(status, "Neon saved.");
        assert_eq!(
            env.credentials
                .secret(SecretKey::NeonApiKey)
                .unwrap()
                .as_deref(),
            Some("napi_x")
        );
        let store = env.store.lock().unwrap();
        assert_eq!(store.settings().neon_org_id, "org-1");
        assert!((store.settings().neon_usd_per_cu_hour - 0.106).abs() < f64::EPSILON);
        assert!(store.settings().neon_usd_per_gib_month.abs() < f64::EPSILON);
        assert!(store
            .dashboard()
            .unwrap()
            .tiles
            .iter()
            .any(|tile| tile.source == "usage"));
        drop(store);
        assert_eq!(*env.woken.borrow(), [SourceWake::UsageProviders]);
    }

    #[test]
    fn a_ticked_part_with_no_key_is_refused_in_its_own_words_and_writes_no_value() {
        for (kind, part, input, sentence) in [
            ("neon", "neon", "orgId", "Enter an organization API key."),
            ("vercel", "vercel", "teamId", "Enter an API token."),
        ] {
            let env = Fake::new();
            let refusal = save_source_with(
                &env,
                kind,
                &parts(&[part]),
                Some("  "),
                &typed(&[(input, "typed")]),
                true,
            );
            assert_eq!(refusal.unwrap_err(), sentence, "{kind}");
            let store = env.store.lock().unwrap();
            assert!(store.settings().neon_org_id.is_empty(), "{kind}");
            assert!(store.settings().vercel_team_id.is_empty(), "{kind}");
            assert!(store.dashboard().is_none(), "{kind}");
            assert!(env.woken.borrow().is_empty(), "{kind}");
        }
    }

    #[test]
    fn a_refused_value_or_unknown_field_changes_nothing_not_even_the_key() {
        for (input, value, sentence) in [
            ("usdPerCuHour", "-1", "Enter a rate of zero or more."),
            ("usdPerGibMonth", "lots", "Enter a rate of zero or more."),
            ("usdPerCuHour", "inf", "Enter a rate of zero or more."),
            ("teamId", "team", "Skipped \u{2014} unknown field."),
        ] {
            let env = Fake::new();
            let refusal = save_source_with(
                &env,
                "neon",
                &parts(&["neon"]),
                Some("napi_x"),
                &typed(&[("orgId", "org-1"), (input, value)]),
                true,
            );
            assert_eq!(refusal.unwrap_err(), sentence, "{input}={value}");
            assert!(env
                .credentials
                .secret(SecretKey::NeonApiKey)
                .unwrap()
                .is_none());
            let store = env.store.lock().unwrap();
            assert!(store.settings().neon_org_id.is_empty());
            assert!(store.dashboard().is_none());
            assert!(env.woken.borrow().is_empty());
        }
    }

    #[test]
    fn a_value_not_sent_is_kept_and_removing_the_source_forgets_every_value_and_the_key() {
        let env = Fake::new();
        save_source_with(
            &env,
            "vercel",
            &parts(&["vercel"]),
            Some("tok"),
            &typed(&[("teamId", "team_1")]),
            false,
        )
        .unwrap();
        // Reopened and left alone: the key stays and so does the team.
        save_source_with(&env, "vercel", &parts(&["vercel"]), None, &[], false).unwrap();
        assert_eq!(
            env.store.lock().unwrap().settings().vercel_team_id,
            "team_1"
        );
        assert!(env
            .credentials
            .secret(SecretKey::VercelApiToken)
            .unwrap()
            .is_some());

        let status = save_source_with(&env, "vercel", &parts(&[]), None, &[], true).unwrap();
        assert_eq!(status, "Vercel removed.");
        assert!(env
            .store
            .lock()
            .unwrap()
            .settings()
            .vercel_team_id
            .is_empty());
        assert!(env
            .credentials
            .secret(SecretKey::VercelApiToken)
            .unwrap()
            .is_none());
        // Vercel's token has no spend figure to drop.
        assert!(env.spend_dropped.borrow().is_empty());
    }

    #[test]
    fn a_failed_save_puts_the_typed_values_back() {
        let env = Fake::new();
        env.store.lock().unwrap().settings_mut().neon_org_id = "org-before".to_owned();
        // A directory where the file goes makes the write fail.
        let file = env.store.lock().unwrap().path().to_owned();
        std::fs::remove_file(&file).ok();
        std::fs::create_dir_all(&file).unwrap();
        let refusal = save_source_with(
            &env,
            "neon",
            &parts(&["neon"]),
            Some("napi_x"),
            &typed(&[("orgId", "org-after"), ("usdPerCuHour", "1")]),
            false,
        );
        assert!(refusal.unwrap_err().starts_with("Failed"));
        let store = env.store.lock().unwrap();
        assert_eq!(store.settings().neon_org_id, "org-before");
        assert!(store.settings().neon_usd_per_cu_hour.abs() < f64::EPSILON);
        assert!(env.woken.borrow().is_empty());
    }

    #[test]
    fn an_unreadable_credential_store_refuses_removing_neon_or_vercel_in_its_own_words() {
        for (kind, sentence) in [
            (
                "neon",
                "Couldn't read the credential store, so a stored organization API key can't \
                 be removed. Nothing changed.",
            ),
            (
                "vercel",
                "Couldn't read the credential store, so a stored API token can't be removed. \
                 Nothing changed.",
            ),
        ] {
            let env = Fake::new();
            env.credentials.fail_read.set(true);
            let refusal = save_source_with(&env, kind, &parts(&[]), None, &[], true);
            assert_eq!(refusal.unwrap_err(), sentence, "{kind}");
            assert!(env.store.lock().unwrap().dashboard().is_none());
            assert!(env.woken.borrow().is_empty());
        }
    }

    #[test]
    fn removing_neon_forgets_its_rates_and_org_and_an_unreadable_store_keeps_a_ticked_part() {
        let env = Fake::new();
        save_source_with(
            &env,
            "neon",
            &parts(&["neon"]),
            Some("napi_x"),
            &typed(&[
                ("orgId", "org-1"),
                ("usdPerCuHour", "0.106"),
                ("usdPerGibMonth", "0.35"),
            ]),
            false,
        )
        .unwrap();
        // A store that won't answer is not "no key": a ticked part needs none typed.
        env.credentials.fail_read.set(true);
        save_source_with(&env, "neon", &parts(&["neon"]), None, &[], false).unwrap();
        env.credentials.fail_read.set(false);

        save_source_with(&env, "neon", &parts(&[]), None, &[], false).unwrap();
        let store = env.store.lock().unwrap();
        assert!(store.settings().neon_org_id.is_empty());
        assert!(store.settings().neon_usd_per_cu_hour.abs() < f64::EPSILON);
        assert!(store.settings().neon_usd_per_gib_month.abs() < f64::EPSILON);
    }

    fn sentry_token(env: &Fake) -> Option<String> {
        env.credentials.secret(SecretKey::SentryUsageToken).unwrap()
    }

    fn sentry_flags(env: &Fake) -> (bool, bool) {
        let store = env.store.lock().unwrap();
        (
            store.settings().sentry_usage_enabled,
            store.settings().sentry_crons_enabled,
        )
    }

    /// Both parts ticked: one token, one slug and the quota, written once.
    fn sentry_with_both(env: &Fake) {
        let status = save_source_with(
            env,
            "sentry",
            &parts(&["usage", "crons"]),
            Some(" sntrys_x "),
            &typed(&[("orgSlug", " acme "), ("quota", "50000")]),
            true,
        )
        .unwrap();
        assert_eq!(status, "Sentry saved.");
    }

    #[test]
    fn sentry_takes_one_token_and_one_slug_for_both_parts_and_wakes_both_loops() {
        let env = Fake::new();
        sentry_with_both(&env);
        assert_eq!(sentry_token(&env).as_deref(), Some("sntrys_x"));
        assert_eq!(sentry_flags(&env), (true, true));
        let store = env.store.lock().unwrap();
        assert_eq!(store.settings().sentry_org_slug, "acme");
        assert_eq!(store.settings().sentry_monthly_event_quota, 50_000);
        drop(store);
        assert_eq!(*env.woken.borrow(), [SourceWake::UsageProvidersAndCrons]);
    }

    #[test]
    fn unticking_usage_keeps_the_token_and_slug_for_crons_and_forgets_the_quota() {
        let env = Fake::new();
        sentry_with_both(&env);
        save_source_with(
            &env,
            "sentry",
            &parts(&["crons"]),
            None,
            &typed(&[("orgSlug", "acme")]),
            false,
        )
        .unwrap();
        assert_eq!(
            sentry_token(&env).as_deref(),
            Some("sntrys_x"),
            "crons still needs it"
        );
        assert_eq!(sentry_flags(&env), (false, true));
        let store = env.store.lock().unwrap();
        assert_eq!(store.settings().sentry_org_slug, "acme");
        assert_eq!(store.settings().sentry_monthly_event_quota, 0);
        drop(store);

        // Then the last part: nothing uses the token now.
        save_source_with(&env, "sentry", &parts(&[]), None, &[], false).unwrap();
        assert_eq!(sentry_token(&env), None);
        assert_eq!(sentry_flags(&env), (false, false));
        assert!(env
            .store
            .lock()
            .unwrap()
            .settings()
            .sentry_org_slug
            .is_empty());
    }

    #[test]
    fn unticking_crons_keeps_the_token_slug_and_quota_for_usage_and_then_the_last_part_deletes_it()
    {
        let env = Fake::new();
        sentry_with_both(&env);
        save_source_with(
            &env,
            "sentry",
            &parts(&["usage"]),
            None,
            &typed(&[("orgSlug", "acme"), ("quota", "50000")]),
            false,
        )
        .unwrap();
        assert_eq!(
            sentry_token(&env).as_deref(),
            Some("sntrys_x"),
            "usage still needs it"
        );
        assert_eq!(sentry_flags(&env), (true, false));
        let store = env.store.lock().unwrap();
        assert_eq!(store.settings().sentry_org_slug, "acme");
        assert_eq!(store.settings().sentry_monthly_event_quota, 50_000);
        drop(store);

        save_source_with(&env, "sentry", &parts(&[]), None, &[], false).unwrap();
        assert_eq!(sentry_token(&env), None);
        assert_eq!(sentry_flags(&env), (false, false));
        let store = env.store.lock().unwrap();
        assert!(store.settings().sentry_org_slug.is_empty());
        assert_eq!(store.settings().sentry_monthly_event_quota, 0);
    }

    #[test]
    fn removing_sentry_in_one_save_deletes_the_shared_token() {
        let env = Fake::new();
        sentry_with_both(&env);
        let status = save_source_with(&env, "sentry", &parts(&[]), None, &[], false).unwrap();
        assert_eq!(status, "Sentry removed.");
        assert_eq!(sentry_token(&env), None);
        assert_eq!(sentry_flags(&env), (false, false));
    }

    #[test]
    fn either_sentry_part_alone_needs_the_token_and_names_it_in_its_own_words() {
        for ticked in [&["usage"][..], &["crons"][..], &["usage", "crons"][..]] {
            let env = Fake::new();
            let refusal = save_source_with(&env, "sentry", &parts(ticked), None, &[], true);
            assert_eq!(
                refusal.unwrap_err(),
                "Enter a Sentry API token.",
                "{ticked:?}"
            );
            assert_eq!(sentry_flags(&env), (false, false));
            assert!(env.store.lock().unwrap().dashboard().is_none());
            assert!(env.woken.borrow().is_empty());
        }
        // One typed token covers whichever part was picked.
        let env = Fake::new();
        save_source_with(&env, "sentry", &parts(&["crons"]), Some("t"), &[], false).unwrap();
        assert_eq!(sentry_flags(&env), (false, true));
        assert_eq!(sentry_token(&env).as_deref(), Some("t"));
    }

    #[test]
    fn each_picked_sentry_part_asks_for_its_own_tile_and_only_that() {
        for (picked, expected) in [
            (&["usage", "crons"][..], vec!["sentryCrons", "usage"]),
            (&["usage"][..], vec!["usage"]),
            (&["crons"][..], vec!["sentryCrons"]),
        ] {
            let env = Fake::new();
            save_source(&env, "sentry", &parts(picked), Some("t"), true).unwrap();
            let mut asked = env.tiles_asked.borrow().clone();
            asked.sort();
            assert_eq!(asked, expected, "{picked:?}");
        }

        let env = Fake::new();
        save_source(&env, "sentry", &parts(&["crons"]), Some("t"), false).unwrap();
        assert!(env.tiles_asked.borrow().is_empty(), "no tile was asked for");
    }

    #[test]
    fn a_stored_sentry_token_covers_either_part_without_typing_it_again() {
        let env = Fake::new();
        sentry_with_both(&env);
        save_source_with(&env, "sentry", &parts(&["crons"]), None, &[], false).unwrap();
        save_source_with(&env, "sentry", &parts(&["usage"]), None, &[], false).unwrap();
        assert_eq!(sentry_token(&env).as_deref(), Some("sntrys_x"));
        assert_eq!(sentry_flags(&env), (true, false));
    }

    #[test]
    fn an_unreadable_store_refuses_removing_sentry_but_keeps_a_ticked_part() {
        let env = Fake::new();
        sentry_with_both(&env);
        env.credentials.fail_read.set(true);
        // A part still ticked means the token is not being removed, so a store
        // that won't answer is not in the way.
        save_source_with(&env, "sentry", &parts(&["usage"]), None, &[], false).unwrap();
        let refusal = save_source_with(&env, "sentry", &parts(&[]), None, &[], false);
        assert_eq!(
            refusal.unwrap_err(),
            "Couldn't read the credential store, so a stored Sentry token can't be removed. \
             Nothing changed."
        );
        assert_eq!(
            sentry_flags(&env),
            (true, false),
            "the refusal changed nothing"
        );
    }

    #[test]
    fn a_whole_number_of_events_or_nothing_is_the_quota() {
        let env = Fake::new();
        let refusal = save_source_with(
            &env,
            "sentry",
            &parts(&["usage"]),
            Some("t"),
            &typed(&[("quota", "1.5")]),
            false,
        );
        assert_eq!(
            refusal.unwrap_err(),
            "Enter a whole number of events, or leave the quota empty."
        );
        assert_eq!(sentry_token(&env), None, "a refused value writes nothing");
        assert_eq!(sentry_flags(&env), (false, false));
    }

    #[test]
    fn parts_sharing_a_key_declare_it_the_same_way() {
        for spec in &SOURCES {
            for part in spec.parts {
                for other in spec.parts {
                    let (Some(a), Some(b)) = (part.key, other.key) else {
                        continue;
                    };
                    if a.secret == b.secret {
                        assert_eq!(a.missing, b.missing, "{}", spec.kind);
                        assert_eq!(a.unreadable, b.unreadable, "{}", spec.kind);
                        assert_eq!(a.spend.is_some(), b.spend.is_some(), "{}", spec.kind);
                    }
                }
            }
        }
    }

    #[test]
    fn every_part_id_is_unique_within_its_source() {
        for spec in &SOURCES {
            let mut ids: Vec<_> = spec.parts.iter().map(|part| part.id).collect();
            ids.sort_unstable();
            ids.dedup();
            assert_eq!(ids.len(), spec.parts.len(), "{}", spec.kind);
        }
    }
}
