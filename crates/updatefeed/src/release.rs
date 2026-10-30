//! Verify both release artifacts, then build the feed.
//!
//! `solador-update-feed` is argument parsing and file I/O around this; the
//! decision that matters lives here so it can be tested without a filesystem:
//! **a feed is built only from artifacts whose signatures verify under the
//! shipped key, and a release missing either one has no feed.**
//!
//! Both artifacts are checked against the *bytes that will be served*. The
//! Windows one is the Authenticode-signed installer, and its minisign `.sig`
//! was made after Authenticode appended to the PE, so a signature made over the
//! unsigned installer fails here exactly as it would in the app.

use crate::manifest::{Artifact, Feed};
use crate::signature;

/// One platform's artifact as the publish workflow harvested it: the bytes, the
/// `.sig` file's text and the URL the plugin will GET.
pub struct Payload<'a> {
    pub bytes: &'a [u8],
    pub signature: &'a str,
    pub url: &'a str,
}

/// Which platform's artifact a refusal is about, so a failed run names the
/// leg of the release that is broken.
fn refusal(platform: &str, why: impl std::fmt::Display) -> String {
    format!("the {platform} artifact: {why}")
}

/// Verify `macos` and `windows` under `pubkey`, then build the [`Feed`].
///
/// Nothing is built unless **both** verify. There is deliberately no path that
/// produces a one-platform feed.
pub fn build_feed(
    pubkey: &str,
    version: &str,
    notes: Option<String>,
    pub_date: Option<String>,
    macos: &Payload<'_>,
    windows: &Payload<'_>,
) -> Result<Feed, String> {
    for (platform, payload) in [("macOS", macos), ("Windows", windows)] {
        signature::verify(pubkey, payload.signature, payload.bytes)
            .map_err(|e| refusal(platform, e.user_message()))?;
    }
    Feed::new(
        version,
        notes,
        pub_date,
        Artifact {
            url: macos.url.to_string(),
            signature: macos.signature.trim().to_string(),
        },
        Artifact {
            url: windows.url.to_string(),
            signature: windows.signature.trim().to_string(),
        },
    )
    .map_err(|e| e.user_message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{DARWIN_AARCH64, WINDOWS_X86_64};

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/updater")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    fn text(name: &str) -> String {
        String::from_utf8(fixture(name)).expect("text fixture")
    }

    const MAC_URL: &str = "https://example.invalid/v2026.8.114/Solador-2026.8.114.app.tar.gz";
    const WIN_URL: &str = "https://example.invalid/v2026.8.114/Solador_2026.8.114_x64-setup.exe";

    /// The fixture stands in for both artifacts: what is under test is that each
    /// platform's bytes are checked against that platform's signature, not that
    /// a second keypair exists.
    fn build(mac_bytes: &[u8], win_bytes: &[u8], win_sig: &str) -> Result<Feed, String> {
        let sig = text("payload.tar.gz.sig");
        build_feed(
            &text("test-key.pub"),
            "2026.8.114",
            None,
            None,
            &Payload {
                bytes: mac_bytes,
                signature: &sig,
                url: MAC_URL,
            },
            &Payload {
                bytes: win_bytes,
                signature: win_sig,
                url: WIN_URL,
            },
        )
    }

    #[test]
    fn a_release_with_both_artifacts_verified_emits_all_three_targets() {
        let payload = fixture("payload.tar.gz");
        let feed = build(&payload, &payload, &text("payload.tar.gz.sig")).expect("both verify");
        let doc = feed.to_json();
        assert_eq!(doc["platforms"][DARWIN_AARCH64]["url"], MAC_URL);
        assert_eq!(doc["platforms"]["darwin-x86_64"]["url"], MAC_URL);
        assert_eq!(doc["platforms"][WINDOWS_X86_64]["url"], WIN_URL);
        assert_eq!(doc["platforms"].as_object().expect("object").len(), 3);
    }

    /// The Windows analogue of the macOS acceptance case, and the realistic
    /// failure: a `.sig` minted before Authenticode covers bytes that never
    /// ship. One flipped byte stands in for the appended certificate.
    #[test]
    fn a_windows_signature_that_does_not_cover_the_installer_is_refused() {
        let payload = fixture("payload.tar.gz");
        let mut shipped = payload.clone();
        shipped[0] ^= 0x01;
        let err = build(&payload, &shipped, &text("payload.tar.gz.sig"))
            .expect_err("a signature over other bytes must not publish");
        assert!(err.contains("Windows"), "should name the platform: {err}");
        assert!(err.contains("does not cover"), "{err}");
    }

    #[test]
    fn a_macos_signature_that_does_not_cover_the_tarball_is_refused() {
        let payload = fixture("payload.tar.gz");
        let mut shipped = payload.clone();
        shipped[0] ^= 0x01;
        let err = build(&shipped, &payload, &text("payload.tar.gz.sig"))
            .expect_err("a signature over other bytes must not publish");
        assert!(err.contains("macOS"), "should name the platform: {err}");
    }

    /// What a missing Windows `.sig` file reads as once harvested. The feed is
    /// refused rather than shipped macOS-only (the #368 symptom).
    #[test]
    fn a_missing_windows_signature_refuses_the_feed() {
        let payload = fixture("payload.tar.gz");
        for empty in ["", "  \n"] {
            let err = build(&payload, &payload, empty)
                .expect_err("a release with no Windows signature has no feed");
            assert!(err.contains("Windows"), "{err}");
        }
    }
}
