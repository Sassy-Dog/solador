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
}

/// The real thing: `keyring` over the platform credential store.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    service: String,
}

impl KeyringStore {
    /// A store bound to [`SERVICE`] — what the app uses.
    #[must_use]
    pub fn new() -> Self {
        KeyringStore {
            service: SERVICE.to_owned(),
        }
    }

    /// A store bound to a caller-supplied service name. Exists so a
    /// throwaway service can be used when someone deliberately exercises the
    /// real OS store; the app itself always uses [`KeyringStore::new`].
    #[must_use]
    pub fn with_service(service: impl Into<String>) -> Self {
        KeyringStore {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretError> {
        keyring::Entry::new(&self.service, account).map_err(|source| SecretError::Backend {
            account: account.to_owned(),
            source,
        })
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        KeyringStore::new()
    }
}

impl CredentialStore for KeyringStore {
    fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        let account = key.account();
        match self.entry(&account)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(source) => Err(SecretError::Backend { account, source }),
        }
    }

    fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        let account = key.account();
        self.entry(&account)?
            .set_password(value)
            .map_err(|source| SecretError::Backend { account, source })
    }

    fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError> {
        let account = key.account();
        match self.entry(&account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(source) => Err(SecretError::Backend { account, source }),
        }
    }
}

/// An in-memory [`CredentialStore`] for tests — nothing here reaches the OS.
///
/// Public because the code that *consumes* credentials (the later Settings/poll
/// wiring) has to be testable without prompting for a keychain unlock.
#[derive(Default)]
pub struct MemoryCredentialStore {
    entries: Mutex<HashMap<String, String>>,
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
    fn secret(&self, key: SecretKey) -> Result<Option<String>, SecretError> {
        let entries = self.entries.lock().expect("credential map poisoned");
        Ok(entries.get(&key.account()).cloned())
    }

    fn set_secret(&self, key: SecretKey, value: &str) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().expect("credential map poisoned");
        entries.insert(key.account(), value.to_owned());
        Ok(())
    }

    fn delete_secret(&self, key: SecretKey) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().expect("credential map poisoned");
        entries.remove(&key.account());
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
}
