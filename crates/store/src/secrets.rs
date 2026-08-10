//! Credential-store access. Mirrors `DevCanopy/Services/KeychainHelper.swift`:
//! every credential lives in the OS store (macOS Keychain / Windows Credential
//! Manager), *never* in the JSON file [`crate::Store`] writes.
//!
//! Two layers, deliberately:
//! - [`SecretKey`] — the pure service/account naming. No I/O, so the naming
//!   (the part that silently loses a token when it drifts) is unit-tested.
//! - [`CredentialStore`] — the tiny get/set/delete surface, implemented over
//!   `keyring` ([`KeyringStore`]) and over a map ([`MemoryCredentialStore`]),
//!   so callers can be tested without touching the real OS store. The keyring
//!   implementation is intentionally *not* exercised by unit tests: hitting the
//!   login keychain from a test run prompts, and on CI it fails.
//!
//! `KeyringStore`'s own routing and migration logic — which credentials go
//! through the consolidated blob, when a write must fall back to a legacy
//! item — is factored behind [`ItemStore`] (get/set/delete one raw item by
//! account, no knowledge of [`SecretKey`] or the blob) so *that* logic is
//! unit-tested too, against a `HashMap`-backed fake, without touching the OS.
//!
//! Nothing here logs, formats, or `Debug`-prints a secret value. Errors carry
//! the account name only.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

/// Keychain/Credential-Manager service name, identical to the Swift app's
/// (`KeychainHelper.serviceName`).
pub const SERVICE: &str = "app.solador.desktop";

/// The Keychain service this app used before it was renamed.
///
/// Every credential lived under it: the GitHub PAT, the provider tokens, every
/// per-host agent token, and the OpenClaw device identity. A rename without a
/// migration orphans all of them at once — and orphaned is worse than deleted,
/// because they stay in the Keychain being useless.
pub const LEGACY_SERVICE: &str = "com.sassydog.devcanopy";

/// The one keychain item all consolidatable text secrets live in (a JSON map
/// keyed by [`SecretKey::account`] strings). One item means one ACL, so one
/// "Always Allow" covers every consolidatable secret. Two keys stay per-item
/// instead — see [`SecretKey::consolidatable`].
pub const BLOB_ACCOUNT: &str = "secrets_v1";

/// Every credential the app stores, and nothing else — a closed set, so a typo
/// in an account string is a compile error instead of a silently missing token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecretKey {
    /// A host's agent bearer token, keyed by [`crate::Host::id`].
    ///
    /// The account is `host-{uuid}`, matching what the Tauri shell already
    /// reads (`app/src-tauri/src/main.rs`). The Swift app spells the same
    /// credential `host_token_{UUID}` under the same service, so a token saved
    /// by one app is not visible to the other; unifying that is separate work
    /// and this crate deliberately does not fork a third spelling.
    HostToken(Uuid),
    /// Fine-grained GitHub PAT behind the Repos / GitHub Runners panels.
    GitHubAccessToken,
    /// Neon *organization* API key behind the Usage panel.
    NeonApiKey,
    /// Sentry `org:read` token behind the Usage panel.
    SentryUsageToken,
    /// Vercel API token, for the Usage panel's spend rows.
    VercelApiToken,
    /// Optional bearer token for the OpenClaw gateway.
    OpenClawBearerToken,
    /// The 32-byte Ed25519 seed behind this install's OpenClaw device identity.
    ///
    /// The only credential that is **not** text: it is raw key material, read
    /// and written through [`CredentialStore::secret_bytes`] /
    /// [`CredentialStore::set_secret_bytes`]. The account below is byte-for-byte
    /// the Swift app's (`KeychainHelper.saveOpenClawDeviceKey`) *and* the one
    /// `openclaw::identity::DEVICE_KEY_ACCOUNT` names, which is deliberate: both
    /// apps store the same 32 raw bytes under the same account, so the operator
    /// approves one device id rather than one per app. Storing it base64-encoded
    /// here would make each app read the other's entry as corrupt and overwrite
    /// it — a pairing that never settles.
    OpenClawDeviceKey,
}

impl SecretKey {
    /// The account name this credential is stored under, within [`SERVICE`].
    #[must_use]
    pub fn account(&self) -> String {
        match self {
            SecretKey::HostToken(id) => format!("host-{id}"),
            SecretKey::GitHubAccessToken => "github_access_token".to_owned(),
            SecretKey::NeonApiKey => "neon_api_key".to_owned(),
            SecretKey::SentryUsageToken => "sentry_usage_token".to_owned(),
            SecretKey::VercelApiToken => "vercel_api_token".to_owned(),
            SecretKey::OpenClawBearerToken => "openclaw_bearer_token".to_owned(),
            SecretKey::OpenClawDeviceKey => "openclaw_device_key".to_owned(),
        }
    }

    /// Whether this credential may live in the consolidated `secrets_v1`
    /// blob ([`BLOB_ACCOUNT`]). The blob's premise is that **this app is the
    /// only writer**: once the blob exists, routing never consults a legacy
    /// item, so a secret any other process maintains would be silently
    /// shadowed by a frozen migration-time copy. A secret with an external
    /// writer must therefore stay per-item.
    ///
    /// `false` for:
    /// - [`SecretKey::OpenClawDeviceKey`] — raw key material under a
    ///   cross-app account contract (see its variant docs); never blob
    ///   material.
    ///
    /// No wildcard arm, deliberately: adding a `SecretKey` variant without
    /// deciding whether it consolidates is a compile error — the same
    /// discipline as [`SecretKey::static_migration_keys`].
    #[must_use]
    pub fn consolidatable(&self) -> bool {
        match self {
            SecretKey::HostToken(_)
            | SecretKey::GitHubAccessToken
            | SecretKey::NeonApiKey
            | SecretKey::SentryUsageToken
            | SecretKey::VercelApiToken
            | SecretKey::OpenClawBearerToken => true,
            SecretKey::OpenClawDeviceKey => false,
        }
    }

    /// Every credential with a fixed identity that migration should always
    /// consider, alongside whatever per-host [`SecretKey::HostToken`]s the
    /// caller appends. Excludes `HostToken` (one per configured host — there
    /// is no fixed set of ids to name here) and
    /// [`SecretKey::OpenClawDeviceKey`] (never enters the blob;
    /// [`CredentialStore::migrate_legacy`] skips it defensively too).
    /// `is_static`'s match has no wildcard arm, so adding a `SecretKey`
    /// variant without deciding whether it belongs here is a compile error —
    /// replacing a hand-maintained list at the call site that a new variant
    /// could join without anyone noticing it was missing from migration.
    #[must_use]
    pub fn static_migration_keys() -> Vec<SecretKey> {
        fn is_static(key: &SecretKey) -> bool {
            match key {
                SecretKey::GitHubAccessToken
                | SecretKey::NeonApiKey
                | SecretKey::SentryUsageToken
                | SecretKey::VercelApiToken
                | SecretKey::OpenClawBearerToken => true,
                SecretKey::HostToken(_) | SecretKey::OpenClawDeviceKey => false,
            }
        }
        let keys = [
            SecretKey::GitHubAccessToken,
            SecretKey::NeonApiKey,
            SecretKey::SentryUsageToken,
            SecretKey::VercelApiToken,
            SecretKey::OpenClawBearerToken,
        ];
        debug_assert!(keys.iter().all(is_static));
        Vec::from(keys)
    }
}

/// A credential-store failure, carrying the account it happened on — never the
/// value. `keyring`'s own `Display` is value-free for the same reason.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("credential store failed for account {account}: {source}")]
    Backend {
        account: String,
        #[source]
        source: keyring::Error,
    },

    /// The consolidated item exists but its JSON would not parse. Deliberately
    /// carries no detail: a serde message can quote stored values. Recovery:
    /// delete the item named here in Keychain Access and relaunch — migration
    /// rebuilds it from the kept legacy items.
    #[error("the consolidated secret item '{account}' is unreadable — delete it in Keychain Access and relaunch to rebuild")]
    CorruptBlob { account: String },
}

