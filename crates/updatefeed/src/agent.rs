//! The agent's update feed: `agent-latest.json`, and the checks that have to
//! pass before it may be published (#391, part of #381).
//!
//! # Not the app's `latest.json`
//!
//! `tauri-plugin-updater` owns [`crate::manifest`]'s document on a schema it
//! controls, and the desktop feed is verified through the plugin's own
//! double-base64 convention ([`crate::signature`]). The agent has no Tauri in
//! it: its binaries are signed with **plain minisign** under a **separate
//! keypair** (`agent/release-signing-key.pub`), so this module verifies with
//! `minisign-verify` directly and shares nothing with the desktop path but the
//! CalVer rule. Passing a plain `.minisig` into [`crate::signature::verify`] would
//! be refused as "not base64", and the reverse would be refused as "not a
//! minisign signature" — see the test that proves both directions.
//!
//! # The wire contract, as `docs/AGENT-DISTRIBUTION.md` §2 states it
//!
//! ```json
//! {
//!   "version": "2026.9.9",
//!   "targets": {
//!     "aarch64-apple-darwin": {
//!       "url": "https://github.com/Sassy-Dog/solador/releases/download/v2026.9.9/solador-agent-2026.9.9-aarch64-apple-darwin",
//!       "signature": "untrusted comment: …\nRW…\ntrusted comment: …\n…\n",
//!       "sha256": "<64 lowercase hex over the raw executable bytes>"
//!     },
//!     …
//!   }
//! }
//! ```
//!
//! - `version` is the release's CalVer with no `v` prefix — the same number
//!   the release, the app and every binary carry.
//! - `targets` is keyed by the full Rust triple and carries **exactly** the
//!   four [`TARGETS`]. No archives, no app-style `darwin-*` aliases, no
//!   Windows agent.
//! - `signature` is the `.minisig` text verbatim, `sha256` is over the raw
//!   executable bytes, and the `url` names `solador-agent-<version>-<triple>`
//!   on the release's tag.
//! - Every signature's **trusted comment is the asset's own file name**
//!   (`scripts/agent-signing.sh` signs with `-t <basename>`), and the producer
//!   refuses one that names anything else — a signature that verifies over
//!   its bytes but was made for another file is a mislabelled artifact.
//!
//! The document is signed as a whole — `agent-latest.json.minisig`, a plain
//! detached minisign signature over the **exact bytes served**, final newline
//! included. A consumer verifies those bytes *before* decoding JSON and never
//! verifies a re-serialised object (`verify_pair`), because two serialisers
//! agreeing on whitespace is not a property anyone should have to defend.
//!
//! # Why the content hash is in the feed at all
//!
//! Agent and app share one version, so an app-only release still moves the
//! agent's number. The hash is what lets an installed agent answer "did the
//! binary change?" without downloading it: equal hash means stop. It is
//! computed here **from the verified bytes**, never taken from a caller, so a
//! feed cannot carry a hash of something its signature does not cover.
//!
//! # Every refusal is a refusal
//!
//! There is no "could not check" outcome. A missing target, a signature from
//! another key, a byte that moved after signing, a URL naming the wrong asset
//! — each is an `Err`, and [`Feed::build`] cannot return a document that
//! carries one. The workflow uploads only what this module has verified.

use std::collections::BTreeMap;

use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::is_calver;

/// The name every agent binary is published under, before its version and
/// triple. `scripts/build-agent.sh` names the files, `release.yml` attaches
/// them, and this is what the feed's URLs have to point at.
pub const BINARY_PREFIX: &str = "solador-agent";

/// The feed's own asset name on the release, and the name the consumer
/// fetches through `releases/latest/download/`.
pub const FEED_ASSET: &str = "agent-latest.json";

/// The four published targets — exactly these, in `scripts/config.sh`'s
/// order. A feed with a fifth key or a missing one is refused, because a
/// consumer that cannot find its own triple has no update, not a different
/// one.
pub const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

/// `solador-agent-<version>-<triple>`: the release asset a feed entry names.
#[must_use]
pub fn asset_name(version: &str, target: &str) -> String {
    format!("{BINARY_PREFIX}-{version}-{target}")
}

/// Is this the `YYYY.M.P` CalVer a release carries? The same rule the
/// desktop feed applies, exposed so a caller can refuse a version *before*
/// going looking for assets named after it.
pub fn check_version(version: &str) -> Result<(), FeedError> {
    if is_calver(version) {
        Ok(())
    } else {
        Err(FeedError::Version(version.to_string()))
    }
}

/// One raw binary as it arrived off disk, with the signature that came beside
/// it. The input to [`Feed::build`]; nothing here has been checked yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// The full Rust target triple this binary was built for.
    pub target: String,
    /// Where the binary will be downloadable from once the release is public.
    pub url: String,
    /// The raw executable bytes — the thing that is signed and hashed.
    pub bytes: Vec<u8>,
    /// The contents of `<binary>.minisig`, verbatim.
    pub signature: String,
}

/// One feed entry: everything a consumer needs to fetch, verify and compare a
/// binary for its target.
///
/// Field order is the serialised order — `serde` writes struct fields as
/// declared — which is what makes [`Feed::to_bytes`] byte-stable without
/// depending on which `serde_json` features the rest of the workspace happens
/// to unify in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Absolute `https://` URL of the raw binary on the release's tag.
    pub url: String,
    /// The binary's `.minisig`, verbatim — a plain minisign signature text.
    pub signature: String,
    /// SHA-256 over the raw executable bytes, 64 lowercase hex characters.
    pub sha256: String,
}

/// An `agent-latest.json`.
///
/// [`Feed::build`] returns one only from verified inputs and [`Feed::parse`]
/// only from bytes that passed the shape check; the fields are public so the
/// workflow can print them and a test can probe them, which also means a
/// hand-assembled `Feed` is possible. Nothing in this type stops one being
/// serialised and signed — the guarantee that only a *built* feed is signed
/// sits in the workflow's step order (`build` writes, the signer signs that
/// file, `verify` re-checks every binary against it before upload), not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Feed {
    /// The release's CalVer, no `v`.
    pub version: String,
    /// Triple → entry. `BTreeMap` so two runs over the same release produce
    /// the same bytes, and so the signature over those bytes is reproducible.
    pub targets: BTreeMap<String, Target>,
}

