//! Does this signature actually cover this artifact, under the key the app
//! ships?
//!
//! # A deliberate re-implementation, and why that is the safe choice here
//!
//! `tauri-plugin-updater` verifies the payload it downloads before installing
//! it, so in principle nothing needs to happen at publish time. In practice a
//! feed entry whose signature does not verify is a *silent* failure: every
//! operator's launch check starts failing, the panel says "the check failed",
//! and nothing in the release that caused it looks wrong. Catching it while
//! the release is being published turns a fleet-wide silent regression into a
//! red workflow run.
//!
//! [`verify`] therefore reproduces the plugin's `verify_signature` **step for
//! step** — base64 → UTF-8 → `PublicKey::decode`, base64 → UTF-8 →
//! `Signature::decode`, then `verify(data, sig, true)`. It is not a stricter
//! check and not a looser one; anything else and this gate would either refuse
//! entries the app would have accepted or, much worse, pass entries it will
//! not. The `minisign-verify` requirement in `Cargo.toml` is the plugin's own
//! `^0.2`, so the workspace resolves ONE copy of the verifier.
//!
//! # Two layers of base64, both from the Tauri toolchain
//!
//! minisign's own file format is text. `cargo tauri signer generate` writes
//! the `.pub` file base64-encoded, and `sign_file` writes the `.sig` file
//! base64-encoded, so both values are carried through `tauri.conf.json` and
//! `latest.json` as one line of base64 wrapping the real minisign document.
//! The plugin undoes exactly that before parsing, and so does this.

use minisign_verify::{PublicKey, Signature};

/// Why a feed entry was refused.
///
/// Six variants rather than one string because they are six different
/// operator-facing problems: a mangled key is a configuration mistake, a
/// mangled signature is a broken artifact, and
/// [`Rejected`](Self::Rejected) is the one that means *this signature does not
/// cover this file* — which is either the wrong key or a tampered payload, and
/// is the only one that must never be explained away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The configured public key is not base64.
    PubKeyNotBase64(String),
    /// The configured public key is base64 of something that is not a minisign
    /// public key.
    PubKeyMalformed(String),
    /// The feed entry's signature is not base64.
    SignatureNotBase64(String),
    /// The feed entry's signature is base64 of something that is not a
    /// minisign signature.
    SignatureMalformed(String),
    /// Both parsed, and the signature does not cover this payload under this
    /// key.
    Rejected(String),
}

impl VerifyError {
    /// The repo-wide `user_message()` convention: one sentence that names what
    /// is wrong and, where it is knowable, which side is at fault.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            VerifyError::PubKeyNotBase64(e) => {
                format!("the configured update public key is not base64 ({e})")
            }
            VerifyError::PubKeyMalformed(e) => {
                format!("the configured update public key is not a minisign public key ({e})")
            }
            VerifyError::SignatureNotBase64(e) => {
                format!("the artifact's signature is not base64 ({e})")
            }
            VerifyError::SignatureMalformed(e) => {
                format!("the artifact's signature is not a minisign signature ({e})")
            }
            VerifyError::Rejected(e) => format!(
                "the signature does not cover this artifact under the shipped public key ({e}) \
                 — the artifact was signed with a different key, or it changed after signing"
            ),
        }
    }
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for VerifyError {}