/// Get/set/delete over the platform credential store.
///
/// A trait rather than a concrete type so callers (the future Settings wiring)
/// can be tested against [`MemoryCredentialStore`]. `&self`, not `&mut self`:
/// the backing store is the OS, and callers hold it behind an `Arc`.
pub trait CredentialStore {
    /// The stored value, or `None` when nothing is stored for `key`.
    ///
    /// # Errors
    /// Returns [`SecretError::Backend`] when the store itself fails. A missing
    /// entry is `Ok(None)`, matching Swift's `try? loadString(for:)`.
    fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError>;

    /// Stores (or replaces) the value for `key`.
    ///
    /// # Errors
    /// Returns [`SecretError::Backend`] when the store rejects the write.
    fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError>;

    /// Removes the value for `key`. Deleting a key that is not stored succeeds,
    /// matching Swift's `errSecItemNotFound` tolerance.
    ///
    /// # Errors
    /// Returns [`SecretError::Backend`] when the store rejects the delete.
    fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError>;

    /// The stored value as raw bytes, for the one credential that is key
    /// material rather than text ([`SecretKey::OpenClawDeviceKey`]).
    ///
    /// Separate methods rather than base64 over the string API on purpose: the
    /// Swift app writes those 32 bytes raw under the same account, and an
    /// encoding this side would make each app read the other's entry as
    /// unusable and replace it.
    ///
    /// # Errors
    /// Returns [`SecretError::Backend`] when the store itself fails. A missing
    /// entry is `Ok(None)`.
    fn secret_bytes(&self, key: SecretKey) -> Result<Option<Vec<u8>>, SecretError>;

    /// Stores (or replaces) raw bytes for `key`. See
    /// [`secret_bytes`](CredentialStore::secret_bytes).
    ///
    /// # Errors
    /// Returns [`SecretError::Backend`] when the store rejects the write.
    fn set_secret_bytes(&self, key: SecretKey, value: &[u8]) -> Result<(), SecretError>;

    /// Copies legacy per-item secrets into the consolidated blob, once.
    ///
    /// Returns how many values were copied. The blob is written even when
    /// empty — that is what marks migration done and stops re-runs. Legacy
    /// items are never modified or deleted: they keep the Swift app alive
    /// through the transition and are the rebuild source if the blob is ever
    /// corrupted.
    ///
    /// On the blob-present path — migration has already run — the call also
    /// scrubs any non-consolidatable account out of the blob (see
    /// [`route_migrate_legacy`]); the returned count still means "legacy
    /// values copied", never the scrub count.
    ///
    /// Calling this before the first [`CredentialStore::set_secret`] is
    /// **not** an invariant callers must uphold. It was, until the
    /// whole-branch review found the ordering unsafe: a denied or failed
    /// migration writes nothing, and a `set_secret` that ran before migration
    /// used to create the blob holding only that one key — permanently
    /// shadowing every legacy value migration never got to copy, since
    /// migration no-ops once the blob exists. `set_secret` now falls through
    /// to the legacy item whenever the blob is absent, so a failed migration
    /// simply retries next launch instead of stranding anything. Calling this
    /// early is still good practice — it is what makes the blob exist at
    /// all — it is just no longer load-bearing for correctness.
    ///
    /// # Errors
    /// Returns [`SecretError::Backend`] when the store itself fails.
    fn migrate_legacy(&self, _keys: &[SecretKey]) -> Result<usize, SecretError> {
        Ok(0)
    }

    /// Adopts credentials written under the pre-rename service. Default: none.
    ///
    /// # Errors
    /// Implementations may fail on a backend error.
    fn migrate_service(&self, _keys: &[SecretKey]) -> Result<usize, SecretError> {
        Ok(0)
    }
}

/// Parse the blob's JSON map. Value-free on failure by construction.
fn parse_blob(text: &str) -> Result<std::collections::BTreeMap<String, String>, SecretError> {
    serde_json::from_str(text).map_err(|_| SecretError::CorruptBlob {
        account: BLOB_ACCOUNT.to_owned(),
    })
}

/// Serialize the blob's JSON map. `BTreeMap` keeps the output deterministic.
fn serialize_blob(map: &std::collections::BTreeMap<String, String>) -> String {
    serde_json::to_string(map).expect("a string map always serializes")
}

