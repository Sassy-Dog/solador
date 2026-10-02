//! The newest **verified** agent release, as the cockpit learns it (#489).
//!
//! The agent has a release train of its own (#472). Its `agent-latest.json`
//! lives on the permanent `agent-latest` release, signed as a whole under the
//! **agent's** minisign key — the one in `agent/release-signing-key.pub`, plus
//! the standby `-next.pub` when it exists — and never the app updater's key.
//! This crate fetches that document and its detached `.minisig` and answers one
//! question: *what version is the newest release, provided the bytes verify?*
//!
//! # Verified, or nothing
//!
//! The order is the same one `agent/src/update.rs` keeps, and it is the
//! security design: the **exact served bytes** are checked against the
//! detached signature under the compiled-in trust set *before anything is
//! decoded*, and only then is `version` read and held to a strict `YYYY.M.N`
//! CalVer. A feed that is one byte off, signed under a foreign key, or signed
//! for another file never reaches the JSON parser. [`Latest::version`] is
//! therefore a claim a caller can build "your host is behind" on.
//!
//! # Every way of not knowing is its own [`Error`]
//!
//! Not published yet (a 404 on `agent-latest` — the state until the first
//! `agent-v*` release is cut), unreachable, an HTTP failure, unsigned, signature
//! rejected, malformed. The caller renders them differently and none of them is
//! "up to date"; an `Ok` is the only thing that can be compared.
//!
//! # Boundaries
//!
//! App-only. `agent/` must never resolve this crate (`scripts/agent-deps-guard.sh`
//! asserts it), and neither side depends on `crates/updatefeed`, which is the
//! feed's *producer*. The consumer verifies with `minisign-verify` directly.
//! The key set is compiled in by `build.rs` and never fetched.

use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use std::time::Duration;

include!(concat!(env!("OUT_DIR"), "/trusted_keys.rs"));

/// Where the releases live. The same constant `agent/src/update.rs` fetches
/// from; a feed's contents never choose it.
pub const RELEASE_BASE: &str = "https://github.com/Sassy-Dog/solador";

/// The permanent release the feed is attached to, and the feed's file name.
pub const FEED_RELEASE: &str = "agent-latest";
pub const FEED_ASSET: &str = "agent-latest.json";

/// What the vendor slot of `crates/fault`'s sentences says. A noun phrase with
/// its own determiner, which every template but `ToolUnavailable`'s allows.
const SOURCE: &str = "the agent release feed";

/// A feed or signature past this is not one: the real documents are about a
/// kilobyte, so the cap bounds what a hostile response can make us buffer.
const BODY_CAP: usize = 256 * 1024;

/// The reason a verified answer could not be had. Classifies; the words are
/// [`Error::user_message`]'s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 404 on `agent-latest`: nothing has been published through the feed yet.
    /// A **state**, not a failure — it is the truth until the first `agent-v*`
    /// release is published — and the only variant a caller may render neutral.
    NotPublished,
    /// The request never completed: DNS, TLS, timeout, reset, no network.
    Unreachable,
    /// The server answered with a status this crate has no sharper reading of
    /// (a 5xx, a 403). The code rides along because it is the one detail a
    /// sentence can carry that the others cannot.
    Status(u16),
    /// The feed was served and its `.minisig` was not (404): there is nothing
    /// to verify it against, so it is not trusted.
    Unsigned,
    /// The bytes do not verify under the trust set, or verify but were signed
    /// for another file.
    SignatureRejected,
    /// The bytes verified and are not a feed this build understands: not JSON,
    /// or a `version` that is not a strict `YYYY.M.N` CalVer.
    Malformed,
    /// The **compiled-in** trust set does not decode. Never a network matter —
    /// a build defect that a unit test catches first — and kept apart from
    /// [`Error::SignatureRejected`] so it cannot be mistaken for a forged feed.
    TrustSet,
}

