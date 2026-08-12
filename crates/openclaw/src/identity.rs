//! Ed25519 device identity for OpenClaw gateway pairing.
//!
//! Rust port of `OpenClawDeviceIdentity`
//! (which itself ports periclaw's `device_identity.rs`, matching OpenClaw's
//! `buildDeviceAuthPayload` v2). The keypair is generated once and its 32-byte
//! seed persisted through a [`DeviceKeyStore`]; the gateway operator approves
//! the resulting [`DeviceIdentity::device_id`] out-of-band
//! (`openclaw devices approve <requestId>`), after which connects authenticate
//! silently by signing the challenge nonce.
//!
//! The load-bearing detail is the payload string, not the crypto: any drift in
//! the delimiter, the scope joining, or the empty-token field makes the gateway
//! reject every connect with no useful diagnostic. [`SIGNED_PAYLOAD_VERSION`]
//! and [`DeviceIdentity::signed_payload`] exist so that string is testable on
//! its own, and `tests` pins it byte-for-byte.
//!
//! Nothing in this module logs, `Debug`-prints, or `Display`s a seed, a private
//! key, or a signature. [`DeviceIdentity`]'s `Debug` is hand-written for that
//! reason.

use std::error::Error as StdError;
use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// Account name the device seed is stored under in the OS credential store,
/// identical to the original app's (`KeychainHelper.saveOpenClawDeviceKey`). A
/// [`DeviceKeyStore`] implementation that talks to the real keyring must use
/// this, or the two apps generate separate identities and the operator has to
/// approve the device twice.
pub const DEVICE_KEY_ACCOUNT: &str = "openclaw_device_key";

/// Prefix of the signed connect payload — the protocol's *auth* version, which
/// is independent of the WS protocol version (3, see [`crate::protocol`]).
pub const SIGNED_PAYLOAD_VERSION: &str = "v2";

/// Length of the raw Ed25519 seed (and of the raw public key).
pub const SEED_LEN: usize = 32;

/// The 32-byte Ed25519 seed. Aliased so signatures read as intent rather than
/// as an anonymous byte array.
pub type DeviceSeed = [u8; SEED_LEN];

/// An Ed25519 device keypair plus the two derived strings the gateway sees.
///
/// Cloneable because the reconnect loop hands a copy to each session; the
/// private key never leaves this type except as a signature.
#[derive(Clone)]
pub struct DeviceIdentity {
    signing_key: SigningKey,
    device_id: String,
    public_key_base64url: String,
}