/// Why a feed could not be built, parsed or verified.
///
/// One variant per operator-facing problem rather than one string, because a
/// missing target is a release that was assembled wrong, a foreign signature
/// is a mis-provisioned key, and a byte that moved is the one nobody may
/// explain away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedError {
    /// Not the `YYYY.M.P` CalVer the release carries.
    Version(String),
    /// The tag does not name the version (`v<version>` is the repo's scheme,
    /// and a feed whose assets sit under some other tag advertises 404s).
    Tag { tag: String, version: String },
    /// The committed public key file is not a minisign public key.
    PubKeyMalformed(String),
    /// An input for a triple that is not published.
    UnknownTarget(String),
    /// Two inputs for one triple.
    DuplicateTarget(String),
    /// Fewer than all four targets. Never publishable: the missing triple's
    /// hosts would read "no update" from a release that has one.
    MissingTargets(Vec<String>),
    /// A URL that is not absolute `https://`, or does not name the asset.
    Url {
        target: String,
        url: String,
        reason: String,
    },
    /// A `.minisig` (or the feed's own) that is not a minisign signature.
    SignatureMalformed { object: String, reason: String },
    /// Parsed, and does not cover these bytes under this key — the wrong key,
    /// or a payload that changed after signing.
    Rejected { object: String, reason: String },
    /// The signature verifies, and its trusted comment names some other
    /// file. `scripts/agent-signing.sh` signs every artifact with `-t
    /// <basename>` so a signature is self-describing; a signature that
    /// covers these bytes but was made for another asset is a mislabelled
    /// artifact, and the label is what a consumer picks by.
    TrustedComment { object: String, found: String },
    /// A feed entry's `sha256` is not 64 lowercase hex characters.
    Sha256Field { target: String, value: String },
    /// A downloaded binary's bytes hash to something other than the feed's
    /// entry says, under a signature that verified — the entry and the file
    /// disagree, and neither is trusted.
    HashMismatch {
        target: String,
        expected: String,
        actual: String,
    },
    /// The bytes are not the document the contract describes.
    Json(String),
}

impl FeedError {
    /// The repo-wide `user_message()` convention: one sentence naming what is
    /// wrong and, where it is knowable, what to do about it.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            FeedError::Version(v) => format!(
                "'{v}' is not the CalVer scripts/get-version-info.sh emits \
                 (YYYY.M.<commits-this-month>, no leading zeroes, no 'v')"
            ),
            FeedError::Tag { tag, version } => format!(
                "tag '{tag}' does not name version '{version}' — the feed's URLs would point at \
                 assets under a different release"
            ),
            FeedError::PubKeyMalformed(e) => format!(
                "the agent public key is not a minisign public key ({e}) — \
                 agent/release-signing-key.pub is the committed file, two lines, the first \
                 'untrusted comment:'"
            ),
            FeedError::UnknownTarget(t) => format!(
                "'{t}' is not a published agent target — the four are {}",
                TARGETS.join(", ")
            ),
            FeedError::DuplicateTarget(t) => {
                format!("two binaries were offered for '{t}' — refusing to pick one")
            }
            FeedError::MissingTargets(ts) => format!(
                "{} target(s) missing: {} — a feed is all four or nothing, because a triple \
                 that is absent reads as 'no update' on every host that runs it",
                ts.len(),
                ts.join(", ")
            ),
            FeedError::Url {
                target,
                url,
                reason,
            } => format!("the {target} URL '{url}' is refused: {reason}"),
            FeedError::SignatureMalformed { object, reason } => format!(
                "{object}'s signature is not a minisign signature ({reason}) — the agent's \
                 signatures are plain minisign text, never the app's base64-wrapped form"
            ),
            FeedError::Rejected { object, reason } => format!(
                "{object}'s signature does not cover its bytes under the committed agent key \
                 ({reason}) — it was signed with a different key, or it changed after signing"
            ),
            FeedError::TrustedComment { object, found } => format!(
                "{object}'s signature was made for '{found}' — the bytes verify, but under a \
                 signature that names a different file, so this is not the artifact it claims to be"
            ),
            FeedError::Sha256Field { target, value } => {
                format!("the {target} entry's sha256 '{value}' is not 64 lowercase hex characters")
            }
            FeedError::HashMismatch {
                target,
                expected,
                actual,
            } => format!(
                "the {target} binary hashes to {actual}, but its feed entry says {expected} — \
                 the file is not the one the feed describes"
            ),
            FeedError::Json(e) => format!("not an agent feed document: {e}"),
        }
    }
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for FeedError {}

impl Feed {
    /// Build the feed from four raw binaries and their signatures, verifying
    /// each under `pubkey` (the text of `agent/release-signing-key.pub`) and
    /// hashing the verified bytes.
    ///
    /// `tag` is the release the URLs point into, and it must be `v<version>`.
    /// Every refusal happens here: nothing else constructs a `Feed` from
    /// inputs, so a `Feed` in hand is one whose every entry verified.
    pub fn build(
        version: &str,
        tag: &str,
        pubkey: &str,
        inputs: Vec<Input>,
    ) -> Result<Self, FeedError> {
        check_version(version)?;
        if tag != format!("v{version}") {
            return Err(FeedError::Tag {
                tag: tag.to_string(),
                version: version.to_string(),
            });
        }
        let key = decode_pubkey(pubkey)?;

        let mut targets: BTreeMap<String, Target> = BTreeMap::new();
        for input in inputs {
            if !TARGETS.contains(&input.target.as_str()) {
                return Err(FeedError::UnknownTarget(input.target));
            }
            if targets.contains_key(&input.target) {
                return Err(FeedError::DuplicateTarget(input.target));
            }
            let asset = asset_name(version, &input.target);
            check_url(&input.target, &input.url, tag, &asset)?;

            // Verify BEFORE hashing. The hash is a claim about bytes the
            // signature has already bound to us; hashing first and verifying
            // second would produce the same result, but it would let a future
            // edit return early with a hash of something unverified.
            verify_with(&key, &asset, &input.signature, &input.bytes)?;
            let sha256 = sha256_hex(&input.bytes);

            targets.insert(
                input.target,
                Target {
                    url: input.url,
                    signature: input.signature,
                    sha256,
                },
            );
        }

        let missing: Vec<String> = TARGETS
            .iter()
            .filter(|t| !targets.contains_key(**t))
            .map(|t| (*t).to_string())
            .collect();
        if !missing.is_empty() {
            return Err(FeedError::MissingTargets(missing));
        }

        Ok(Feed {
            version: version.to_string(),
            targets,
        })
    }