/// The three primitives [`KeyringStore`]'s routing and migration are built on:
/// get/set/delete one *raw* item by account, with no knowledge of
/// [`SecretKey`], the blob, or migration. A trait, not direct `keyring::Entry`
/// calls, purely so the routing/migration logic below is unit-testable
/// against a fake — this module's convention (see the module doc) is that no
/// test exercises the real keychain.
trait ItemStore {
    fn get_item(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn set_item(&self, account: &str, value: &str) -> Result<(), SecretError>;
    fn delete_item(&self, account: &str) -> Result<(), SecretError>;
}

/// Reads and parses the consolidated blob item via any [`ItemStore`].
/// `Ok(None)` means the item does not exist yet (pre-migration, or a fresh
/// install).
fn read_blob<S: ItemStore>(
    store: &S,
) -> Result<Option<std::collections::BTreeMap<String, String>>, SecretError> {
    match store.get_item(BLOB_ACCOUNT)? {
        Some(text) => parse_blob(&text).map(Some),
        None => Ok(None),
    }
}

/// Writes the consolidated blob item wholesale via any [`ItemStore`].
fn write_blob<S: ItemStore>(
    store: &S,
    map: &std::collections::BTreeMap<String, String>,
) -> Result<(), SecretError> {
    store.set_item(BLOB_ACCOUNT, &serialize_blob(map))
}

/// Accounts the blob must never hold: every fixed-account key
/// [`SecretKey::consolidatable`] refuses. This is what the existing-blob
/// scrub in [`route_migrate_legacy`] removes — healing installs whose blob
/// was written before the external-writer rule existed. `HostToken` is
/// absent because it is consolidatable; the `debug_assert` keeps this list
/// honest against the predicate rather than trusting hand maintenance.
fn non_consolidatable_accounts() -> Vec<String> {
    let keys = [SecretKey::OpenClawDeviceKey];
    debug_assert!(keys.iter().all(|key| !key.consolidatable()));
    let mut accounts: Vec<String> = keys.iter().map(SecretKey::account).collect();
    accounts.push(RETIRED_AZURE_SAS_ACCOUNT.to_owned());
    accounts
}

/// The account of a credential this app no longer keeps.
///
/// The Azure Cost SAS used to live in the credential store, written from
/// outside this process by a LaunchAgent — which is why it was exempt from
/// consolidation. The app now mints its own SAS per poll from the operator's
/// Entra session and stores nothing, so [`SecretKey`] has no variant for it.
///
/// The name survives here because the *scrub* must: an install upgraded from a
/// build that had this key may still carry the account inside its blob, and
/// dropping it from this list would leave a stale credential sitting in the
/// consolidated item forever with nothing left to remove it.
const RETIRED_AZURE_SAS_ACCOUNT: &str = "azure_cost_sas_url";

/// Copies every credential from a pre-rename service into this one.
///
/// Generic over [`ItemStore`] on both sides so the whole thing is testable
/// without an OS keychain — the same reason the routing below is.
///
/// **Copy, never move.** The old entries stay where they are: an operator who
/// runs an older build afterwards still finds their credentials, and deleting
/// somebody's secrets as a side effect of a rename is not a trade this code
/// gets to make on their behalf.
///
/// **Once, and never over the top.** If the destination holds anything at all
/// this returns 0 without reading further. A second pass must not resurrect a
/// credential the operator has since cleared — that is the failure mode where
/// a "helpful" migration hands back a revoked token forever.
fn route_migrate_service<S: ItemStore>(
    legacy: &S,
    current: &S,
    keys: &[SecretKey],
) -> Result<usize, SecretError> {
    // The blob first: on a consolidated install it holds everything, so its
    // presence alone means this migration has already happened.
    let mut accounts: Vec<String> = vec![BLOB_ACCOUNT.to_owned()];
    accounts.extend(keys.iter().map(SecretKey::account));
    // The Azure SAS no longer has a `SecretKey`, but a pre-rename install can
    // still be holding one. Copied so the *scrub* can then remove it from the
    // blob on the far side, rather than leaving it stranded under a service
    // nothing will ever look at again.
    accounts.push(RETIRED_AZURE_SAS_ACCOUNT.to_owned());
    // The device key, explicitly. `static_migration_keys` leaves it out on
    // purpose — it never enters the blob — so relying on the caller's list
    // silently drops the one credential a human cannot re-type: losing it
    // means re-pairing the OpenClaw device identity from scratch.
    accounts.push(SecretKey::OpenClawDeviceKey.account());
    accounts.sort();
    accounts.dedup();

    for account in &accounts {
        if current.get_item(account)?.is_some() {
            return Ok(0);
        }
    }

    let mut copied = 0;
    for account in &accounts {
        if let Some(value) = legacy.get_item(account)? {
            current.set_item(account, &value)?;
            copied += 1;
        }
    }
    Ok(copied)
}

/// [`CredentialStore::secret`]'s routing, generic over [`ItemStore`] so it is
/// testable without the OS. `consolidate` is a parameter rather than read
/// internally (vs. calling [`KeyringStore::consolidate`] here) so tests can
/// exercise both branches — consolidated and forced-legacy — deterministically
/// on any platform's CI, rather than only whichever branch happens to be live
/// on the machine actually running the test.
fn route_secret<S: ItemStore>(
    store: &S,
    consolidate: bool,
    key: SecretKey,
) -> Result<Option<String>, SecretError> {
    if !key.consolidatable() || !consolidate {
        return store.get_item(&key.account());
    }
    match read_blob(store)? {
        Some(map) => Ok(map.get(&key.account()).cloned()),
        // Not migrated yet: the legacy item is still the truth.
        None => store.get_item(&key.account()),
    }
}

/// [`CredentialStore::set_secret`]'s routing. See [`route_secret`].
///
/// The blob-absent arm falls through to the legacy item rather than creating
/// the blob: an early write must not shadow legacy values migration has not
/// copied yet (see [`CredentialStore::migrate_legacy`]'s docs for the failure
/// this fixes).
fn route_set_secret<S: ItemStore>(
    store: &S,
    blob_lock: &Mutex<()>,
    consolidate: bool,
    key: SecretKey,
    value: &str,
) -> Result<(), SecretError> {
    if !key.consolidatable() || !consolidate {
        return store.set_item(&key.account(), value);
    }
    let _guard = blob_lock.lock().expect("blob lock poisoned");
    match read_blob(store)? {
        Some(mut map) => {
            map.insert(key.account(), value.to_owned());
            write_blob(store, &map)
        }
        // Not migrated yet: the legacy item is still the truth, and creating
        // the blob here would shadow every legacy value migration has not
        // copied yet.
        None => store.set_item(&key.account(), value),
    }
}

/// [`CredentialStore::delete_secret`]'s routing. See [`route_secret`].
///
/// The blob-present arm clears the legacy item too: leaving it would
/// resurrect a cleared credential the next time the blob is rebuilt from
/// legacy items.
fn route_delete_secret<S: ItemStore>(
    store: &S,
    blob_lock: &Mutex<()>,
    consolidate: bool,
    key: SecretKey,
) -> Result<(), SecretError> {
    if !key.consolidatable() || !consolidate {
        return store.delete_item(&key.account());
    }
    let _guard = blob_lock.lock().expect("blob lock poisoned");
    match read_blob(store)? {
        Some(mut map) => {
            map.remove(&key.account());
            write_blob(store, &map)?;
            store.delete_item(&key.account())
        }
        // Not migrated yet: clear the legacy item, or it resurfaces at
        // migration time.
        None => store.delete_item(&key.account()),
    }
}

/// [`CredentialStore::migrate_legacy`]'s implementation. See [`route_secret`]
/// for why `consolidate` is a parameter.
///
/// Returns `(copied, scrubbed)`. `copied` is what
/// [`CredentialStore::migrate_legacy`] returns to its caller — legacy values
/// copied into a freshly-written blob. `scrubbed` is a second, internal-only
/// count of non-consolidatable accounts removed from an *existing* blob;
/// [`KeyringStore::migrate_legacy`] logs it (count-only, when nonzero) and
/// does not fold it into the public `Result<usize>` contract, which means
/// "copied" and nothing else.
fn route_migrate_legacy<S: ItemStore>(
    store: &S,
    blob_lock: &Mutex<()>,
    consolidate: bool,
    keys: &[SecretKey],
) -> Result<(usize, usize), SecretError> {
    if !consolidate {
        return Ok((0, 0));
    }
    let _guard = blob_lock.lock().expect("blob lock poisoned");
    if let Some(mut map) = read_blob(store)? {
        // Migration already ran. Scrub any non-consolidatable account out
        // of the existing blob: routing can never read it again, so it is
        // a stale credential at rest — and a resurrection hazard if routing
        // ever changed. Rewrite only when something was removed. The
        // returned count still means "legacy values copied": a scrub-only
        // pass is 0, so the startup log line stays truthful.
        //
        // Conscious trade-off: this *discards* rather than relocates. On an
        // install where the SAS was pasted via Settings after consolidation
        // and no LaunchAgent exists to have ever written a legacy item, the
        // scrub deletes the only copy — there is no per-item entry to fall
        // back to — and the Azure Cost panel shows its setup instruction
        // until the URL is re-pasted. Accepted: a credential routing can
        // never read again is a bigger hazard left in place than lost, and a
        // real external writer would have overwritten a relocated copy
        // anyway.
        let before = map.len();
        for account in non_consolidatable_accounts() {
            map.remove(&account);
        }
        let scrubbed = before - map.len();
        if scrubbed > 0 {
            write_blob(store, &map)?;
        }
        return Ok((0, scrubbed));
    }
    let mut map = std::collections::BTreeMap::new();
    for key in keys {
        // Non-consolidatable keys never join the blob (see `consolidatable`);
        // this generalizes the former device-key-only skip.
        if !key.consolidatable() {
            continue;
        }
        if let Some(value) = store.get_item(&key.account())? {
            map.insert(key.account(), value);
        }
    }
    let copied = map.len();
    write_blob(store, &map)?;
    Ok((copied, 0))
}

/// Compile-time half of [`KeyringStore::consolidate`]: true only on macOS.
/// Kept a literal `const` (rather than folded straight into that function) so
/// it can be flipped by hand for a one-off cross-platform sanity check when a
/// real non-macOS target is not available locally.
const CONSOLIDATE: bool = cfg!(target_os = "macos");

/// The real thing: `keyring` over the platform credential store.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
    /// Serializes blob read-modify-write. Per-item writes never raced each
    /// other; a shared blob can (poll loop vs. settings commands). This is a
    /// process-local lock, not a cross-process one: it serializes writes
    /// within this process only, and two concurrent app instances can still
    /// race a whole-map write and clobber each other.
    blob_lock: std::sync::Arc<std::sync::Mutex<()>>,
}