/// Hand-written so a `{:?}` of an identity — in a log line, a panic message, an
/// error chain — can never print key material. A derived `Debug` would print
/// the seed, because `SigningKey` implements `Debug`.
impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .field("public_key_base64url", &self.public_key_base64url)
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    /// Build an identity from a known 32-byte seed.
    #[must_use]
    pub fn from_seed(seed: &DeviceSeed) -> Self {
        DeviceIdentity::from_signing_key(SigningKey::from_bytes(seed))
    }

    fn from_signing_key(signing_key: SigningKey) -> Self {
        let public = signing_key.verifying_key().to_bytes();
        DeviceIdentity {
            device_id: hex::encode(Sha256::digest(public)),
            public_key_base64url: URL_SAFE_NO_PAD.encode(public),
            signing_key,
        }
    }

    /// Generate a fresh identity from OS entropy.
    ///
    /// # Errors
    /// Returns [`EntropyError`] when the platform RNG is unavailable. Callers
    /// on a machine in that state cannot pair at all, so this surfaces rather
    /// than silently substituting a weak key.
    pub fn generate() -> Result<Self, EntropyError> {
        let mut seed: DeviceSeed = [0u8; SEED_LEN];
        getrandom::fill(&mut seed).map_err(|source| EntropyError { source })?;
        Ok(DeviceIdentity::from_seed(&seed))
    }

    /// Lowercase hex of SHA-256(raw 32-byte public key), 64 chars. Stable
    /// across runs for a given seed; this is what the operator approves.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// base64url-no-pad of the raw 32-byte public key (43 chars).
    #[must_use]
    pub fn public_key_base64url(&self) -> &str {
        &self.public_key_base64url
    }

    /// The 32-byte seed, for persisting through a [`DeviceKeyStore`].
    ///
    /// Named to make call sites obviously security-relevant. Do not log the
    /// result, and do not put it anywhere a `Debug` could reach.
    #[must_use]
    pub fn seed_bytes(&self) -> DeviceSeed {
        self.signing_key.to_bytes()
    }

    /// The exact bytes the gateway reconstructs and verifies:
    ///
    /// ```text
    /// v2|{deviceId}|{clientId}|{clientMode}|{role}|{scopes,joined}|{signedAtMs}|{token}|{nonce}
    /// ```
    ///
    /// An absent token serializes as an **empty field**, not a dropped
    /// delimiter — the gateway signs the empty string for anonymous connects.
    #[must_use]
    pub fn signed_payload(&self, params: &SignConnectParams<'_>) -> String {
        format!(
            "{version}|{device}|{client}|{mode}|{role}|{scopes}|{signed_at}|{token}|{nonce}",
            version = SIGNED_PAYLOAD_VERSION,
            device = self.device_id,
            client = params.client_id,
            mode = params.client_mode,
            role = params.role,
            scopes = params.scopes.join(","),
            signed_at = params.signed_at_ms,
            token = params.token.unwrap_or(""),
            nonce = params.nonce,
        )
    }

    /// Sign the v2 connect payload, returning the base64url-no-pad signature.
    ///
    /// The caller must echo the exact `signed_at_ms` and `nonce` in the connect
    /// frame's `device` block, since the gateway rebuilds the payload from
    /// them. [`crate::protocol::connect_frame`] does that in one step.
    #[must_use]
    pub fn sign_connect(&self, params: &SignConnectParams<'_>) -> String {
        let payload = self.signed_payload(params);
        URL_SAFE_NO_PAD.encode(self.signing_key.sign(payload.as_bytes()).to_bytes())
    }
}

/// Inputs to the OpenClaw v2 connect signature, bundled so the signing API
/// stays one parameter (mirrors periclaw's and the original's `SignConnectParams`).
#[derive(Debug, Clone, Copy)]
pub struct SignConnectParams<'a> {
    pub client_id: &'a str,
    pub client_mode: &'a str,
    pub role: &'a str,
    pub scopes: &'a [&'a str],
    /// `None` and `Some("")` are equivalent on the wire: both serialize as the
    /// empty field.
    pub token: Option<&'a str>,
    pub nonce: &'a str,
    pub signed_at_ms: i64,
}

/// The platform RNG refused to produce a seed.
#[derive(Debug, thiserror::Error)]
#[error("could not read OS entropy for a device key: {source}")]
pub struct EntropyError {
    #[source]
    source: getrandom::Error,
}

/// Persistence for the 32-byte device seed.
///
/// A trait, not a concrete keyring call, for two reasons: this crate stays free
/// of an OS-credential dependency, and every consumer is testable against
/// [`MemoryDeviceKeyStore`] without prompting for a keychain unlock. The app
/// binds it to the credential store under [`DEVICE_KEY_ACCOUNT`].
///
/// `&self`, not `&mut self`: the backing store is the OS, and callers hold it
/// behind an `Arc`.
pub trait DeviceKeyStore {
    /// The stored seed, or `None` when nothing is stored yet.
    ///
    /// # Errors
    /// Returns [`DeviceKeyStoreError`] when the store itself fails. A missing
    /// entry is `Ok(None)`, and a stored value of the wrong length is *also*
    /// `Ok(None)` — an unusable seed is indistinguishable from no seed, and
    /// both mean "generate a fresh one".
    fn load_device_key(&self) -> Result<Option<DeviceSeed>, DeviceKeyStoreError>;