    /// The exact bytes to sign and serve: pretty-printed, two-space indent,
    /// one trailing newline. Byte-stable across runs — struct fields serialise
    /// in declared order and the targets are a `BTreeMap` — which is what
    /// makes "the signature covers the served bytes" a checkable claim.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        // A struct with String and BTreeMap fields cannot fail to serialise.
        let mut json = serde_json::to_string_pretty(self)
            .expect("a Feed serialises: strings and a map of strings");
        json.push('\n');
        json.into_bytes()
    }

    /// Decode a served document and hold it to the contract: exactly the
    /// four targets, a CalVer version, absolute `https://` URLs naming the
    /// right asset, 64-hex hashes, and signatures that at least parse.
    ///
    /// This is the *shape* check. It does not verify anything cryptographic —
    /// [`verify_pair`] does that over the raw bytes first, and calls this
    /// second, which is the order a consumer must use too.
    pub fn parse(bytes: &[u8]) -> Result<Self, FeedError> {
        let feed: Feed =
            serde_json::from_slice(bytes).map_err(|e| FeedError::Json(e.to_string()))?;

        if !is_calver(&feed.version) {
            return Err(FeedError::Version(feed.version));
        }
        let tag = format!("v{}", feed.version);

        for target in feed.targets.keys() {
            if !TARGETS.contains(&target.as_str()) {
                return Err(FeedError::UnknownTarget(target.clone()));
            }
        }
        let missing: Vec<String> = TARGETS
            .iter()
            .filter(|t| !feed.targets.contains_key(**t))
            .map(|t| (*t).to_string())
            .collect();
        if !missing.is_empty() {
            return Err(FeedError::MissingTargets(missing));
        }

        for (target, entry) in &feed.targets {
            let asset = asset_name(&feed.version, target);
            check_url(target, &entry.url, &tag, &asset)?;
            if !is_sha256_hex(&entry.sha256) {
                return Err(FeedError::Sha256Field {
                    target: target.clone(),
                    value: entry.sha256.clone(),
                });
            }
            Signature::decode(&entry.signature).map_err(|e| FeedError::SignatureMalformed {
                object: asset,
                reason: e.to_string(),
            })?;
        }

        Ok(feed)
    }
}

/// The consumer's check, in the consumer's order: verify the **exact served
/// bytes** of `agent-latest.json` against its detached `.minisig` under the
/// committed key, and only then decode them.
///
/// Verifying a re-serialised object instead of the bytes is the mistake this
/// signature's design rules out — two serialisers agreeing on whitespace is
/// not something anyone should have to defend — so this function takes bytes,
/// never a `Feed`.
pub fn verify_pair(
    pubkey: &str,
    feed_bytes: &[u8],
    feed_signature: &str,
) -> Result<Feed, FeedError> {
    let key = decode_pubkey(pubkey)?;
    verify_with(&key, FEED_ASSET, feed_signature, feed_bytes)?;
    Feed::parse(feed_bytes)
}

/// Does this feed entry describe these bytes? Signature under `pubkey`
/// **and** hash equality — both, because they answer different questions:
/// the signature says *we* published these bytes, the hash says they are the
/// bytes *this feed* is talking about.
pub fn verify_binary(
    pubkey: &str,
    version: &str,
    target: &str,
    entry: &Target,
    bytes: &[u8],
) -> Result<(), FeedError> {
    let key = decode_pubkey(pubkey)?;
    verify_with(&key, &asset_name(version, target), &entry.signature, bytes)?;
    let actual = sha256_hex(bytes);
    if actual != entry.sha256 {
        return Err(FeedError::HashMismatch {
            target: target.to_string(),
            expected: entry.sha256.clone(),
            actual,
        });
    }
    Ok(())
}

/// Verify `payload` against a plain minisign `signature` under `pubkey`, as
/// a signature made for the file named `object`.
///
/// Both strings are file contents verbatim — the `.pub` and the `.minisig` —
/// with no base64 layer to undo. **`Ok(())` is the only success.**
pub fn verify(
    pubkey: &str,
    object: &str,
    signature: &str,
    payload: &[u8],
) -> Result<(), FeedError> {
    let key = decode_pubkey(pubkey)?;
    verify_with(&key, object, signature, payload)
}

/// SHA-256 of `bytes`, as the feed spells it: 64 lowercase hex characters.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The key id minisign prints for a public key — e.g. `B2E5C62B763FD2C4` —
/// read out of the **key bytes**, not the comment above them.
///
/// The comment is untrusted by minisign's own naming, and it is prose a human
/// typed; the id is eight bytes of the key itself, printed as minisign does
/// (little-endian, uppercase hex). A workflow prints this so a reader knows
/// which key verified without a second place for the string to be wrong.
pub fn key_id(pubkey: &str) -> Result<String, FeedError> {
    use base64::Engine as _;
    let line = pubkey
        .lines()
        .nth(1)
        .ok_or_else(|| FeedError::PubKeyMalformed("no key line".to_string()))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(line.trim())
        .map_err(|e| FeedError::PubKeyMalformed(e.to_string()))?;
    if raw.len() != 42 {
        return Err(FeedError::PubKeyMalformed(format!(
            "{} bytes, expected 42",
            raw.len()
        )));
    }
    let mut id = raw[2..10].to_vec();
    id.reverse();
    Ok(hex::encode_upper(id))
}