impl KeyringStore {
    /// A store bound to [`SERVICE`] — what the app uses.
    #[must_use]
    pub fn new() -> Self {
        KeyringStore {
            service: SERVICE.to_owned(),
            blob_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// A store bound to a caller-supplied service name. Exists so a
    /// throwaway service can be used when someone deliberately exercises the
    /// real OS store; the app itself always uses [`KeyringStore::new`].
    #[must_use]
    pub fn with_service(service: impl Into<String>) -> Self {
        KeyringStore {
            service: service.into(),
            blob_lock: std::sync::Arc::new(std::sync::Mutex::new(())),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, account).map_err(|source| SecretError::Backend {
            account: account.to_owned(),
            source,
        })
    }

    /// Whether this process routes secrets through the consolidated
    /// `secrets_v1` blob at all. `false` forces every routed method
    /// ([`KeyringStore::secret`](CredentialStore::secret),
    /// [`KeyringStore::set_secret`](CredentialStore::set_secret),
    /// [`KeyringStore::delete_secret`](CredentialStore::delete_secret),
    /// [`KeyringStore::migrate_legacy`](CredentialStore::migrate_legacy)) back
    /// to per-item storage, exactly as this store behaved before
    /// consolidation.
    ///
    /// Two independent things force `false`:
    /// - **Any platform but macOS** ([`CONSOLIDATE`]). The per-item ACL
    ///   prompt consolidation exists to fix is a macOS keychain behavior, and
    ///   `keyring`'s Windows Credential Manager backend rejects a credential
    ///   blob over 2560 bytes (`TooLong`) — a ceiling per-item storage never
    ///   hits, but a shared blob crosses at roughly five hosts, after which
    ///   *every* save and delete fails (the read-modify-write rewrites the
    ///   whole map).
    /// - **`SOLADOR_LEGACY_SECRETS=1`**, checked fresh on every call — the
    ///   manual escape hatch: abandoning consolidation is one env var, not
    ///   deleting the `secrets_v1` item before every launch.
    fn consolidate() -> bool {
        CONSOLIDATE && std::env::var_os("SOLADOR_LEGACY_SECRETS").is_none()
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        KeyringStore::new()
    }
}

impl ItemStore for KeyringStore {
    fn get_item(&self, account: &str) -> Result<Option<String>, SecretError> {
        match self.entry(account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(SecretError::Backend {
                account: account.to_owned(),
                source,
            }),
        }
    }

    fn set_item(&self, account: &str, value: &str) -> Result<(), SecretError> {
        self.entry(account)?
            .set_password(value)
            .map_err(|source| SecretError::Backend {
                account: account.to_owned(),
                source,
            })
    }

    fn delete_item(&self, account: &str) -> Result<(), SecretError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(source) => Err(SecretError::Backend {
                account: account.to_owned(),
                source,
            }),
        }
    }
}

impl CredentialStore for KeyringStore {
    fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        route_secret(self, Self::consolidate(), key)
    }

    fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        route_set_secret(self, &self.blob_lock, Self::consolidate(), key, value)
    }

    fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError> {
        route_delete_secret(self, &self.blob_lock, Self::consolidate(), key)
    }

    /// Raw bytes stay per-item for every key, on every platform, regardless
    /// of [`KeyringStore::consolidate`]: [`SecretKey::OpenClawDeviceKey`] is
    /// their only consumer, and it never enters the blob (see
    /// [`SecretKey::OpenClawDeviceKey`]'s docs).
    fn secret_bytes(&self, key: SecretKey) -> Result<Option<Vec<u8>>, SecretError> {
        let account = key.account();
        match self.entry(&account)?.get_secret() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(SecretError::Backend { account, source }),
        }
    }

    /// Raw bytes stay per-item for every key. See
    /// [`KeyringStore::secret_bytes`].
    fn set_secret_bytes(&self, key: SecretKey, value: &[u8]) -> Result<(), SecretError> {
        let account = key.account();
        self.entry(&account)?
            .set_secret(value)
            .map_err(|source| SecretError::Backend { account, source })
    }

    fn migrate_service(&self, keys: &[SecretKey]) -> Result<usize, SecretError> {
        if self.service == LEGACY_SERVICE {
            return Ok(0);
        }
        let legacy = KeyringStore::with_service(LEGACY_SERVICE);
        let copied = route_migrate_service(&legacy, self, keys)?;
        if copied > 0 {
            // Count only, never account names or values — the module's
            // value-free rule applies to its own progress reports.
            let noun = if copied == 1 {
                "credential"
            } else {
                "credentials"
            };
            eprintln!("adopted {copied} {noun} from the pre-rename keychain service");
        }
        Ok(copied)
    }

    fn migrate_legacy(&self, keys: &[SecretKey]) -> Result<usize, SecretError> {
        let (copied, scrubbed) =
            route_migrate_legacy(self, &self.blob_lock, Self::consolidate(), keys)?;
        // Count-only, never account names or values — matches the module's
        // value-free rule. Only when the rewrite actually happened: a quiet
        // pass (nothing to scrub) prints nothing.
        if scrubbed > 0 {
            let noun = if scrubbed == 1 { "entry" } else { "entries" };
            eprintln!(
                "secrets: scrubbed {scrubbed} non-consolidatable {noun} from the consolidated item"
            );
        }
        Ok(copied)
    }
}

/// An in-memory [`CredentialStore`] for tests — nothing here reaches the OS.
///
/// Public because the code that *consumes* credentials (the later Settings/poll
/// wiring) has to be testable without prompting for a keychain unlock.
/// Values are held as bytes, not `String`, so the one binary credential
/// ([`SecretKey::OpenClawDeviceKey`]) round-trips through this store exactly as
/// it does through the real one.
#[derive(Default)]
pub struct MemoryCredentialStore {
    entries: Mutex<HashMap<String, Vec<u8>>>,
}

impl MemoryCredentialStore {
    #[must_use]
    pub fn new() -> Self {
        MemoryCredentialStore::default()
    }

    /// Accounts currently holding a value, sorted. Deliberately returns the
    /// account names, never the values.
    #[must_use]
    pub fn accounts(&self) -> Vec<String> {
        let entries = self.entries.lock().expect("credential map poisoned");
        let mut accounts: Vec<String> = entries.keys().cloned().collect();
        accounts.sort();
        accounts
    }
}

/// Hand-written so a `{:?}` of a store — in a log line, a panic message, an
/// error chain — can never print a credential. Derived `Debug` would.
impl std::fmt::Debug for MemoryCredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryCredentialStore")
            .field("accounts", &self.accounts())
            .finish_non_exhaustive()
    }
}