    /// Store (or replace) the seed.
    ///
    /// # Errors
    /// Returns [`DeviceKeyStoreError`] when the store rejects the write.
    fn save_device_key(&self, seed: &DeviceSeed) -> Result<(), DeviceKeyStoreError>;
}

/// A credential-store failure. Carries only the backend's own message — never
/// the seed, and never a partial seed.
#[derive(Debug, thiserror::Error)]
#[error("device key store failed for account {DEVICE_KEY_ACCOUNT}: {source}")]
pub struct DeviceKeyStoreError {
    #[source]
    source: Box<dyn StdError + Send + Sync>,
}

impl DeviceKeyStoreError {
    /// Wrap a backend error. Implementations of [`DeviceKeyStore`] use this so
    /// the crate needs no knowledge of their error types.
    #[must_use]
    pub fn new(source: impl Into<Box<dyn StdError + Send + Sync>>) -> Self {
        DeviceKeyStoreError {
            source: source.into(),
        }
    }
}

/// What [`load_or_create`] did, so the caller can log the interesting cases
/// without this module owning a logger.
#[derive(Debug)]
pub struct LoadedIdentity {
    pub identity: DeviceIdentity,
    /// `true` when no usable seed was stored and a new one was minted — the
    /// moment the operator has to approve the device on the gateway.
    pub generated: bool,
    /// Set when a freshly generated seed could not be persisted. Deliberately
    /// **not** fatal: the returned identity works for this session, it just
    /// won't survive a restart, which beats refusing to connect at all.
    pub persist_error: Option<DeviceKeyStoreError>,
}

/// Load the stored device key, or generate and persist a new one.
///
/// Mirrors the original's `loadOrCreate`: a read miss, a store failure, or a stored
/// value that isn't a valid 32-byte seed all fall through to generation.
///
/// # Errors
/// Returns [`EntropyError`] only when a new key was needed and the platform RNG
/// was unavailable.
pub fn load_or_create<S>(store: &S) -> Result<LoadedIdentity, EntropyError>
where
    S: DeviceKeyStore + ?Sized,
{
    if let Ok(Some(seed)) = store.load_device_key() {
        return Ok(LoadedIdentity {
            identity: DeviceIdentity::from_seed(&seed),
            generated: false,
            persist_error: None,
        });
    }
    let identity = DeviceIdentity::generate()?;
    let persist_error = store.save_device_key(&identity.seed_bytes()).err();
    Ok(LoadedIdentity {
        identity,
        generated: true,
        persist_error,
    })
}

/// The device id for the already-stored key, or `None` if none exists yet.
///
/// Lets a Settings surface show the fingerprint the operator must approve
/// *without* generating a key as a side effect.
pub fn current_device_id<S>(store: &S) -> Option<String>
where
    S: DeviceKeyStore + ?Sized,
{
    let seed = store.load_device_key().ok()??;
    Some(DeviceIdentity::from_seed(&seed).device_id)
}

/// An in-memory [`DeviceKeyStore`] for tests — nothing here reaches the OS.
///
/// Public because the code that *consumes* the identity has to be testable
/// without a keychain prompt (which fails outright on CI).
#[derive(Default)]
pub struct MemoryDeviceKeyStore {
    seed: std::sync::Mutex<Option<DeviceSeed>>,
    /// When set, every call fails with this message — the "keychain is
    /// unavailable" path, which must stay non-fatal.
    failure: Option<String>,
}

impl MemoryDeviceKeyStore {
    #[must_use]
    pub fn new() -> Self {
        MemoryDeviceKeyStore::default()
    }

    /// A store seeded with an existing key, as if a previous run had persisted
    /// one.
    #[must_use]
    pub fn with_seed(seed: DeviceSeed) -> Self {
        MemoryDeviceKeyStore {
            seed: std::sync::Mutex::new(Some(seed)),
            failure: None,
        }
    }