fn decode_pubkey(pubkey: &str) -> Result<PublicKey, FeedError> {
    PublicKey::decode(pubkey).map_err(|e| FeedError::PubKeyMalformed(e.to_string()))
}

fn verify_with(
    key: &PublicKey,
    object: &str,
    signature: &str,
    payload: &[u8],
) -> Result<(), FeedError> {
    let sig = Signature::decode(signature).map_err(|e| FeedError::SignatureMalformed {
        object: object.to_string(),
        reason: e.to_string(),
    })?;
    // `false`: prehashed signatures only. The pinned `rsign2` and the
    // reference `minisign` both prehash by default, and a consumer verifying
    // a multi-megabyte binary will stream it — which `minisign-verify` only
    // supports for prehashed signatures. Refusing the legacy algorithm here
    // is what guarantees a consumer never meets one.
    key.verify(payload, &sig, false)
        .map_err(|e| FeedError::Rejected {
            object: object.to_string(),
            reason: e.to_string(),
        })?;
    // The trusted comment is covered by the global signature (the verify
    // above checked it), so it is the signer's own statement of which file
    // this is. Two binaries with identical bytes under two triples cannot
    // happen — every build embeds its target — but a `.minisig` copied beside
    // the wrong asset can, and this is what catches it.
    if sig.trusted_comment() != object {
        return Err(FeedError::TrustedComment {
            object: object.to_string(),
            found: sig.trusted_comment().to_string(),
        });
    }
    Ok(())
}