impl Error {
    /// The one sentence for this state, written out arm by arm. Nothing a
    /// transport, a URL or a decoder produced is interpolated: the stock
    /// vocabulary is `crates/fault`'s, and what it has no word for (a signature
    /// rejected, an unsigned feed, nothing published) keeps its own sentence
    /// rather than borrowing a neighbour's.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Error::NotPublished => "no agent release published yet".to_string(),
            Error::Unreachable => fault::Fault::Unreachable.message(SOURCE),
            Error::Status(code) => fault::http_status_message(*code, SOURCE),
            Error::Unsigned => format!("{SOURCE} has no signature — not trusted"),
            Error::SignatureRejected => {
                format!("{SOURCE} failed signature verification — not trusted")
            }
            Error::Malformed => fault::Fault::Undecodable.message(SOURCE),
            Error::TrustSet => fault::Fault::Unexpected.message("the agent key check"),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for Error {}

/// The keys a feed signature may verify under: one or two, never zero.
pub struct Trust {
    keys: Vec<PublicKey>,
}

impl Trust {
    /// The agent keys `build.rs` compiled in.
    pub fn compiled_in() -> Result<Self, Error> {
        Self::from_texts(TRUSTED_PUBLIC_KEYS)
    }

    /// A trust set from public key file texts (two lines, the first an
    /// `untrusted comment:`). The seam tests inject a key through; the shell
    /// only ever uses [`Trust::compiled_in`].
    pub fn from_texts(texts: &[&str]) -> Result<Self, Error> {
        if texts.is_empty() {
            return Err(Error::TrustSet);
        }
        let keys = texts
            .iter()
            .map(|text| PublicKey::decode(text).map_err(|_| Error::TrustSet))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Trust { keys })
    }

    /// Whether `signature` (plain minisign text) covers exactly `payload` under
    /// any trusted key **and** was made for `object`. The trusted comment is
    /// itself covered by the signature, so once the bytes verify it is the
    /// signer's own statement of which file this is; a signature that verifies
    /// over these bytes but names another asset is a mislabelled artifact.
    fn verifies(&self, object: &str, signature: &str, payload: &[u8]) -> bool {
        let Ok(sig) = Signature::decode(signature) else {
            return false;
        };
        // `false`: prehashed signatures only, which both `rsign2` and the
        // reference `minisign` produce; the legacy algorithm is refused.
        self.keys
            .iter()
            .any(|key| key.verify(payload, &sig, false).is_ok())
            && sig.trusted_comment() == object
    }
}

/// A strict `YYYY.M.N` CalVer: three dotted integers, a four-digit year, no
/// leading zeroes — the shape `scripts/get-version-info.sh` writes and the
/// producer enforces.
fn is_calver(v: &str) -> bool {
    let fields: Vec<&str> = v.split('.').collect();
    fields.len() == 3
        && fields[0].len() == 4
        && fields.iter().all(|f| {
            !f.is_empty()
                && f.bytes().all(|b| b.is_ascii_digit())
                && !(f.len() > 1 && f.starts_with('0'))
                && f.parse::<u32>().is_ok()
        })
}

/// The newest release the feed names, **after** its signature verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    /// A strict CalVer, `YYYY.M.N` (no `agent-v`).
    pub version: String,
}

#[derive(Deserialize)]
struct Feed {
    version: String,
}

/// Verify `feed_bytes` against `signature` under `trust`, **then** decode.
///
/// A byte off, a foreign key, or a signature made for another file is
/// [`Error::SignatureRejected`] and the JSON is never looked at; bytes that
/// verify but carry no CalVer `version` are [`Error::Malformed`]. Pure — no I/O
/// — so each refusal is a unit test against the signed fixtures.
pub fn verify(trust: &Trust, feed_bytes: &[u8], signature: &str) -> Result<Latest, Error> {
    if !trust.verifies(FEED_ASSET, signature, feed_bytes) {
        return Err(Error::SignatureRejected);
    }
    let feed: Feed = serde_json::from_slice(feed_bytes).map_err(|_| Error::Malformed)?;
    if !is_calver(&feed.version) {
        return Err(Error::Malformed);
    }
    Ok(Latest {
        version: feed.version,
    })
}