/// Verify `payload` against `signature`, under `pubkey`.
///
/// `pubkey` is `tauri.conf.json`'s `plugins.updater.pubkey` verbatim, and
/// `signature` is the contents of the `.sig` file verbatim — the same two
/// strings that reach the plugin, so nothing is re-encoded on the way in.
///
/// **`Ok(())` is the only success.** There is no "could not check" outcome on
/// purpose: every way this can fail is a refusal, because a publish step that
/// treats *unable to verify* as *fine* is exactly the fail-open behaviour the
/// signature exists to prevent.
pub fn verify(pubkey: &str, signature: &str, payload: &[u8]) -> Result<(), VerifyError> {
    let key_text = decode_base64(pubkey)
        .map_err(VerifyError::PubKeyNotBase64)
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .map_err(|_| VerifyError::PubKeyNotBase64("not UTF-8 once decoded".to_string()))
        })?;
    let public_key =
        PublicKey::decode(&key_text).map_err(|e| VerifyError::PubKeyMalformed(e.to_string()))?;

    let sig_text = decode_base64(signature)
        .map_err(VerifyError::SignatureNotBase64)
        .and_then(|bytes| {
            String::from_utf8(bytes)
                .map_err(|_| VerifyError::SignatureNotBase64("not UTF-8 once decoded".to_string()))
        })?;
    let signature =
        Signature::decode(&sig_text).map_err(|e| VerifyError::SignatureMalformed(e.to_string()))?;

    // `true` is the plugin's own argument (`public_key.verify(data, &signature,
    // true)`), and it means "accept the legacy, non-prehashed algorithm too".
    // Matching it is the point: this gate must admit exactly what the app
    // admits.
    public_key
        .verify(payload, &signature, true)
        .map_err(|e| VerifyError::Rejected(e.to_string()))
}

/// The key id minisign puts in a public key's untrusted comment, e.g.
/// `B59A12A5D5E11672`.
///
/// Read out of the decoded key rather than configured separately, so the
/// workflow can print *which* key it verified under without a second place for
/// that string to be wrong.
#[must_use]
pub fn key_id(pubkey: &str) -> Option<String> {
    let text = String::from_utf8(decode_base64(pubkey).ok()?).ok()?;
    let comment = text.lines().next()?;
    comment
        .rsplit_once("minisign public key:")
        .map(|(_, id)| id.trim().to_string())
        .filter(|id| !id.is_empty())
}