impl CredentialStore for MemoryCredentialStore {
    /// A stored value that is not UTF-8 reads as absent through the *string*
    /// API — it is key material, not a password, and handing back a lossy
    /// transcription of it would be worse than admitting it is not text.
    fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        let entries = self.entries.lock().expect("credential map poisoned");
        Ok(entries
            .get(&key.account())
            .and_then(|bytes| String::from_utf8(bytes.clone()).ok()))
    }

    fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        self.set_secret_bytes(key, value.as_bytes())
    }

    fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().expect("credential map poisoned");
        entries.remove(&key.account());
        Ok(())
    }

    fn secret_bytes(&self, key: SecretKey) -> Result<Option<Vec<u8>>, SecretError> {
        let entries = self.entries.lock().expect("credential map poisoned");
        Ok(entries.get(&key.account()).cloned())
    }

    fn set_secret_bytes(&self, key: SecretKey, value: &[u8]) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().expect("credential map poisoned");
        entries.insert(key.account(), value.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The service name is not a detail: it is the address of every credential
    /// this app has. Pinned in a test so moving it is always a deliberate act
    /// with a migration attached — which is exactly how it moved last time.
    ///
    /// It used to have to match the frozen Swift app's keychain helper, and no
    /// longer does. That app is frozen, still reads the old service, and will
    /// keep finding its own credentials there because this migration *copies*
    /// rather than moves.
    #[test]
    fn the_service_is_the_renamed_one_and_the_old_name_is_still_known() {
        assert_eq!(SERVICE, "app.solador.desktop");
        assert_eq!(
            LEGACY_SERVICE, "com.sassydog.devcanopy",
            "the migration cannot find what it cannot name"
        );
        assert_ne!(SERVICE, LEGACY_SERVICE);
    }

    // MARK: - Adopting the pre-rename service

    /// The whole point: a rename must not orphan every credential the operator
    /// has. Orphaned is worse than deleted — the secrets stay in the Keychain,
    /// being useless.
    #[test]
    fn a_rename_adopts_every_credential_from_the_old_service() {
        let legacy = FakeItemStore::default();
        let current = FakeItemStore::default();
        legacy
            .set_item("github_access_token", "ghp_x")
            .expect("seed");
        legacy.set_item("neon_api_key", "napi_x").expect("seed");
        legacy
            .set_item("openclaw_device_key", "seed-bytes")
            .expect("seed");

        let keys = SecretKey::static_migration_keys();
        let copied = route_migrate_service(&legacy, &current, &keys).expect("migrate");

        assert_eq!(copied, 3);
        assert_eq!(
            current.get_item("github_access_token").expect("read"),
            Some("ghp_x".to_owned())
        );
        // The device identity above all: losing it means re-pairing with the
        // OpenClaw gateway, which is the one credential a human cannot simply
        // re-type.
        assert_eq!(
            current.get_item("openclaw_device_key").expect("read"),
            Some("seed-bytes".to_owned())
        );
    }

    /// Copy, never move. An operator who runs an older build afterwards still
    /// finds their credentials; deleting somebody's secrets as a side effect of
    /// a rename is not this code's call to make.
    #[test]
    fn adoption_leaves_the_old_service_intact() {
        let legacy = FakeItemStore::default();
        let current = FakeItemStore::default();
        legacy
            .set_item("github_access_token", "ghp_x")
            .expect("seed");

        route_migrate_service(&legacy, &current, &SecretKey::static_migration_keys())
            .expect("migrate");

        assert_eq!(
            legacy.get_item("github_access_token").expect("read"),
            Some("ghp_x".to_owned())
        );
    }

    /// The failure mode this guards is specific and nasty: a migration that
    /// runs twice hands back a credential the operator has since *cleared*,
    /// resurrecting a revoked token on every launch forever.
    #[test]
    fn adoption_never_writes_over_a_service_that_already_holds_anything() {
        let legacy = FakeItemStore::default();
        let current = FakeItemStore::default();
        legacy
            .set_item("github_access_token", "ghp_old")
            .expect("seed");
        current
            .set_item("github_access_token", "ghp_current")
            .expect("seed");

        let copied = route_migrate_service(&legacy, &current, &SecretKey::static_migration_keys())
            .expect("migrate");

        assert_eq!(copied, 0, "a populated service is never migrated into");
        assert_eq!(
            current.get_item("github_access_token").expect("read"),
            Some("ghp_current".to_owned()),
            "the live credential must win"
        );
    }

    /// A consolidated install keeps everything in the blob, so the blob alone
    /// has to count as "already migrated" — otherwise the per-item scan below
    /// it sees nothing and cheerfully re-copies stale entries over the top.
    #[test]
    fn a_destination_holding_only_the_blob_still_counts_as_migrated() {
        let legacy = FakeItemStore::default();
        let current = FakeItemStore::default();
        legacy.set_item("neon_api_key", "napi_old").expect("seed");
        current.set_item(BLOB_ACCOUNT, "{}").expect("seed");

        let copied = route_migrate_service(&legacy, &current, &SecretKey::static_migration_keys())
            .expect("migrate");

        assert_eq!(copied, 0);
        assert!(current.get_item("neon_api_key").expect("read").is_none());
    }

    /// A genuine first install has nothing to adopt, and must not be an error.
    #[test]
    fn a_fresh_install_adopts_nothing_quietly() {
        let legacy = FakeItemStore::default();
        let current = FakeItemStore::default();
        let copied = route_migrate_service(&legacy, &current, &SecretKey::static_migration_keys())
            .expect("migrate");
        assert_eq!(copied, 0);
    }

    /// The Azure SAS has no `SecretKey` any more, but a pre-rename install can
    /// still hold one — and it has to arrive on this side for the blob scrub to
    /// be able to remove it.
    #[test]
    fn the_retired_azure_account_is_adopted_so_the_scrub_can_reach_it() {
        let legacy = FakeItemStore::default();
        let current = FakeItemStore::default();
        legacy
            .set_item(RETIRED_AZURE_SAS_ACCOUNT, "https://acct...?sig=x")
            .expect("seed");

        let copied = route_migrate_service(&legacy, &current, &SecretKey::static_migration_keys())
            .expect("migrate");

        assert_eq!(copied, 1);
        assert!(current
            .get_item(RETIRED_AZURE_SAS_ACCOUNT)
            .expect("read")
            .is_some());
    }

    #[test]
    fn host_token_account_matches_the_tauri_shell() {
        let id = Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").expect("uuid");
        assert_eq!(
            SecretKey::HostToken(id).account(),
            "host-6ba7b810-9dad-11d1-80b4-00c04fd430c8"
        );
    }

    #[test]
    fn provider_accounts_match_the_swift_keys() {
        assert_eq!(
            SecretKey::GitHubAccessToken.account(),
            "github_access_token"
        );
        assert_eq!(SecretKey::NeonApiKey.account(), "neon_api_key");
        assert_eq!(SecretKey::SentryUsageToken.account(), "sentry_usage_token");
        assert_eq!(
            SecretKey::OpenClawBearerToken.account(),
            "openclaw_bearer_token"
        );
    }

    /// The device seed's account is shared with the Swift app on purpose: both
    /// write the same raw 32 bytes there, so one device id gets approved rather
    /// than one per app. A rename here silently splits the identity in two.
    #[test]
    fn the_device_key_account_is_the_one_both_apps_agree_on() {
        assert_eq!(
            SecretKey::OpenClawDeviceKey.account(),
            "openclaw_device_key"
        );
    }

    #[test]
    fn every_account_is_distinct() {
        let keys = [
            SecretKey::HostToken(Uuid::new_v4()),
            SecretKey::HostToken(Uuid::new_v4()),
            SecretKey::GitHubAccessToken,
            SecretKey::NeonApiKey,
            SecretKey::SentryUsageToken,
            SecretKey::VercelApiToken,
            SecretKey::OpenClawBearerToken,
            SecretKey::OpenClawDeviceKey,
        ];
        let mut accounts: Vec<String> = keys.iter().map(SecretKey::account).collect();
        let total = accounts.len();
        accounts.sort();
        accounts.dedup();
        assert_eq!(
            accounts.len(),
            total,
            "two credentials share one account name"
        );
    }

    /// [`non_consolidatable_accounts`] is what the existing-blob scrub trusts
    /// to know which accounts must never live in the blob. Its own
    /// `debug_assert` only checks that its hand-written list is *sound*
    /// (every listed key really is non-consolidatable) — not that the list is
    /// *complete*. This test checks completeness directly against the
    /// predicate, over every fixed-account key (`HostToken` excluded: it has
    /// no fixed account to enumerate): a variant reclassified
    /// non-consolidatable but left out of the scrub list fails here, not
    /// silently at scrub time.
    #[test]
    fn non_consolidatable_accounts_matches_the_predicate_over_every_fixed_account_key() {
        let fixed_account_keys = [
            SecretKey::GitHubAccessToken,
            SecretKey::NeonApiKey,
            SecretKey::SentryUsageToken,
            SecretKey::VercelApiToken,
            SecretKey::OpenClawBearerToken,
            SecretKey::OpenClawDeviceKey,
        ];
        let mut expected: Vec<String> = fixed_account_keys
            .iter()
            .filter(|key| !key.consolidatable())
            .map(SecretKey::account)
            .collect();
        // Plus the one account no `SecretKey` spells any more. The Azure Cost
        // SAS was removed when the app learned to mint its own, but an
        // upgraded install can still be carrying it in the blob, so the scrub
        // list is deliberately *wider* than the predicate. Named here rather
        // than tolerated, so a future addition to the list still has to be
        // justified rather than merely accommodated.
        expected.push(RETIRED_AZURE_SAS_ACCOUNT.to_owned());
        expected.sort();

        let mut actual = non_consolidatable_accounts();
        actual.sort();

        assert_eq!(actual, expected);
    }

    /// The blob's premise is "this app is the only writer". The two keys that
    /// break the premise — and only those — must refuse consolidation.
    #[test]
    fn consolidatable_refuses_the_externally_written_and_raw_byte_keys() {
        assert!(!SecretKey::OpenClawDeviceKey.consolidatable());

        assert!(SecretKey::HostToken(Uuid::new_v4()).consolidatable());
        assert!(SecretKey::GitHubAccessToken.consolidatable());
        assert!(SecretKey::NeonApiKey.consolidatable());
        assert!(SecretKey::SentryUsageToken.consolidatable());
        assert!(SecretKey::VercelApiToken.consolidatable());
        assert!(SecretKey::OpenClawBearerToken.consolidatable());
    }

    #[test]
    fn static_migration_keys_lists_only_consolidatable_static_keys() {
        let keys = SecretKey::static_migration_keys();
        let accounts: Vec<String> = keys.iter().map(SecretKey::account).collect();
        assert_eq!(
            accounts,
            vec![
                "github_access_token",
                "neon_api_key",
                "sentry_usage_token",
                "vercel_api_token",
                "openclaw_bearer_token",
            ]
        );
        assert!(!keys.iter().any(|k| matches!(k, SecretKey::HostToken(_))));
        assert!(!keys.contains(&SecretKey::OpenClawDeviceKey));
    }

    #[test]
    fn memory_store_round_trips_and_deletes() {
        let store = MemoryCredentialStore::new();
        assert_eq!(store.secret(SecretKey::NeonApiKey).expect("get"), None);

        store
            .set_secret(SecretKey::NeonApiKey, "value-a")
            .expect("set");
        assert_eq!(
            store.secret(SecretKey::NeonApiKey).expect("get").as_deref(),
            Some("value-a")
        );

        store
            .set_secret(SecretKey::NeonApiKey, "value-b")
            .expect("replace");
        assert_eq!(
            store.secret(SecretKey::NeonApiKey).expect("get").as_deref(),
            Some("value-b")
        );

        store.delete_secret(SecretKey::NeonApiKey).expect("delete");
        assert_eq!(store.secret(SecretKey::NeonApiKey).expect("get"), None);
        // Deleting what is not there is not an error.
        store
            .delete_secret(SecretKey::NeonApiKey)
            .expect("delete again");
    }

    #[test]
    fn per_host_tokens_do_not_collide() {
        let store = MemoryCredentialStore::new();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        store
            .set_secret(SecretKey::HostToken(a), "token-a")
            .expect("set a");
        store
            .set_secret(SecretKey::HostToken(b), "token-b")
            .expect("set b");
        assert_eq!(
            store
                .secret(SecretKey::HostToken(a))
                .expect("get a")
                .as_deref(),
            Some("token-a")
        );
        assert_eq!(
            store
                .secret(SecretKey::HostToken(b))
                .expect("get b")
                .as_deref(),
            Some("token-b")
        );

        store
            .delete_secret(SecretKey::HostToken(a))
            .expect("delete a");
        assert_eq!(store.secret(SecretKey::HostToken(a)).expect("get a"), None);
        assert_eq!(
            store
                .secret(SecretKey::HostToken(b))
                .expect("get b")
                .as_deref(),
            Some("token-b")
        );
    }

    /// The device seed is 32 bytes of key material and is almost never valid
    /// UTF-8. Round-tripping it through the *string* API would corrupt it, so
    /// the byte API has to be the one that carries it — intact, and byte-equal
    /// to what went in.
    #[test]
    fn raw_bytes_round_trip_without_going_through_a_string() {
        let store = MemoryCredentialStore::new();
        let seed: Vec<u8> = (0u8..32)
            .map(|i| i.wrapping_mul(7).wrapping_add(200))
            .collect();
        assert!(
            String::from_utf8(seed.clone()).is_err(),
            "the fixture must not accidentally be valid UTF-8"
        );

        store
            .set_secret_bytes(SecretKey::OpenClawDeviceKey, &seed)
            .expect("set");
        assert_eq!(
            store
                .secret_bytes(SecretKey::OpenClawDeviceKey)
                .expect("get"),
            Some(seed)
        );
        // …and it does not masquerade as a password.
        assert_eq!(
            store.secret(SecretKey::OpenClawDeviceKey).expect("get"),
            None
        );

        store
            .delete_secret(SecretKey::OpenClawDeviceKey)
            .expect("delete");
        assert_eq!(
            store
                .secret_bytes(SecretKey::OpenClawDeviceKey)
                .expect("get"),
            None
        );
    }

    #[test]
    fn debug_never_prints_a_secret_value() {
        let store = MemoryCredentialStore::new();
        store
            .set_secret(SecretKey::GitHubAccessToken, "ghp_supersecret")
            .expect("set");
        let rendered = format!("{store:?}");
        assert!(
            !rendered.contains("ghp_supersecret"),
            "Debug leaked a credential: {rendered}"
        );
        assert!(rendered.contains("github_access_token"));
    }

    /// The blob's account must never shadow a real per-secret account — the
    /// legacy fallthrough would read the blob as a secret.
    #[test]
    fn the_blob_account_collides_with_no_secret_account() {
        let mut accounts = vec![
            SecretKey::GitHubAccessToken.account(),
            SecretKey::NeonApiKey.account(),
            SecretKey::SentryUsageToken.account(),
            SecretKey::OpenClawBearerToken.account(),
            SecretKey::OpenClawDeviceKey.account(),
            SecretKey::HostToken(uuid::Uuid::nil()).account(),
        ];
        accounts.retain(|a| a == BLOB_ACCOUNT);
        assert!(accounts.is_empty());
    }

    /// Corrupt blob JSON is a typed, value-free error — never an empty map
    /// (which would read as "no secrets stored") and never the serde message
    /// (which can quote stored values).
    #[test]
    fn a_corrupt_blob_is_a_value_free_error() {
        let err = parse_blob("{not json").unwrap_err();
        let text = err.to_string();
        assert!(text.contains(BLOB_ACCOUNT));
        assert!(
            !text.contains("not json"),
            "error must not echo blob content"
        );
    }

    /// The blob round-trips deterministically (BTreeMap ordering).
    #[test]
    fn the_blob_map_round_trips() {
        let mut map = std::collections::BTreeMap::new();
        map.insert("neon_api_key".to_owned(), "napi_x".to_owned());
        map.insert("github_access_token".to_owned(), "ghp_y".to_owned());
        let text = serialize_blob(&map);
        assert_eq!(parse_blob(&text).expect("parse"), map);
    }

    /// The default implementation is a no-op — MemoryCredentialStore and any
    /// future double migrate nothing.
    #[test]
    fn migration_defaults_to_a_no_op() {
        let store = MemoryCredentialStore::new();
        assert_eq!(
            store
                .migrate_legacy(&[SecretKey::NeonApiKey])
                .expect("no-op"),
            0
        );
    }

    /// A [`HashMap`]-backed [`ItemStore`], purely so `KeyringStore`'s routing
    /// and migration are testable without touching the OS — this module's
    /// convention (see the module doc) is that no test exercises the real
    /// keychain.
    #[derive(Default)]
    struct FakeItemStore {
        items: Mutex<HashMap<String, String>>,
        /// Accounts whose reads fail, simulating e.g. a locked keychain — for
        /// the migration-failure test.
        poisoned: Mutex<HashSet<String>>,
    }

    impl FakeItemStore {
        fn poison_reads(&self, account: &str) {
            self.poisoned
                .lock()
                .expect("poison set poisoned")
                .insert(account.to_owned());
        }

        fn unpoison(&self, account: &str) {
            self.poisoned
                .lock()
                .expect("poison set poisoned")
                .remove(account);
        }
    }

    impl ItemStore for FakeItemStore {
        fn get_item(&self, account: &str) -> Result<Option<String>, SecretError> {
            if self
                .poisoned
                .lock()
                .expect("poison set poisoned")
                .contains(account)
            {
                return Err(SecretError::Backend {
                    account: account.to_owned(),
                    source: keyring::Error::NoStorageAccess(Box::new(std::io::Error::other(
                        "simulated locked keychain",
                    ))),
                });
            }
            Ok(self
                .items
                .lock()
                .expect("fake item store poisoned")
                .get(account)
                .cloned())
        }

        fn set_item(&self, account: &str, value: &str) -> Result<(), SecretError> {
            self.items
                .lock()
                .expect("fake item store poisoned")
                .insert(account.to_owned(), value.to_owned());
            Ok(())
        }

        fn delete_item(&self, account: &str) -> Result<(), SecretError> {
            self.items
                .lock()
                .expect("fake item store poisoned")
                .remove(account);
            Ok(())
        }
    }

    #[test]
    fn route_secret_with_no_blob_reads_the_legacy_item() {
        let store = FakeItemStore::default();
        store
            .set_item("neon_api_key", "napi_legacy")
            .expect("seed legacy");
        assert_eq!(
            route_secret(&store, true, SecretKey::NeonApiKey).expect("get"),
            Some("napi_legacy".to_owned())
        );
    }

    #[test]
    fn route_secret_with_blob_present_reads_the_blob_not_the_legacy_item() {
        let store = FakeItemStore::default();
        // A stale legacy value that must NOT be what comes back.
        store
            .set_item("neon_api_key", "napi_stale")
            .expect("seed legacy");
        let mut map = std::collections::BTreeMap::new();
        map.insert("neon_api_key".to_owned(), "napi_fresh".to_owned());
        write_blob(&store, &map).expect("seed blob");

        assert_eq!(
            route_secret(&store, true, SecretKey::NeonApiKey).expect("get"),
            Some("napi_fresh".to_owned())
        );
    }

    /// CRITICAL: `set_secret` must never create the blob when it is absent —
    /// that would permanently shadow every unmigrated legacy value once a
    /// later migration attempt gives up (migration no-ops once the blob
    /// exists).
    #[test]
    fn set_secret_with_no_blob_writes_the_legacy_item_and_creates_no_blob() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        route_set_secret(&store, &blob_lock, true, SecretKey::NeonApiKey, "napi_x").expect("set");

        assert_eq!(
            store.get_item("neon_api_key").expect("legacy read"),
            Some("napi_x".to_owned())
        );
        assert_eq!(
            store.get_item(BLOB_ACCOUNT).expect("blob read"),
            None,
            "set_secret must not create the blob"
        );
    }

    #[test]
    fn set_secret_with_blob_present_updates_the_blob_not_the_legacy_item() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        write_blob(&store, &std::collections::BTreeMap::new()).expect("seed empty blob");

        route_set_secret(&store, &blob_lock, true, SecretKey::NeonApiKey, "napi_x").expect("set");

        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
        assert_eq!(
            store.get_item("neon_api_key").expect("legacy read"),
            None,
            "a blob-routed write must not also touch the legacy item"
        );
    }

    #[test]
    fn delete_secret_with_no_blob_deletes_only_the_legacy_item() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store
            .set_item("neon_api_key", "napi_x")
            .expect("seed legacy");

        route_delete_secret(&store, &blob_lock, true, SecretKey::NeonApiKey).expect("delete");

        assert_eq!(store.get_item("neon_api_key").expect("get"), None);
        assert_eq!(store.get_item(BLOB_ACCOUNT).expect("get"), None);
    }

    /// IMPORTANT: post-migration, deleting a secret must clear the legacy
    /// item too, or it resurrects the "deleted" value the next time the blob
    /// is rebuilt from legacy items.
    #[test]
    fn delete_secret_with_blob_present_clears_both_the_blob_and_the_legacy_item() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store
            .set_item("neon_api_key", "napi_stale_legacy")
            .expect("seed legacy");
        let mut map = std::collections::BTreeMap::new();
        map.insert("neon_api_key".to_owned(), "napi_current".to_owned());
        write_blob(&store, &map).expect("seed blob");

        route_delete_secret(&store, &blob_lock, true, SecretKey::NeonApiKey).expect("delete");

        let map = read_blob(&store)
            .expect("read blob")
            .expect("blob still present");
        assert!(!map.contains_key("neon_api_key"));
        assert_eq!(
            store.get_item("neon_api_key").expect("legacy read"),
            None,
            "the legacy item must also be cleared, or a blob rebuild resurrects it"
        );
    }

    #[test]
    fn the_device_key_never_enters_the_blob() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        write_blob(&store, &std::collections::BTreeMap::new()).expect("seed empty blob");

        route_set_secret(
            &store,
            &blob_lock,
            true,
            SecretKey::OpenClawDeviceKey,
            "not-really-32-bytes",
        )
        .expect("set");

        assert_eq!(
            store.get_item("openclaw_device_key").expect("legacy read"),
            Some("not-really-32-bytes".to_owned())
        );
        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert!(
            !map.contains_key("openclaw_device_key"),
            "the device key must never join the blob"
        );

        assert_eq!(
            route_secret(&store, true, SecretKey::OpenClawDeviceKey).expect("get"),
            Some("not-really-32-bytes".to_owned()),
            "reads for the device key must ignore the blob too"
        );

        route_delete_secret(&store, &blob_lock, true, SecretKey::OpenClawDeviceKey)
            .expect("delete");
        assert_eq!(store.get_item("openclaw_device_key").expect("get"), None);
    }

    #[test]
    fn consolidate_false_forces_legacy_routing_even_with_a_blob_present() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        let mut map = std::collections::BTreeMap::new();
        map.insert("neon_api_key".to_owned(), "napi_in_blob".to_owned());
        write_blob(&store, &map).expect("seed blob");

        route_set_secret(
            &store,
            &blob_lock,
            false,
            SecretKey::NeonApiKey,
            "napi_legacy",
        )
        .expect("set");
        assert_eq!(
            store.get_item("neon_api_key").expect("legacy read"),
            Some("napi_legacy".to_owned()),
            "consolidate=false must write the legacy item"
        );

        assert_eq!(
            route_secret(&store, false, SecretKey::NeonApiKey).expect("get"),
            Some("napi_legacy".to_owned()),
            "consolidate=false must read the legacy item, ignoring the blob"
        );

        route_delete_secret(&store, &blob_lock, false, SecretKey::NeonApiKey).expect("delete");
        assert_eq!(store.get_item("neon_api_key").expect("get"), None);
        // The blob is untouched throughout -- consolidate=false must not
        // read or write it at all.
        let blob = read_blob(&store).expect("read blob").expect("blob present");
        assert_eq!(blob.get("neon_api_key"), Some(&"napi_in_blob".to_owned()));
    }

    #[test]
    fn migrate_legacy_fresh_copies_legacy_values_and_writes_the_blob() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store.set_item("neon_api_key", "napi_x").expect("seed");
        store
            .set_item("github_access_token", "ghp_x")
            .expect("seed");

        let (copied, scrubbed) = route_migrate_legacy(
            &store,
            &blob_lock,
            true,
            &[SecretKey::NeonApiKey, SecretKey::GitHubAccessToken],
        )
        .expect("migrate");

        assert_eq!(copied, 2);
        assert_eq!(scrubbed, 0, "a fresh migration scrubs nothing");
        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
        assert_eq!(map.get("github_access_token"), Some(&"ghp_x".to_owned()));
    }

    #[test]
    fn migrate_legacy_re_run_is_a_no_op() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store.set_item("neon_api_key", "napi_x").expect("seed");
        route_migrate_legacy(&store, &blob_lock, true, &[SecretKey::NeonApiKey])
            .expect("first migrate");

        // Change the legacy item after migration -- a re-run must not touch
        // the blob again, so this new value must never show up in it.
        store
            .set_item("neon_api_key", "napi_changed_after_migration")
            .expect("mutate legacy");

        let (copied, scrubbed) =
            route_migrate_legacy(&store, &blob_lock, true, &[SecretKey::NeonApiKey])
                .expect("second migrate");
        assert_eq!(copied, 0);
        assert_eq!(scrubbed, 0, "the blob has no non-consolidatable accounts");

        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
    }

    #[test]
    fn migrate_legacy_with_nothing_to_copy_still_writes_an_empty_blob_and_marks_done() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());

        let (copied, scrubbed) =
            route_migrate_legacy(&store, &blob_lock, true, &[SecretKey::NeonApiKey])
                .expect("migrate");
        assert_eq!(copied, 0);
        assert_eq!(scrubbed, 0);

        let map = read_blob(&store)
            .expect("read blob")
            .expect("blob must exist even when empty");
        assert!(map.is_empty());

        // A later write must see the blob as present and route into it, not
        // fall back to legacy -- that is what "marks migration done" means.
        route_set_secret(&store, &blob_lock, true, SecretKey::NeonApiKey, "napi_x").expect("set");
        assert_eq!(
            store.get_item("neon_api_key").expect("legacy read"),
            None,
            "once the blob exists (even empty), writes must route into it"
        );
    }

    /// CRITICAL, end to end: a failed migration must write nothing, so a
    /// later `set_secret` falls through to the legacy item (never creating
    /// the blob itself -- see the other CRITICAL test above), and a
    /// subsequent migration attempt -- standing in for the next launch --
    /// still finds no blob and migrates everything, including what the
    /// interim `set_secret` wrote.
    #[test]
    fn a_failed_migration_writes_nothing_and_a_later_relaunch_still_migrates_everything() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store.set_item("neon_api_key", "napi_x").expect("seed");
        store
            .set_item("github_access_token", "ghp_x")
            .expect("seed");
        store.poison_reads("github_access_token");

        let err = route_migrate_legacy(
            &store,
            &blob_lock,
            true,
            &[SecretKey::NeonApiKey, SecretKey::GitHubAccessToken],
        )
        .expect_err("a poisoned legacy read must fail the whole migration");
        assert!(matches!(err, SecretError::Backend { .. }));
        assert_eq!(
            store.get_item(BLOB_ACCOUNT).expect("blob read"),
            None,
            "a failed migration must write nothing"
        );

        // The interim save: with no blob, this must land on the legacy item.
        route_set_secret(
            &store,
            &blob_lock,
            true,
            SecretKey::SentryUsageToken,
            "sty_x",
        )
        .expect("set");
        assert_eq!(
            store.get_item(BLOB_ACCOUNT).expect("blob read"),
            None,
            "set_secret must still create no blob"
        );

        // "Relaunch": the keychain unlocks, and migration runs again.
        store.unpoison("github_access_token");
        let (copied, scrubbed) = route_migrate_legacy(
            &store,
            &blob_lock,
            true,
            &[
                SecretKey::NeonApiKey,
                SecretKey::GitHubAccessToken,
                SecretKey::SentryUsageToken,
            ],
        )
        .expect("retry migrate");
        assert_eq!(
            copied, 3,
            "the retry must migrate everything, including the interim save"
        );
        assert_eq!(scrubbed, 0, "a fresh migration scrubs nothing");

        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
        assert_eq!(map.get("github_access_token"), Some(&"ghp_x".to_owned()));
        assert_eq!(map.get("sentry_usage_token"), Some(&"sty_x".to_owned()));
    }

    #[test]
    fn migrate_legacy_skips_the_device_key_even_when_listed() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store
            .set_item("openclaw_device_key", "raw-bytes-as-text")
            .expect("seed");
        store.set_item("neon_api_key", "napi_x").expect("seed");

        let (copied, scrubbed) = route_migrate_legacy(
            &store,
            &blob_lock,
            true,
            &[SecretKey::OpenClawDeviceKey, SecretKey::NeonApiKey],
        )
        .expect("migrate");

        assert_eq!(copied, 1, "the device key must not be counted as copied");
        assert_eq!(scrubbed, 0, "a fresh migration scrubs nothing");
        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert!(!map.contains_key("openclaw_device_key"));
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
    }

    #[test]
    fn migrate_legacy_fresh_skips_non_consolidatable_keys_even_when_listed() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store
            .set_item("azure_cost_sas_url", "sas_fresh")
            .expect("seed");
        store.set_item("neon_api_key", "napi_x").expect("seed");

        let (copied, scrubbed) =
            route_migrate_legacy(&store, &blob_lock, true, &[SecretKey::NeonApiKey])
                .expect("migrate");

        assert_eq!(copied, 1, "the SAS must not be counted as copied");
        assert_eq!(scrubbed, 0, "a fresh migration scrubs nothing");
        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert!(!map.contains_key("azure_cost_sas_url"));
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
    }

    /// Heals installs consolidated before the external-writer rule existed:
    /// the stale SAS copy is deleted from the blob, while the legacy item —
    /// the LaunchAgent's, not ours — is left strictly alone.
    #[test]
    fn migrate_legacy_scrubs_non_consolidatable_accounts_from_an_existing_blob() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store
            .set_item("azure_cost_sas_url", "sas_fresh_from_launchagent")
            .expect("seed legacy");
        let mut map = std::collections::BTreeMap::new();
        map.insert("azure_cost_sas_url".to_owned(), "sas_stale".to_owned());
        map.insert("neon_api_key".to_owned(), "napi_x".to_owned());
        write_blob(&store, &map).expect("seed blob");

        let (copied, scrubbed) =
            route_migrate_legacy(&store, &blob_lock, true, &[SecretKey::NeonApiKey])
                .expect("migrate");

        assert_eq!(copied, 0, "a scrub-only pass copies nothing");
        assert_eq!(scrubbed, 1, "the stale SAS entry was scrubbed");
        let map = read_blob(&store).expect("read blob").expect("blob present");
        assert!(!map.contains_key("azure_cost_sas_url"));
        assert_eq!(map.get("neon_api_key"), Some(&"napi_x".to_owned()));
        assert_eq!(
            store.get_item("azure_cost_sas_url").expect("legacy read"),
            Some("sas_fresh_from_launchagent".to_owned()),
            "the scrub must never touch the LaunchAgent's item"
        );
    }

    #[test]
    fn migrate_legacy_scrub_re_run_is_a_no_op() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        let mut map = std::collections::BTreeMap::new();
        map.insert("azure_cost_sas_url".to_owned(), "sas_stale".to_owned());
        map.insert("neon_api_key".to_owned(), "napi_x".to_owned());
        write_blob(&store, &map).expect("seed blob");

        let (_, first_scrubbed) =
            route_migrate_legacy(&store, &blob_lock, true, &[]).expect("first run scrubs");
        assert_eq!(first_scrubbed, 1, "the stale SAS entry was scrubbed");
        let blob_after_scrub = store.get_item(BLOB_ACCOUNT).expect("raw blob");

        let (copied, scrubbed) =
            route_migrate_legacy(&store, &blob_lock, true, &[]).expect("second run");
        assert_eq!(copied, 0);
        assert_eq!(scrubbed, 0, "a clean blob has nothing left to scrub");
        assert_eq!(
            store.get_item(BLOB_ACCOUNT).expect("raw blob"),
            blob_after_scrub,
            "a clean blob must not be rewritten"
        );
    }

    #[test]
    fn migrate_legacy_with_consolidate_false_is_a_no_op() {
        let store = FakeItemStore::default();
        let blob_lock = Mutex::new(());
        store.set_item("neon_api_key", "napi_x").expect("seed");

        let (copied, scrubbed) =
            route_migrate_legacy(&store, &blob_lock, false, &[SecretKey::NeonApiKey])
                .expect("migrate");
        assert_eq!(copied, 0);
        assert_eq!(scrubbed, 0);
        assert_eq!(
            store.get_item(BLOB_ACCOUNT).expect("blob read"),
            None,
            "consolidate=false must never create the blob"
        );
    }
}

#[cfg(test)]
mod retired_accounts {
    use super::*;

    /// The Azure Cost SAS is no longer a credential this app keeps — it mints
    /// a short-lived one per poll instead. But an install upgraded from a
    /// build that stored it may still carry the account inside its
    /// consolidated blob, and nothing else would ever remove it.
    #[test]
    fn the_retired_azure_account_is_still_scrubbed_from_an_upgraded_blob() {
        assert!(
            non_consolidatable_accounts().contains(&RETIRED_AZURE_SAS_ACCOUNT.to_owned()),
            "an upgraded install would keep a dead credential in its blob forever"
        );
    }

    /// The name is load-bearing precisely because no `SecretKey` spells it any
    /// more: a typo here is a scrub that silently matches nothing.
    #[test]
    fn the_retired_account_name_is_the_one_that_was_written() {
        assert_eq!(RETIRED_AZURE_SAS_ACCOUNT, "azure_cost_sas_url");
    }
}