/// `https://<host>/<owner>/<repo>/releases/download/<tag>/<asset>` — GitHub's
/// release-asset layout, checked **positionally** rather than by suffix.
///
/// A suffix check accepted `…/download/v2026.9.9/v2026.9.9/solador-agent-…`
/// (a `--download-base` that already carried the tag) and
/// `https://example.invalid/v2026.9.9/solador-agent-…` (no release path at
/// all). The contract says the URL names the asset *on the release's tag*,
/// and this is the only URL shape that does.
fn check_url(target: &str, url: &str, tag: &str, asset: &str) -> Result<(), FeedError> {
    let refuse = |reason: &str| FeedError::Url {
        target: target.to_string(),
        url: url.to_string(),
        reason: reason.to_string(),
    };
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| refuse("not an absolute https:// URL"))?;
    if rest.chars().any(char::is_whitespace) {
        return Err(refuse("contains whitespace"));
    }
    let segments: Vec<&str> = rest.split('/').collect();
    // host, owner, repo, "releases", "download", tag, asset — and nothing
    // else, in that order.
    let expected_tail = ["releases", "download", tag, asset];
    if segments.len() != 7
        || segments[..3].iter().any(|s| s.is_empty())
        || segments[3..] != expected_tail
    {
        return Err(refuse(&format!(
            "not https://<host>/<owner>/<repo>/releases/download/{tag}/{asset}"
        )));
    }
    Ok(())
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway minisign keypair generated once with the pinned `rsign2`
    /// (`rsign generate -W -p test-agent-key.pub -s key`); its private half
    /// was never committed and signs nothing that exists. The four "binaries"
    /// are short text files standing in for executables — the verifier does
    /// not care what the bytes are, only that they are the bytes signed — and
    /// `agent-latest.json` + `.minisig` are what `solador-agent-feed build`
    /// produced from them and `rsign` signed, so the committed pair is one
    /// this code and the release signer agree on. `tests/fixtures/README.md`
    /// says how to regenerate the set.
    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/agent")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    fn text(name: &str) -> String {
        String::from_utf8(fixture(name)).unwrap_or_else(|_| panic!("{name} is text"))
    }

    const VERSION: &str = "2026.9.9";
    const TAG: &str = "v2026.9.9";
    const BASE: &str = "https://github.com/Sassy-Dog/solador/releases/download";

    fn test_pubkey() -> String {
        text("test-agent-key.pub")
    }

    /// The key the release actually signs under — read from the committed
    /// file, not copied, so a rotation moves these tests with it.
    fn shipped_pubkey() -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../agent/release-signing-key.pub");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    fn input(target: &str) -> Input {
        let asset = asset_name(VERSION, target);
        Input {
            target: target.to_string(),
            url: format!("{BASE}/{TAG}/{asset}"),
            bytes: fixture(&asset),
            signature: text(&format!("{asset}.minisig")),
        }
    }

    fn inputs() -> Vec<Input> {
        TARGETS.iter().map(|t| input(t)).collect()
    }

    fn feed() -> Feed {
        Feed::build(VERSION, TAG, &test_pubkey(), inputs()).expect("the fixtures build a feed")
    }

    /// Same regression guard as the desktop fixtures carry: a Windows checkout
    /// under `core.autocrlf=true` rewrites LF to CRLF, and the only symptom is
    /// every positive test below failing on a sentence about cryptography.
    /// `.gitattributes` pins `tests/fixtures/agent/**`; this names what
    /// happened if it stops.
    #[test]
    fn the_fixtures_arrive_as_the_bytes_that_were_signed() {
        let mut names = vec![
            "test-agent-key.pub".to_string(),
            FEED_ASSET.to_string(),
            format!("{FEED_ASSET}.minisig"),
        ];
        for t in TARGETS {
            names.push(asset_name(VERSION, t));
            names.push(format!("{}.minisig", asset_name(VERSION, t)));
        }
        for name in names {
            assert!(
                !fixture(&name).contains(&b'\r'),
                "{name} contains a carriage return, so this checkout rewrote it. \
                 Check that `.gitattributes` still declares tests/fixtures/agent/** -text."
            );
        }
    }

    // --- Building -----------------------------------------------------------

    /// The acceptance item: four targets, each URL naming its asset on the
    /// tag, each hash over the raw bytes, each signature verbatim.
    #[test]
    fn four_verified_targets_build_a_feed_that_obeys_the_contract() {
        let feed = feed();
        assert_eq!(feed.version, VERSION);
        assert_eq!(feed.targets.len(), 4);
        for t in TARGETS {
            let entry = feed.targets.get(t).unwrap_or_else(|| panic!("{t} present"));
            let asset = asset_name(VERSION, t);
            assert_eq!(entry.url, format!("{BASE}/{TAG}/{asset}"));
            assert_eq!(entry.sha256, sha256_hex(&fixture(&asset)));
            assert_eq!(entry.signature, text(&format!("{asset}.minisig")));
        }
    }

    /// The committed `agent-latest.json` is what `build` produces from the
    /// committed binaries — byte for byte. That is the determinism claim
    /// (two runs, one document), and it is what makes the committed
    /// `.minisig` a signature over *this code's* output rather than over a
    /// document that happened to look similar.
    #[test]
    fn the_serialised_bytes_are_deterministic_and_match_the_signed_fixture() {
        let bytes = feed().to_bytes();
        assert_eq!(bytes, feed().to_bytes(), "two builds, one document");
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            text(FEED_ASSET),
            "the committed feed fixture is not what build() emits — regenerate the pair \
             (tests/fixtures/README.md) rather than editing either half"
        );
        assert!(bytes.ends_with(b"}\n"), "one trailing newline, no more");
    }

    #[test]
    fn version_is_first_and_targets_are_sorted_by_triple() {
        let json = String::from_utf8(feed().to_bytes()).expect("utf-8");
        assert!(json.starts_with("{\n  \"version\": \"2026.9.9\",\n  \"targets\": {\n"));
        let positions: Vec<usize> = [
            "aarch64-apple-darwin",
            "aarch64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "x86_64-unknown-linux-musl",
        ]
        .iter()
        .map(|t| {
            json.find(&format!("\"{t}\": {{"))
                .unwrap_or_else(|| panic!("{t} present"))
        })
        .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "BTreeMap order");
    }

    #[test]
    fn sha256_is_exact_over_known_bytes() {
        // FIPS 180-2 test vector.
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn a_missing_target_is_refused_rather_than_published_short() {
        let three: Vec<Input> = inputs().into_iter().take(3).collect();
        let err = Feed::build(VERSION, TAG, &test_pubkey(), three).expect_err("three of four");
        assert_eq!(
            err,
            FeedError::MissingTargets(vec!["x86_64-apple-darwin".to_string()])
        );
        // And an empty set names all four, rather than building an empty feed.
        let err = Feed::build(VERSION, TAG, &test_pubkey(), vec![]).expect_err("none");
        assert!(matches!(err, FeedError::MissingTargets(ref m) if m.len() == 4));
    }

    #[test]
    fn a_duplicate_or_unknown_target_is_refused() {
        let mut twice = inputs();
        twice.push(input("aarch64-apple-darwin"));
        assert_eq!(
            Feed::build(VERSION, TAG, &test_pubkey(), twice).expect_err("duplicate"),
            FeedError::DuplicateTarget("aarch64-apple-darwin".to_string())
        );

        let mut extra = inputs();
        extra.push(Input {
            target: "x86_64-pc-windows-msvc".to_string(),
            ..input("x86_64-apple-darwin")
        });
        assert_eq!(
            Feed::build(VERSION, TAG, &test_pubkey(), extra).expect_err("no Windows agent"),
            FeedError::UnknownTarget("x86_64-pc-windows-msvc".to_string())
        );
        // The app's own platform keys are not agent targets either.
        let mut alias = inputs();
        alias.push(Input {
            target: "darwin-aarch64".to_string(),
            ..input("aarch64-apple-darwin")
        });
        assert!(matches!(
            Feed::build(VERSION, TAG, &test_pubkey(), alias),
            Err(FeedError::UnknownTarget(_))
        ));
    }

    /// **The load-bearing negative.** One byte moved after signing; the feed
    /// must not carry a hash of it.
    #[test]
    fn a_binary_that_changed_after_signing_is_refused() {
        let mut ins = inputs();
        ins[0].bytes[0] ^= 0x01;
        let err = Feed::build(VERSION, TAG, &test_pubkey(), ins).expect_err("tampered");
        assert!(
            matches!(err, FeedError::Rejected { ref object, .. } if object == &asset_name(VERSION, TARGETS[0])),
            "expected a rejection naming the asset, got {err:?}"
        );
    }

    /// A perfectly well-formed signature from *somebody else's* key does not
    /// pass under the key the release publishes — here the real committed key,
    /// which did not sign these fixtures.
    #[test]
    fn a_signature_from_a_foreign_key_is_refused_under_the_shipped_key() {
        let err = Feed::build(VERSION, TAG, &shipped_pubkey(), inputs()).expect_err("foreign key");
        assert!(
            matches!(err, FeedError::Rejected { .. }),
            "expected a rejection, got {err:?}"
        );
    }

    /// A signature lifted from one binary onto another is a signature that
    /// parses and does not cover — the case a "does it decode?" check would
    /// wave through.
    #[test]
    fn a_signature_lifted_from_another_binary_is_refused() {
        let mut ins = inputs();
        let other = ins[1].signature.clone();
        ins[0].signature = other;
        assert!(matches!(
            Feed::build(VERSION, TAG, &test_pubkey(), ins),
            Err(FeedError::Rejected { .. })
        ));
    }

    #[test]
    fn an_empty_or_mangled_signature_is_refused_rather_than_ignored() {
        for bad in ["", "untrusted comment: nothing\n", "not a signature at all"] {
            let mut ins = inputs();
            ins[2].signature = bad.to_string();
            assert!(
                matches!(
                    Feed::build(VERSION, TAG, &test_pubkey(), ins),
                    Err(FeedError::SignatureMalformed { .. })
                ),
                "{bad:?} should have been refused as malformed"
            );
        }
    }

    /// The two feeds' signature conventions are not interchangeable, in
    /// either direction — proven, since the temptation to route one through
    /// the other's verifier is exactly what this module's existence resists.
    #[test]
    fn the_app_verifier_and_the_agent_verifier_refuse_each_others_signatures() {
        let desktop_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/updater");
        let read = |n: &str| std::fs::read(desktop_dir.join(n)).expect("desktop fixture");
        let desktop_sig = String::from_utf8(read("payload.tar.gz.sig")).expect("text");
        let desktop_key = String::from_utf8(read("test-key.pub")).expect("text");

        // Tauri's base64-wrapped .sig into the plain verifier: one line, no
        // trusted comment — not a minisign signature.
        assert!(matches!(
            verify(
                &test_pubkey(),
                "payload.tar.gz",
                &desktop_sig,
                &read("payload.tar.gz")
            ),
            Err(FeedError::SignatureMalformed { .. })
        ));
        // And the Tauri-style key is base64 of the .pub text, so as a .pub
        // text it has one line and no key line.
        assert!(matches!(
            verify(
                &desktop_key,
                "x",
                &text("solador-agent-2026.9.9-aarch64-apple-darwin.minisig"),
                b"x"
            ),
            Err(FeedError::PubKeyMalformed(_))
        ));
        // The reverse: a plain .minisig into the app's verifier is not base64.
        let asset = asset_name(VERSION, "aarch64-apple-darwin");
        assert!(matches!(
            crate::signature::verify(
                &desktop_key,
                &text(&format!("{asset}.minisig")),
                &fixture(&asset)
            ),
            Err(crate::signature::VerifyError::SignatureNotBase64(_))
        ));
    }

    #[test]
    fn a_version_or_tag_the_contract_forbids_is_refused() {
        for bad in ["v2026.9.9", "2026.09.9", "2026.9", "", "26.9.9"] {
            assert!(
                matches!(
                    Feed::build(bad, TAG, &test_pubkey(), inputs()),
                    Err(FeedError::Version(_))
                ),
                "{bad:?} should have been refused"
            );
        }
        // The tag has to name the version, or the URLs point into some other
        // release.
        assert_eq!(
            Feed::build(VERSION, "v2026.9.8", &test_pubkey(), inputs()).expect_err("wrong tag"),
            FeedError::Tag {
                tag: "v2026.9.8".to_string(),
                version: VERSION.to_string()
            }
        );
    }

    #[test]
    fn a_url_that_does_not_name_the_asset_on_the_tag_is_refused() {
        let asset = asset_name(VERSION, "aarch64-apple-darwin");
        for bad in [
            "".to_string(),
            "https://".to_string(),
            format!("http://github.com/x/{TAG}/{asset}"),
            format!("https:///{TAG}/{asset}"),
            format!("{BASE}/{TAG}/{asset}.tar.gz"),
            format!("{BASE}/v2026.9.8/{asset}"),
            format!("{BASE}/{TAG}/solador-agent-2026.9.9-x86_64-apple-darwin"),
            format!("{BASE}/{TAG}/{asset} "),
            // A --download-base that already carried the tag: the right
            // suffix, one segment too many.
            format!("{BASE}/{TAG}/{TAG}/{asset}"),
            // No release path at all — a suffix check waved this through.
            format!("https://example.invalid/{TAG}/{asset}"),
            // The right shape on the wrong path.
            format!("https://github.com/Sassy-Dog/solador/releases/assets/{TAG}/{asset}"),
            format!("https:///solador/releases/download/{TAG}/{asset}"),
            // An empty owner or repo segment: seven segments, one of them
            // nothing.
            format!("https://github.com//solador/releases/download/{TAG}/{asset}"),
            format!("https://github.com/Sassy-Dog//releases/download/{TAG}/{asset}"),
        ] {
            let mut ins = inputs();
            ins[2].url = bad.clone();
            assert!(
                matches!(
                    Feed::build(VERSION, TAG, &test_pubkey(), ins),
                    Err(FeedError::Url { .. })
                ),
                "{bad:?} should have been refused"
            );
        }
    }

    #[test]
    fn a_mangled_public_key_is_refused_before_anything_is_checked() {
        assert!(matches!(
            Feed::build(VERSION, TAG, "not a key", inputs()),
            Err(FeedError::PubKeyMalformed(_))
        ));
        assert!(matches!(
            verify_pair(
                "untrusted comment: x\nnotbase64!!\n",
                &fixture(FEED_ASSET),
                ""
            ),
            Err(FeedError::PubKeyMalformed(_))
        ));
    }

    // --- The signed pair ---------------------------------------------------

    /// The consumer's exact-served-bytes verification succeeds on the
    /// committed pair, and yields a feed with the four entries.
    #[test]
    fn the_signed_feed_pair_verifies_over_its_exact_bytes() {
        let feed = verify_pair(
            &test_pubkey(),
            &fixture(FEED_ASSET),
            &text(&format!("{FEED_ASSET}.minisig")),
        )
        .expect("the committed pair verifies");
        assert_eq!(feed, self::feed());
    }

    /// The feed's signature covers the bytes *as served*: strip the final
    /// newline, or touch one character, and it no longer does. Re-serialising
    /// would hide exactly this, which is why the consumer never does it.
    #[test]
    fn a_feed_whose_served_bytes_changed_is_refused_before_decoding() {
        let sig = text(&format!("{FEED_ASSET}.minisig"));
        let mut without_newline = fixture(FEED_ASSET);
        assert_eq!(without_newline.pop(), Some(b'\n'));
        assert!(matches!(
            verify_pair(&test_pubkey(), &without_newline, &sig),
            Err(FeedError::Rejected { ref object, .. }) if object == FEED_ASSET
        ));

        let mut edited = fixture(FEED_ASSET);
        // Turn a lowercase hex digit in a hash into another — still valid
        // JSON, still a valid-looking feed, no longer the signed bytes.
        let idx = String::from_utf8_lossy(&edited)
            .find("\"sha256\": \"")
            .expect("a hash")
            + "\"sha256\": \"".len();
        edited[idx] = if edited[idx] == b'0' { b'1' } else { b'0' };
        assert!(matches!(
            verify_pair(&test_pubkey(), &edited, &sig),
            Err(FeedError::Rejected { .. })
        ));
    }

    #[test]
    fn the_feed_pair_does_not_verify_under_the_shipped_key() {
        assert!(matches!(
            verify_pair(
                &shipped_pubkey(),
                &fixture(FEED_ASSET),
                &text(&format!("{FEED_ASSET}.minisig"))
            ),
            Err(FeedError::Rejected { .. })
        ));
    }

    #[test]
    fn a_feed_entry_verifies_its_binary_and_refuses_a_swapped_one() {
        let feed = feed();
        let target = "x86_64-unknown-linux-musl";
        let entry = &feed.targets[target];
        verify_binary(
            &test_pubkey(),
            VERSION,
            target,
            entry,
            &fixture(&asset_name(VERSION, target)),
        )
        .expect("the entry describes its binary");
        // The right signature text with the wrong bytes.
        let other = fixture(&asset_name(VERSION, "aarch64-unknown-linux-musl"));
        assert!(matches!(
            verify_binary(&test_pubkey(), VERSION, target, entry, &other),
            Err(FeedError::Rejected { .. })
        ));
        // A hash the signature does not agree with — an entry edited after
        // signing, which the feed signature would have caught first, but this
        // check stands on its own.
        let mut lying = entry.clone();
        lying.sha256 = sha256_hex(b"something else");
        assert!(matches!(
            verify_binary(
                &test_pubkey(),
                VERSION,
                target,
                &lying,
                &fixture(&asset_name(VERSION, target))
            ),
            Err(FeedError::HashMismatch { .. })
        ));
    }

    /// Another binary's bytes AND its signature, offered under this triple:
    /// the signature genuinely covers those bytes, so only the trusted
    /// comment — which the signer set to the asset's own name — says they
    /// are the wrong file. A review probe built exactly this feed before the
    /// check existed: an x86_64 entry carrying the aarch64 hash.
    #[test]
    fn a_signature_made_for_another_asset_is_refused_even_though_it_verifies() {
        let mut ins = inputs();
        let donor = input("aarch64-unknown-linux-musl");
        ins[0].bytes = donor.bytes.clone();
        ins[0].signature = donor.signature.clone();
        let err = Feed::build(VERSION, TAG, &test_pubkey(), ins).expect_err("mislabelled");
        assert_eq!(
            err,
            FeedError::TrustedComment {
                object: asset_name(VERSION, TARGETS[0]),
                found: asset_name(VERSION, "aarch64-unknown-linux-musl"),
            }
        );
        // And on the consumer side, through the entry.
        let feed = feed();
        let entry = feed.targets["aarch64-unknown-linux-musl"].clone();
        assert!(matches!(
            verify_binary(
                &test_pubkey(),
                VERSION,
                "x86_64-unknown-linux-musl",
                &entry,
                &donor.bytes
            ),
            Err(FeedError::TrustedComment { .. })
        ));
        // The feed's own signature is held to the same rule: a binary's
        // signature offered as the feed's names the binary, not the feed.
        assert!(matches!(
            verify_pair(&test_pubkey(), &donor.bytes, &donor.signature),
            Err(FeedError::TrustedComment { .. })
        ));
    }

    /// The Rust copy of the target list is the one that refuses at publish
    /// time; `scripts/config.sh` is the one `build-agent.sh` builds from.
    /// They are asserted equal rather than trusted to be, the way
    /// `shipped_pubkey()` reads the real key file.
    #[test]
    fn the_targets_are_the_ones_config_sh_builds() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/config.sh");
        let config = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        let value = |name: &str| -> Vec<String> {
            let prefix = format!("export {name}=\"");
            config
                .lines()
                .find_map(|l| l.strip_prefix(&prefix))
                .unwrap_or_else(|| panic!("{name} not exported by config.sh"))
                .trim_end_matches('"')
                .split_whitespace()
                .map(str::to_string)
                .collect()
        };
        let mut from_config = value("AGENT_LINUX_TARGETS");
        from_config.extend(value("AGENT_MACOS_TARGETS"));
        assert_eq!(
            from_config, TARGETS,
            "TARGETS and scripts/config.sh disagree"
        );
    }

    // --- Parsing -----------------------------------------------------------

    #[test]
    fn parse_holds_a_document_to_the_contract() {
        let good = fixture(FEED_ASSET);
        let doc: serde_json::Value = serde_json::from_slice(&good).expect("json");

        let refused = |mutate: &dyn Fn(&mut serde_json::Value)| {
            let mut d = doc.clone();
            mutate(&mut d);
            let bytes = serde_json::to_vec(&d).expect("json");
            Feed::parse(&bytes).expect_err("should refuse")
        };

        // A fifth target.
        assert!(matches!(
            refused(&|d| {
                let entry = d["targets"]["aarch64-apple-darwin"].clone();
                d["targets"]["x86_64-pc-windows-msvc"] = entry;
            }),
            FeedError::UnknownTarget(_)
        ));
        // A missing one.
        assert!(matches!(
            refused(&|d| {
                d["targets"]
                    .as_object_mut()
                    .unwrap()
                    .remove("aarch64-apple-darwin");
            }),
            FeedError::MissingTargets(_)
        ));
        // An extra field, at either level: held, not passed.
        assert!(matches!(
            refused(&|d| d["notes"] = serde_json::json!("hello")),
            FeedError::Json(_)
        ));
        assert!(matches!(
            refused(&|d| d["targets"]["aarch64-apple-darwin"]["size"] = serde_json::json!(1)),
            FeedError::Json(_)
        ));
        // A hash that is not 64 lowercase hex.
        assert!(matches!(
            refused(
                &|d| d["targets"]["aarch64-apple-darwin"]["sha256"] = serde_json::json!("ABCDEF")
            ),
            FeedError::Sha256Field { .. }
        ));
        // A signature that is not minisign text.
        assert!(matches!(
            refused(&|d| d["targets"]["aarch64-apple-darwin"]["signature"] =
                serde_json::json!("dW50cnVzdGVk")),
            FeedError::SignatureMalformed { .. }
        ));
        // A URL naming another version's asset.
        assert!(matches!(
            refused(&|d| {
                d["targets"]["aarch64-apple-darwin"]["url"] = serde_json::json!(
                "https://github.com/Sassy-Dog/solador/releases/download/v2026.9.8/solador-agent-2026.9.8-aarch64-apple-darwin"
            )
            }),
            FeedError::Url { .. }
        ));
        // The tag, not the version.
        assert!(matches!(
            refused(&|d| d["version"] = serde_json::json!("v2026.9.9")),
            FeedError::Version(_)
        ));
        // Not JSON at all.
        assert!(matches!(Feed::parse(b"{"), Err(FeedError::Json(_))));
        assert!(matches!(Feed::parse(b""), Err(FeedError::Json(_))));
    }

    // --- The committed key -------------------------------------------------

    /// The key the release signs under is the one provisioned in Doppler
    /// `solador/prd` as `SOLADOR_AGENT_SIGNING_PRIVATE_KEY`'s public half. A
    /// different id here means every release's signatures would be refused
    /// by every agent that ever compiles this key in.
    ///
    /// Read from the key BYTES, and cross-checked against the comment above
    /// them — `docs/AGENT-DISTRIBUTION.md` once cited an id the file never
    /// carried, which is the drift this test exists to catch.
    #[test]
    fn the_committed_agent_key_is_the_provisioned_one_and_its_comment_agrees() {
        let pubkey = shipped_pubkey();
        let id = key_id(&pubkey).expect("the committed key decodes");
        assert_eq!(id, "B2E5C62B763FD2C4");
        let comment = pubkey.lines().next().expect("an untrusted comment line");
        assert!(
            comment.contains(&id),
            "the comment '{comment}' does not name the key id {id} the bytes carry"
        );
        // And it loads in the verifier, so it is a key and not merely a
        // well-shaped string.
        assert!(PublicKey::decode(&pubkey).is_ok());
        // It is NOT the app's updater key, which is the separation §5 requires
        // — read from `tauri.conf.json` through the desktop verifier's own
        // reader, so an app-key rotation cannot make this vacuous.
        let conf_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../app/src-tauri/tauri.conf.json");
        let conf: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&conf_path).expect("tauri.conf.json"))
                .expect("json");
        let app_key_id = crate::signature::key_id(
            conf["plugins"]["updater"]["pubkey"]
                .as_str()
                .expect("plugins.updater.pubkey"),
        )
        .expect("the app key has an id");
        assert_ne!(id, app_key_id, "the agent key must not be the app's");
    }

    #[test]
    fn key_id_is_refused_rather_than_invented_for_a_mangled_key() {
        assert!(matches!(
            key_id("one line only"),
            Err(FeedError::PubKeyMalformed(_))
        ));
        assert!(matches!(
            key_id("untrusted comment: x\n}}}}\n"),
            Err(FeedError::PubKeyMalformed(_))
        ));
        assert!(matches!(
            key_id("untrusted comment: x\nAAAA\n"),
            Err(FeedError::PubKeyMalformed(_))
        ));
    }

    #[test]
    fn asset_names_follow_build_agent_sh() {
        assert_eq!(
            asset_name("2026.9.8", "x86_64-unknown-linux-musl"),
            "solador-agent-2026.9.8-x86_64-unknown-linux-musl"
        );
    }

    /// Every variant's sentence carries the thing it is about — the target,
    /// the asset, the URL — so a dropped `{placeholder}` fails here rather
    /// than in a workflow log that says "'s signature does not cover".
    #[test]
    fn every_error_has_a_user_message_that_names_the_object() {
        let cases: Vec<(FeedError, &[&str])> = vec![
            (FeedError::Version("VER".into()), &["VER"]),
            (
                FeedError::Tag {
                    tag: "TAGX".into(),
                    version: "VERX".into(),
                },
                &["TAGX", "VERX"],
            ),
            (FeedError::PubKeyMalformed("REASON".into()), &["REASON"]),
            (FeedError::UnknownTarget("TRIPLE".into()), &["TRIPLE"]),
            (FeedError::DuplicateTarget("TRIPLE".into()), &["TRIPLE"]),
            (
                FeedError::MissingTargets(vec!["T1".into(), "T2".into()]),
                &["2 target", "T1, T2"],
            ),
            (
                FeedError::Url {
                    target: "TRIPLE".into(),
                    url: "URLX".into(),
                    reason: "REASON".into(),
                },
                &["TRIPLE", "URLX", "REASON"],
            ),
            (
                FeedError::SignatureMalformed {
                    object: "OBJ".into(),
                    reason: "REASON".into(),
                },
                &["OBJ", "REASON"],
            ),
            (
                FeedError::Rejected {
                    object: "OBJ".into(),
                    reason: "REASON".into(),
                },
                &["OBJ", "REASON"],
            ),
            (
                FeedError::TrustedComment {
                    object: "OBJ".into(),
                    found: "OTHER".into(),
                },
                &["OBJ", "OTHER"],
            ),
            (
                FeedError::Sha256Field {
                    target: "TRIPLE".into(),
                    value: "VAL".into(),
                },
                &["TRIPLE", "VAL"],
            ),
            (
                FeedError::HashMismatch {
                    target: "TRIPLE".into(),
                    expected: "EXP".into(),
                    actual: "ACT".into(),
                },
                &["TRIPLE", "EXP", "ACT"],
            ),
            (FeedError::Json("REASON".into()), &["REASON"]),
        ];
        for (e, expected) in cases {
            let msg = e.user_message();
            for needle in expected {
                assert!(msg.contains(needle), "{e:?}: '{msg}' lacks '{needle}'");
            }
            assert_eq!(e.to_string(), msg, "Display is user_message");
        }
    }
}