fn decode_base64(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway keypair generated once with `cargo tauri signer generate`;
    /// its private half was never committed and signs nothing that exists. To
    /// regenerate: `cargo tauri signer generate -w key -f --password ""`, then
    /// `cargo tauri signer sign -f key payload.tar.gz`, keeping only
    /// `key.pub` (as `test-key.pub`), the payload and the `.sig`.
    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/updater")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    fn test_pubkey() -> String {
        String::from_utf8(fixture("test-key.pub")).expect("the .pub fixture is text")
    }

    fn test_signature() -> String {
        String::from_utf8(fixture("payload.tar.gz.sig")).expect("the .sig fixture is text")
    }

    /// The shipped key, read out of the file the app actually compiles in — not
    /// a copy. If someone rotates the key in `tauri.conf.json`, the tests below
    /// follow it rather than keeping a second opinion about what ships.
    fn shipped_pubkey() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/src-tauri/tauri.conf.json");
        let conf: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
        )
        .expect("tauri.conf.json is JSON");
        conf["plugins"]["updater"]["pubkey"]
            .as_str()
            .expect("tauri.conf.json declares plugins.updater.pubkey")
            .to_string()
    }

    /// The fixtures must reach the test as the bytes that were signed.
    ///
    /// This is not paranoia about the filesystem; it is a regression test for a
    /// real failure. With no `.gitattributes`, a Windows runner cloning under
    /// `core.autocrlf=true` rewrote `payload.tar.gz`'s single LF to CRLF — 71
    /// bytes became 72 — and the *only* symptom was the positive test below
    /// failing with `The signature verification failed`. That message points at
    /// the verifier, which was working perfectly; every negative test passed,
    /// because they ask for a rejection and a corrupted payload duly produced
    /// one.
    ///
    /// `.gitattributes` pins the files. This says so out loud, so a future
    /// checkout that unpins them fails on the sentence "a CR arrived" rather
    /// than on a sentence about cryptography. **Not a substitute for the pin** —
    /// it cannot repair the bytes, only name what happened to them.
    #[test]
    fn the_fixtures_arrive_as_the_bytes_that_were_signed() {
        for name in ["payload.tar.gz", "payload.tar.gz.sig", "test-key.pub"] {
            let bytes = fixture(name);
            assert!(
                !bytes.contains(&b'\r'),
                "{name} contains a carriage return, so this checkout rewrote it. \
                 Nothing below can pass: the signature covers the bytes as committed. \
                 Check that `.gitattributes` still declares tests/fixtures/updater/** -text."
            );
        }
    }

    #[test]
    fn a_signature_over_the_payload_verifies_under_its_own_key() {
        assert_eq!(
            verify(
                &test_pubkey(),
                &test_signature(),
                &fixture("payload.tar.gz")
            ),
            Ok(())
        );
    }

    /// **The acceptance case.** An updater that accepts an unsigned or
    /// mis-signed payload is worse than no updater, so the refusal is asserted
    /// rather than inferred from the happy path passing.
    #[test]
    fn a_payload_that_changed_after_signing_is_rejected() {
        let mut tampered = fixture("payload.tar.gz");
        // One byte. The whole point of a signature is that this is enough.
        tampered[0] ^= 0x01;

        let err = verify(&test_pubkey(), &test_signature(), &tampered)
            .expect_err("a modified payload must not verify");
        assert!(
            matches!(err, VerifyError::Rejected(_)),
            "expected a rejection, got {err:?}"
        );
    }

    /// The other half of the same guarantee: a perfectly well-formed signature
    /// from *somebody else's* key must not pass under ours. This is the case a
    /// "does the signature parse?" check would wave through.
    #[test]
    fn a_signature_from_a_foreign_key_is_rejected_under_the_shipped_key() {
        let err = verify(
            &shipped_pubkey(),
            &test_signature(),
            &fixture("payload.tar.gz"),
        )
        .expect_err("a foreign key's signature must not verify under the shipped key");
        assert!(
            matches!(err, VerifyError::Rejected(_)),
            "expected a rejection, got {err:?}"
        );
    }

    #[test]
    fn an_empty_signature_is_refused_rather_than_ignored() {
        // The shape a feed built from a missing `.sig` file would have. It must
        // be an error, never an entry that simply carries no signature.
        let err = verify(&test_pubkey(), "", &fixture("payload.tar.gz"))
            .expect_err("an empty signature must not verify");
        assert!(
            matches!(err, VerifyError::SignatureMalformed(_)),
            "expected a malformed-signature refusal, got {err:?}"
        );
    }

    #[test]
    fn a_mangled_public_key_is_refused_rather_than_ignored() {
        let err = verify("not base64 at all!!", &test_signature(), b"whatever")
            .expect_err("a mangled key must not verify");
        assert!(
            matches!(err, VerifyError::PubKeyNotBase64(_)),
            "expected a base64 refusal, got {err:?}"
        );
    }

    /// The key the bundle ships must be the one #305 generated and put in
    /// Doppler `solador/prd`. A different key id here means every release
    /// signed by CI would be refused by every installed app, and the first
    /// symptom would be a fleet that silently stops updating.
    #[test]
    fn the_shipped_public_key_is_the_one_provisioned_for_releases() {
        let pubkey = shipped_pubkey();
        assert_eq!(key_id(&pubkey).as_deref(), Some("B59A12A5D5E11672"));
        // And it is a key the verifier can actually load — a well-formed
        // base64 string that decodes to something minisign rejects would pass
        // the assertion above and fail on every operator's machine.
        assert!(
            PublicKey::decode(
                &String::from_utf8(decode_base64(&pubkey).expect("base64")).expect("utf-8")
            )
            .is_ok(),
            "the shipped pubkey must parse as a minisign public key"
        );
    }

    #[test]
    fn key_id_is_absent_rather_than_invented_when_the_comment_says_nothing() {
        use base64::Engine as _;
        let odd = base64::engine::general_purpose::STANDARD.encode("untrusted comment: hello\nX\n");
        assert_eq!(key_id(&odd), None);
        assert_eq!(key_id("}}}not base64"), None);
    }
}