/// `<base>/releases/download/agent-latest/<asset>`. Built from the compiled-in
/// base and nothing a feed says.
fn asset_url(base: &str, asset: &str) -> String {
    format!("{base}/releases/download/{FEED_RELEASE}/{asset}")
}

/// Plain `http://` is for the loopback servers in tests and nothing a shipped
/// build is configured with ([`RELEASE_BASE`] is a constant).
fn is_loopback_base(base: &str) -> bool {
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|prefix| {
            base.strip_prefix(prefix).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')
            })
        })
}

/// GET one asset into memory. A 404 is [`Error::NotPublished`] for the caller
/// to reinterpret (the signature's 404 is [`Error::Unsigned`], not that).
async fn get(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, Error> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| Error::Unreachable)?;
    let status = response.status();
    if status.as_u16() == 404 {
        return Err(Error::NotPublished);
    }
    if !status.is_success() {
        return Err(Error::Status(status.as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Unreachable)? {
        if body.len() + chunk.len() > BODY_CAP {
            // Not a feed. Reported as the response being unreadable rather
            // than quoting a size that would only invite parsing it.
            return Err(Error::Malformed);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Fetch the feed and its signature from `base` and verify them under `trust`.
///
/// `base` is a parameter so tests can point it at a loopback server; the
/// shell calls [`latest`].
pub async fn fetch_latest(base: &str, trust: &Trust) -> Result<Latest, Error> {
    let client = reqwest::Client::builder()
        .user_agent("solador-cockpit/agent-release-check")
        .https_only(!is_loopback_base(base))
        .redirect(reqwest::redirect::Policy::limited(5))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| Error::Unreachable)?;

    // The feed first: its 404 is the "nothing published yet" state.
    let feed = get(&client, &asset_url(base, FEED_ASSET)).await?;
    let signature = match get(&client, &asset_url(base, &format!("{FEED_ASSET}.minisig"))).await {
        Ok(bytes) => bytes,
        // A feed with no signature beside it is a different finding from no
        // feed at all.
        Err(Error::NotPublished) => return Err(Error::Unsigned),
        Err(other) => return Err(other),
    };
    let signature = String::from_utf8(signature).map_err(|_| Error::SignatureRejected)?;
    verify(trust, &feed, &signature)
}

/// The production read: the real release base, the compiled-in agent keys.
pub async fn latest() -> Result<Latest, Error> {
    fetch_latest(RELEASE_BASE, &Trust::compiled_in()?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/agent-v");

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!("{FIXTURES}/{name}")).expect("fixture")
    }

    fn fixture_text(name: &str) -> String {
        String::from_utf8(fixture(name)).expect("utf-8 fixture")
    }

    fn fixture_trust() -> Trust {
        Trust::from_texts(&[&fixture_text("test-agent-key.pub")]).expect("fixture key")
    }

    #[test]
    fn the_compiled_in_agent_keys_decode() {
        Trust::compiled_in().expect("agent/release-signing-key*.pub decode");
        assert!(!TRUSTED_PUBLIC_KEYS.is_empty());
    }

    #[test]
    fn the_signed_fixture_verifies_and_returns_its_version() {
        let latest = verify(
            &fixture_trust(),
            &fixture("agent-latest.json"),
            &fixture_text("agent-latest.json.minisig"),
        )
        .expect("verifies");
        assert_eq!(latest.version, "2026.9.9");
    }

    #[test]
    fn a_feed_one_byte_off_is_refused_before_it_is_decoded() {
        let mut bytes = fixture("agent-latest.json");
        // Still valid JSON with a valid CalVer: only the signature can say no.
        let at = bytes
            .windows(8)
            .position(|w| w == b"2026.9.9")
            .expect("version");
        bytes[at + 7] = b'8';
        assert!(std::str::from_utf8(&bytes).unwrap().contains("2026.9.8"));
        let err = verify(
            &fixture_trust(),
            &bytes,
            &fixture_text("agent-latest.json.minisig"),
        )
        .expect_err("refused");
        assert_eq!(err, Error::SignatureRejected);
    }

    #[test]
    fn bytes_that_are_not_even_json_are_still_a_signature_failure_first() {
        // Garbage would be Malformed if it were decoded; it must not be reached.
        let err = verify(
            &fixture_trust(),
            b"not json at all",
            &fixture_text("agent-latest.json.minisig"),
        )
        .expect_err("refused");
        assert_eq!(err, Error::SignatureRejected);
    }

    #[test]
    fn a_signature_under_a_foreign_key_is_refused() {
        // The compiled-in set holds the real agent key; the fixture was signed
        // by a throwaway one, so verifying it there is a foreign signature.
        let err = verify(
            &Trust::compiled_in().unwrap(),
            &fixture("agent-latest.json"),
            &fixture_text("agent-latest.json.minisig"),
        )
        .expect_err("refused");
        assert_eq!(err, Error::SignatureRejected);
    }

    #[test]
    fn a_signature_made_for_another_file_is_refused() {
        // A real signature, under the trusted key, over *this* payload — but
        // whose trusted comment names a binary, not the feed.
        let name = "solador-agent-2026.9.9-aarch64-apple-darwin";
        let err = verify(
            &fixture_trust(),
            &fixture(name),
            &fixture_text(&format!("{name}.minisig")),
        )
        .expect_err("refused");
        assert_eq!(err, Error::SignatureRejected);
        // And the feed's own signature does not cover a binary either way.
        assert!(!fixture_trust().verifies(
            FEED_ASSET,
            &fixture_text(&format!("{name}.minisig")),
            &fixture(name)
        ));
    }

    #[test]
    fn an_empty_or_garbage_signature_is_refused() {
        for sig in ["", "not a signature"] {
            assert_eq!(
                verify(&fixture_trust(), &fixture("agent-latest.json"), sig),
                Err(Error::SignatureRejected),
                "{sig:?}"
            );
        }
    }

    #[test]
    fn a_trust_set_that_does_not_decode_is_its_own_error() {
        assert!(matches!(Trust::from_texts(&[]), Err(Error::TrustSet)));
        assert!(matches!(Trust::from_texts(&["junk"]), Err(Error::TrustSet)));
    }

    #[test]
    fn calver_is_strict() {
        for good in ["2026.9.9", "2026.10.1", "2027.1.120"] {
            assert!(is_calver(good), "{good}");
        }
        for bad in [
            "",
            "2026.9",
            "2026.9.9.1",
            "v2026.9.9",
            "26.9.9",
            "2026.09.9",
            "2026.9.09",
            "2026.9.x",
            "0.4.0",
            "2026.9.9+dev.1.gabc",
            "2026.9.-1",
            " 2026.9.9",
            "2026.9.99999999999",
        ] {
            assert!(!is_calver(bad), "{bad:?}");
        }
    }

    #[test]
    fn every_error_has_a_distinct_sentence_and_none_is_a_verdict() {
        let all = [
            Error::NotPublished,
            Error::Unreachable,
            Error::Status(503),
            Error::Unsigned,
            Error::SignatureRejected,
            Error::Malformed,
            Error::TrustSet,
        ];
        let messages: Vec<String> = all.iter().map(Error::user_message).collect();
        for (i, a) in messages.iter().enumerate() {
            assert!(!a.is_empty());
            assert!(!a.to_lowercase().contains("up to date"), "{a}");
            for b in &messages[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert_eq!(
            Error::NotPublished.user_message(),
            "no agent release published yet"
        );
        assert!(Error::Status(503).user_message().contains("503"));
    }

    #[test]
    fn urls_are_built_from_the_base_alone() {
        assert_eq!(
            asset_url(RELEASE_BASE, FEED_ASSET),
            "https://github.com/Sassy-Dog/solador/releases/download/agent-latest/agent-latest.json"
        );
    }

    #[test]
    fn only_loopback_may_be_plain_http() {
        assert!(is_loopback_base("http://127.0.0.1:8080"));
        assert!(is_loopback_base("http://localhost"));
        assert!(is_loopback_base("http://[::1]:1"));
        assert!(!is_loopback_base("http://127.0.0.1.evil.example"));
        assert!(!is_loopback_base("http://example.com"));
        assert!(!is_loopback_base(RELEASE_BASE));
    }
}
