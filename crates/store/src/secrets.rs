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
//! Nothing here logs, formats, or `Debug`-prints a secret value. Errors carry
//! the account name only.

use std::collections::HashMap;
use std::sync::Mutex;

use uuid::Uuid;

/// Keychain/Credential-Manager service name, identical to the Swift app's
/// (`KeychainHelper.serviceName`).
pub const SERVICE: &str = "com.sassydog.devcanopy";

/// The one keychain item all text secrets live in (a JSON map keyed by
/// [`SecretKey::account`] strings). One item means one ACL, so one
/// "Always Allow" covers every secret. The OpenClaw device key stays its own
/// item: raw bytes, and an account name two apps agree on.
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
    /// Container-scoped SAS URL for the Azure cost export. The URL *is* the
    /// credential, which is why it lives here and not in [`crate::Settings`].
    AzureCostSasUrl,
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
            SecretKey::AzureCostSasUrl => "azure_cost_sas_url".to_owned(),
            SecretKey::OpenClawBearerToken => "openclaw_bearer_token".to_owned(),
            SecretKey::OpenClawDeviceKey => "openclaw_device_key".to_owned(),
        }
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

/// The real thing: `keyring` over the platform credential store.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
    /// Serializes blob read-modify-write. Per-item writes never raced each
    /// other; a shared blob can (poll loop vs. settings commands).
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

    /// Reads and parses the consolidated blob item. `Ok(None)` means the item
    /// does not exist yet (pre-migration, or a fresh install).
    fn read_blob(&self) -> Result<Option<std::collections::BTreeMap<String, String>>, SecretError> {
        match self.entry(BLOB_ACCOUNT)?.get_password() {
            Ok(text) => parse_blob(&text).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(SecretError::Backend {
                account: BLOB_ACCOUNT.to_owned(),
                source,
            }),
        }
    }

    /// Writes the consolidated blob item wholesale.
    fn write_blob(
        &self,
        map: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), SecretError> {
        self.entry(BLOB_ACCOUNT)?
            .set_password(&serialize_blob(map))
            .map_err(|source| SecretError::Backend {
                account: BLOB_ACCOUNT.to_owned(),
                source,
            })
    }

    /// The pre-consolidation per-item read: one keychain entry per
    /// [`SecretKey::account`]. Still the only path for
    /// [`SecretKey::OpenClawDeviceKey`], and the fallback for any other key
    /// before it has been migrated into the blob.
    fn legacy_secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        let account = key.account();
        match self.entry(&account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(SecretError::Backend { account, source }),
        }
    }

    /// The pre-consolidation per-item write. See
    /// [`KeyringStore::legacy_secret`].
    fn legacy_set(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        let account = key.account();
        self.entry(&account)?
            .set_password(value)
            .map_err(|source| SecretError::Backend { account, source })
    }

    /// The pre-consolidation per-item delete. See
    /// [`KeyringStore::legacy_secret`].
    fn legacy_delete(&self, key: SecretKey) -> Result<(), SecretError> {
        let account = key.account();
        match self.entry(&account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(source) => Err(SecretError::Backend { account, source }),
        }
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        KeyringStore::new()
    }
}

impl CredentialStore for KeyringStore {
    fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        if matches!(key, SecretKey::OpenClawDeviceKey) {
            return self.legacy_secret(key);
        }
        match self.read_blob()? {
            Some(map) => Ok(map.get(&key.account()).cloned()),
            // Not migrated yet: the legacy item is still the truth.
            None => self.legacy_secret(key),
        }
    }

    fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        if matches!(key, SecretKey::OpenClawDeviceKey) {
            return self.legacy_set(key, value);
        }
        let _guard = self.blob_lock.lock().expect("blob lock poisoned");
        let mut map = self.read_blob()?.unwrap_or_default();
        map.insert(key.account(), value.to_owned());
        self.write_blob(&map)
    }

    fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError> {
        if matches!(key, SecretKey::OpenClawDeviceKey) {
            return self.legacy_delete(key);
        }
        let _guard = self.blob_lock.lock().expect("blob lock poisoned");
        match self.read_blob()? {
            Some(mut map) => {
                map.remove(&key.account());
                self.write_blob(&map)
            }
            // Not migrated yet: clear the legacy item, or it resurfaces at
            // migration time.
            None => self.legacy_delete(key),
        }
    }

    /// Raw bytes stay per-item for every key: [`SecretKey::OpenClawDeviceKey`]
    /// is their only consumer, and it never enters the blob (see
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
    use super::*;

    #[test]
    fn service_matches_the_swift_keychain_helper() {
        assert_eq!(SERVICE, "com.sassydog.devcanopy");
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
        assert_eq!(SecretKey::AzureCostSasUrl.account(), "azure_cost_sas_url");
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
            SecretKey::AzureCostSasUrl,
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
            SecretKey::AzureCostSasUrl.account(),
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
}