    /// A store whose every operation fails, for exercising the non-fatal
    /// persist path.
    #[must_use]
    pub fn failing(message: impl Into<String>) -> Self {
        MemoryDeviceKeyStore {
            seed: std::sync::Mutex::new(None),
            failure: Some(message.into()),
        }
    }

    /// Whether a seed is currently stored. Deliberately returns a bool, never
    /// the seed.
    #[must_use]
    pub fn has_seed(&self) -> bool {
        self.seed
            .lock()
            .expect("device key mutex poisoned")
            .is_some()
    }
}

/// Hand-written for the same reason as [`DeviceIdentity`]'s: a derived `Debug`
/// would print the seed.
impl fmt::Debug for MemoryDeviceKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryDeviceKeyStore")
            .field("has_seed", &self.has_seed())
            .field("failing", &self.failure.is_some())
            .finish()
    }
}

impl DeviceKeyStore for MemoryDeviceKeyStore {
    fn load_device_key(&self) -> Result<Option<DeviceSeed>, DeviceKeyStoreError> {
        if let Some(message) = &self.failure {
            return Err(DeviceKeyStoreError::new(message.clone()));
        }
        Ok(*self.seed.lock().expect("device key mutex poisoned"))
    }

    fn save_device_key(&self, seed: &DeviceSeed) -> Result<(), DeviceKeyStoreError> {
        if let Some(message) = &self.failure {
            return Err(DeviceKeyStoreError::new(message.clone()));
        }
        *self.seed.lock().expect("device key mutex poisoned") = Some(*seed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    use super::*;

    /// The original tests' fixed seeds: `Data(repeating: <byte>, count: 32)`.
    fn seed(byte: u8) -> DeviceSeed {
        [byte; SEED_LEN]
    }

    fn params<'a>(
        scopes: &'a [&'a str],
        token: Option<&'a str>,
        nonce: &'a str,
        signed_at_ms: i64,
    ) -> SignConnectParams<'a> {
        SignConnectParams {
            client_id: "openclaw-tui",
            client_mode: "ui",
            role: "operator",
            scopes,
            token,
            nonce,
            signed_at_ms,
        }
    }

    fn decode_base64url(s: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(s).expect("valid base64url-no-pad")
    }

    fn verifying_key(identity: &DeviceIdentity) -> VerifyingKey {
        let raw: [u8; 32] = decode_base64url(identity.public_key_base64url())
            .try_into()
            .expect("32-byte public key");
        VerifyingKey::from_bytes(&raw).expect("valid Ed25519 public key")
    }

    fn signature(encoded: &str) -> Signature {
        let raw: [u8; 64] = decode_base64url(encoded)
            .try_into()
            .expect("64-byte signature");
        Signature::from_bytes(&raw)
    }

    // MARK: byte-exact fixtures
    //
    // Every constant below was produced by this implementation and then
    // independently confirmed against Apple CryptoKit — the shipped original app's
    // crypto — for the same seed: identical device id, identical public key,
    // and `Curve25519.Signing.PublicKey.isValidSignature` accepting the
    // ed25519-dalek signature below over the payload below (and rejecting it
    // over a tampered payload).
    //
    // CryptoKit's own signing is hedged (randomized), so *signature bytes* can
    // only ever be pinned on the deterministic RFC 8032 side. That is exactly
    // why this fixture lives here and not in the counterpart, which can only
    // assert verification.

    /// SHA-256 hex of the public key for the all-zero seed.
    const SEED0_DEVICE_ID: &str =
        "139e3940e64b5491722088d9a0d741628fc826e09475d341a780acde3c4b8070";
    const SEED0_PUBLIC_KEY: &str = "O2onvM62pC1io6jQKm8Nc2UyFXcd4kOmOsBIoYtZ2ik";
    const SEED0_PAYLOAD: &str = "v2|139e3940e64b5491722088d9a0d741628fc826e09475d341a780acde3c4b8070|openclaw-tui|ui|operator|operator.read|1776000000000|tok|nonce-xyz";
    const SEED0_SIGNATURE: &str =
        "cB8KAlNCGz9HjrlbVMdBHNWjAEZqIh3XG5xC8Ngk7zaA5hiOHgxAEtSzSXbXOQnWjV-FlqUbzHMae5FKARRIBA";

    /// The empty-token case: `...|42||n2`, with a two-scope join.
    const SEED3_PAYLOAD: &str = "v2|b62e867fa2f33afe62d5d6b1642e1621d543307846b2a57b897e710919b76709|openclaw-tui|ui|operator|operator.read,operator.admin|42||n2";
    const SEED3_SIGNATURE: &str =
        "rNEl47xY_F1LwF0QS2aQpUsVv0uA7TIXjieXAg3bolf_9Yb597-RvrLThH51OPUMM1vTNfGi4DTT7q9bmxDzAg";

    #[test]
    fn signed_payload_matches_the_openclaw_v2_shape() {
        let identity = DeviceIdentity::from_seed(&seed(0));
        let scopes = ["operator.read"];
        let payload = identity.signed_payload(&params(
            &scopes,
            Some("tok"),
            "nonce-xyz",
            1_776_000_000_000,
        ));
        assert_eq!(payload, SEED0_PAYLOAD);
    }

    #[test]
    fn absent_token_serializes_as_an_empty_field() {
        // token == None must produce `...|signedAt||nonce` (empty field), not a
        // dropped delimiter — the gateway signs the empty string for tokenless
        // connects, and a dropped `|` silently invalidates every signature.
        let identity = DeviceIdentity::from_seed(&seed(3));
        let scopes = ["operator.read", "operator.admin"];
        let payload = identity.signed_payload(&params(&scopes, None, "n2", 42));
        assert_eq!(payload, SEED3_PAYLOAD);
    }

    #[test]
    fn empty_token_is_identical_to_no_token() {
        let identity = DeviceIdentity::from_seed(&seed(3));
        let scopes = ["operator.read", "operator.admin"];
        assert_eq!(
            identity.signed_payload(&params(&scopes, Some(""), "n2", 42)),
            identity.signed_payload(&params(&scopes, None, "n2", 42))
        );
    }

    #[test]
    fn signature_is_byte_identical_to_the_pinned_fixture() {
        // RFC 8032 determinism is the whole reason this can be pinned: the same
        // seed and the same payload bytes must produce these exact signature
        // bytes on every platform and every run.
        let identity = DeviceIdentity::from_seed(&seed(0));
        let scopes = ["operator.read"];
        assert_eq!(
            identity.sign_connect(&params(
                &scopes,
                Some("tok"),
                "nonce-xyz",
                1_776_000_000_000
            )),
            SEED0_SIGNATURE
        );

        let identity = DeviceIdentity::from_seed(&seed(3));
        let scopes = ["operator.read", "operator.admin"];
        assert_eq!(
            identity.sign_connect(&params(&scopes, None, "n2", 42)),
            SEED3_SIGNATURE
        );
    }

    #[test]
    fn signature_verifies_against_the_exact_payload() {
        // The counterpart's check, kept because it is the one that fails loudly
        // if the payload format drifts *and* the fixture is regenerated to
        // match: verification is over independently-spelled expected bytes.
        let identity = DeviceIdentity::from_seed(&seed(0));
        let scopes = ["operator.read"];
        let encoded = identity.sign_connect(&params(
            &scopes,
            Some("tok"),
            "nonce-xyz",
            1_776_000_000_000,
        ));
        verifying_key(&identity)
            .verify(SEED0_PAYLOAD.as_bytes(), &signature(&encoded))
            .expect("sign_connect must sign the exact v2 payload");
    }

    #[test]
    fn wrong_payload_fails_verification() {
        // Guard the guard: a signature over the real payload must NOT verify
        // against a tampered payload (proves the check above has teeth).
        let identity = DeviceIdentity::from_seed(&seed(5));
        let scopes = ["operator.read"];
        let encoded = identity.sign_connect(&params(&scopes, Some("t"), "n", 7));
        let tampered = format!(
            "v2|{}|openclaw-tui|ui|operator|operator.read|7|t|DIFFERENT",
            identity.device_id()
        );
        assert!(verifying_key(&identity)
            .verify(tampered.as_bytes(), &signature(&encoded))
            .is_err());
    }

    #[test]
    fn device_id_is_lowercase_sha256_hex_of_the_public_key() {
        let identity = DeviceIdentity::from_seed(&seed(1));
        assert_eq!(
            identity.device_id().len(),
            64,
            "SHA-256 is 32 bytes -> 64 hex chars"
        );
        assert!(identity
            .device_id()
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        // Pinned against the fixture so a hashing change is a test failure, not
        // a silent re-pairing request for every existing install.
        assert_eq!(
            DeviceIdentity::from_seed(&seed(0)).device_id(),
            SEED0_DEVICE_ID
        );
    }

    #[test]
    fn public_key_is_43_char_base64url_no_pad() {
        let identity = DeviceIdentity::from_seed(&seed(2));
        let encoded = identity.public_key_base64url();
        assert_eq!(encoded.len(), 43, "32 bytes base64url-no-pad is 43 chars");
        assert!(!encoded.contains('='), "url-safe encoding drops padding");
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));

        assert_eq!(
            DeviceIdentity::from_seed(&seed(0)).public_key_base64url(),
            SEED0_PUBLIC_KEY
        );
    }

    #[test]
    fn seed_round_trips_through_the_store() {
        let store = MemoryDeviceKeyStore::new();
        let first = load_or_create(&store).expect("entropy");
        assert!(first.generated);
        assert!(first.persist_error.is_none());
        assert!(store.has_seed());

        let second = load_or_create(&store).expect("entropy");
        assert!(!second.generated, "a stored seed must be reused");
        assert_eq!(second.identity.device_id(), first.identity.device_id());
        assert_eq!(
            second.identity.public_key_base64url(),
            first.identity.public_key_base64url()
        );
    }

    #[test]
    fn a_failing_store_still_yields_a_usable_identity() {
        // The original treats a Keychain write failure as non-fatal; so do we. The
        // device just has to be re-approved after a restart.
        let store = MemoryDeviceKeyStore::failing("keychain locked");
        let loaded = load_or_create(&store).expect("entropy");
        assert!(loaded.generated);
        let error = loaded.persist_error.expect("persist error surfaced");
        assert!(error.to_string().contains(DEVICE_KEY_ACCOUNT));
        assert_eq!(loaded.identity.device_id().len(), 64);
    }

    #[test]
    fn current_device_id_does_not_generate() {
        let store = MemoryDeviceKeyStore::new();
        assert_eq!(current_device_id(&store), None);
        assert!(
            !store.has_seed(),
            "reading the fingerprint must not mint a key"
        );

        let store = MemoryDeviceKeyStore::with_seed(seed(0));
        assert_eq!(current_device_id(&store).as_deref(), Some(SEED0_DEVICE_ID));
    }

    #[test]
    fn debug_never_prints_key_material() {
        let identity = DeviceIdentity::from_seed(&seed(7));
        let rendered = format!("{identity:?}");
        assert!(rendered.contains(identity.device_id()));
        assert!(rendered.contains(identity.public_key_base64url()));
        // A derived `Debug` would render the `SigningKey` field, and dalek
        // prints the seed as a byte array. Both markers are absent only if the
        // hand-written impl is still in place.
        assert!(
            !rendered.contains("signing_key"),
            "Debug leaked the key field: {rendered}"
        );
        assert!(
            !rendered.contains('['),
            "Debug rendered a byte array: {rendered}"
        );

        let store = MemoryDeviceKeyStore::with_seed(seed(7));
        let rendered = format!("{store:?}");
        assert!(rendered.contains("has_seed: true"));
        assert!(
            !rendered.contains('[') && !rendered.contains('7'),
            "Debug leaked a seed: {rendered}"
        );
    }
}
