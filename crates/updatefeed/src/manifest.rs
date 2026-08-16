//! The `latest.json` document itself.
//!
//! # One artifact, two platform keys, and why that is honest
//!
//! `tauri-plugin-updater` asks the manifest for `<os>-<arch>` —
//! `darwin-aarch64` on Apple silicon, `darwin-x86_64` on Intel — and refuses
//! with `TargetNotFound` if its own key is missing. Solador's macOS bundle is
//! **universal** (#335, asserted by `build.sh`'s `assert_universal_binary`), so
//! both keys point at the same tarball. That is not a shortcut standing in for
//! two builds we have not made: the file genuinely carries both slices, so both
//! answers are true.
//!
//! # The platform keys are settled here so Windows is additive
//!
//! Windows packaging is #334 and does not exist yet (`scripts/build.sh` refuses
//! on non-Darwin by design). Naming the key scheme now — the plugin's own
//! `<os>-<arch>`, static format, one entry per target — means #334 adds a
//! `windows-x86_64` entry to [`Feed::platforms`] and changes nothing else. A
//! feed shaped around "the macOS artifact" would have had to be rewritten.
//!
//! # Every field here can break every client at once
//!
//! The manifest is fetched by every installed app. A `pub_date` that is not
//! RFC 3339, or a `version` that is not semver, does not degrade one field —
//! the plugin's deserializer fails and the whole check errors out on every
//! machine. So this module validates rather than trusts, and
//! [`Feed::to_json`] cannot be reached without going through
//! [`Feed::macos_universal`], which is where the refusals live.

use std::collections::BTreeMap;

use serde_json::{json, Value};

/// Apple silicon, as `tauri_plugin_updater::target()` spells it.
pub const DARWIN_AARCH64: &str = "darwin-aarch64";
/// Intel macOS, same source.
pub const DARWIN_X86_64: &str = "darwin-x86_64";

/// One downloadable artifact and the signature that covers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Where the plugin will GET it. A published release asset — never a draft
    /// one, which is not attached to its tag and 404s for everyone.
    pub url: String,
    /// The contents of the artifact's `.sig` file, verbatim.
    pub signature: String,
}

/// Why a feed could not be built.
///
/// Each variant is a document the plugin would have refused to deserialize, or
/// a comparison key that would have sorted wrongly — caught here, at publish
/// time, instead of on every operator's machine at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedError {
    /// The version is not the `YYYY.M.P` CalVer `get-version-info.sh` emits.
    Version(String),
    /// `pub_date` was supplied and is not RFC 3339.
    PubDate(String),
    /// A URL that is not an absolute `https://` URL.
    Url(String),
    /// An artifact with no signature. Never publishable.
    MissingSignature(String),
}

impl FeedError {
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            FeedError::Version(v) => format!(
                "'{v}' is not the CalVer scripts/get-version-info.sh emits (YYYY.M.<commits-this-month>, \
                 no leading zeroes) — the plugin compares this field as semver, and a value it cannot \
                 parse fails the update check on every installed app"
            ),
            FeedError::PubDate(d) => format!(
                "'{d}' is not an RFC 3339 timestamp — the plugin parses pub_date strictly and a bad \
                 one fails the whole manifest, not just this field"
            ),
            FeedError::Url(u) => format!("'{u}' is not an absolute https:// URL"),
            FeedError::MissingSignature(t) => {
                format!("the {t} artifact carries no signature — refusing to publish an entry the app would reject")
            }
        }
    }
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_message())
    }
}

impl std::error::Error for FeedError {}

/// A validated `latest.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    /// The marketing CalVer, and the *only* comparison key
    /// (`docs/VERSIONING.md` §"Mapping onto the update feed"). The build number
    /// is deliberately not here: one manifest field means one number carries
    /// the comparison, and it is not that one.
    pub version: String,
    /// Release notes, or `None`. Absent is a key that is simply not written —
    /// an empty string would render as a release that shipped with nothing to
    /// say, which is a different claim.
    pub notes: Option<String>,
    /// RFC 3339, or `None` on the same reasoning.
    pub pub_date: Option<String>,
    /// Target → artifact. `BTreeMap` so the emitted JSON is byte-stable and two
    /// runs over the same release produce the same file.
    pub platforms: BTreeMap<String, Artifact>,
}

impl Feed {
    /// The macOS feed: one universal tarball, advertised under both Apple
    /// targets.
    ///
    /// Every argument is validated here rather than at [`to_json`](Self::to_json)
    /// because this is the only constructor — there is no way to hold a `Feed`
    /// that would not deserialize.
    pub fn macos_universal(
        version: &str,
        notes: Option<String>,
        pub_date: Option<String>,
        artifact: Artifact,
    ) -> Result<Self, FeedError> {
        if !is_calver(version) {
            return Err(FeedError::Version(version.to_string()));
        }
        if let Some(date) = pub_date.as_deref() {
            if !is_rfc3339(date) {
                return Err(FeedError::PubDate(date.to_string()));
            }
        }
        if !artifact.url.starts_with("https://") || artifact.url.len() <= "https://".len() {
            return Err(FeedError::Url(artifact.url));
        }
        if artifact.signature.trim().is_empty() {
            return Err(FeedError::MissingSignature("macOS".to_string()));
        }

        let mut platforms = BTreeMap::new();
        platforms.insert(DARWIN_AARCH64.to_string(), artifact.clone());
        platforms.insert(DARWIN_X86_64.to_string(), artifact);

        Ok(Feed {
            version: version.to_string(),
            notes,
            pub_date,
            platforms,
        })
    }

    /// The document, in the plugin's *static* format (a `platforms` map), which
    /// is the shape a file served from a release asset must take — the dynamic
    /// format has one url/signature at the top level and can only come from a
    /// server that knows who is asking.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let mut doc = json!({ "version": self.version });
        // Written only when present. `"notes": null` and `"pub_date": null`
        // would both deserialize, but a null date reads as "we know it was
        // released at no time"; an absent key reads as "not recorded", which is
        // the true one.
        if let Some(notes) = &self.notes {
            doc["notes"] = json!(notes);
        }
        if let Some(pub_date) = &self.pub_date {
            doc["pub_date"] = json!(pub_date);
        }
        doc["platforms"] = Value::Object(
            self.platforms
                .iter()
                .map(|(target, artifact)| {
                    (
                        target.clone(),
                        json!({ "signature": artifact.signature, "url": artifact.url }),
                    )
                })
                .collect(),
        );
        doc
    }
}

/// `YYYY.M.P`, exactly as `scripts/get-version-info.sh --version` emits it.
///
/// Three numeric fields, no leading zeroes. The zero rule is not pedantry:
/// semver forbids leading zeroes in numeric identifiers, so `2026.08.1` does
/// not parse as a version at all — which is precisely why
/// `get-version-info.sh` emits a non-padded month, and why that property is
/// worth re-asserting on the way into the feed.
fn is_calver(v: &str) -> bool {
    let fields: Vec<&str> = v.split('.').collect();
    if fields.len() != 3 {
        return false;
    }
    if fields[0].len() != 4 {
        return false;
    }
    fields.iter().all(|f| {
        !f.is_empty()
            && f.bytes().all(|b| b.is_ascii_digit())
            && (f.len() == 1 || !f.starts_with('0'))
    })
}

/// `YYYY-MM-DDTHH:MM:SSZ` and the offset/fraction forms `time`'s RFC 3339
/// parser accepts.
///
/// Shape-checked rather than parsed into a date: this crate has no time
/// dependency, and the failure being guarded against is a *malformed string*
/// reaching a strict parser on every operator's machine — `2026-13-45` would
/// pass this and be caught by the plugin, but `2026-08-15 12:00` (the shape a
/// hand-written `date` invocation produces) is caught here.
fn is_rfc3339(d: &str) -> bool {
    let bytes = d.as_bytes();
    // Shortest legal form: 1970-01-01T00:00:00Z
    if bytes.len() < 20 {
        return false;
    }
    let digits_at = |idx: &[usize]| idx.iter().all(|&i| bytes[i].is_ascii_digit());
    if !digits_at(&[0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18]) {
        return false;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return false;
    }
    if bytes[13] != b':' || bytes[16] != b':' {
        return false;
    }
    let mut tail = &d[19..];
    // An optional fractional second.
    if let Some(rest) = tail.strip_prefix('.') {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        tail = &rest[digits..];
    }
    // The offset is MANDATORY in RFC 3339 — `Z` or `±HH:MM`. A timestamp with
    // no zone is the shape a bare `date` invocation produces, and it is the one
    // the plugin would reject.
    if tail.eq_ignore_ascii_case("Z") {
        return true;
    }
    let offset = tail.as_bytes();
    offset.len() == 6
        && (offset[0] == b'+' || offset[0] == b'-')
        && offset[3] == b':'
        && offset[1..3].iter().all(u8::is_ascii_digit)
        && offset[4..6].iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact() -> Artifact {
        Artifact {
            url: "https://github.com/Sassy-Dog/solador/releases/download/v2026.8.114/Solador-2026.8.114.app.tar.gz".to_string(),
            signature: "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZQo=".to_string(),
        }
    }

    fn feed() -> Feed {
        Feed::macos_universal(
            "2026.8.114",
            Some("what changed".to_string()),
            Some("2026-08-15T12:00:00Z".to_string()),
            artifact(),
        )
        .expect("a valid feed")
    }

    /// The acceptance item, spelled as an assertion: the version in the feed is
    /// the one `scripts/get-version-info.sh --version` produced, carried through
    /// unchanged — not re-derived, not prefixed with `v`, not padded.
    #[test]
    fn the_version_is_carried_through_verbatim() {
        let doc = feed().to_json();
        assert_eq!(doc["version"], json!("2026.8.114"));
    }

    #[test]
    fn both_apple_targets_point_at_the_one_universal_tarball() {
        let doc = feed().to_json();
        let a = &doc["platforms"][DARWIN_AARCH64];
        let x = &doc["platforms"][DARWIN_X86_64];
        assert_eq!(a["url"], x["url"], "one universal binary, two true answers");
        assert_eq!(a["signature"], x["signature"]);
        assert_eq!(a["url"], json!(artifact().url));
        // And nothing else is advertised: a target key we cannot build for
        // would send those machines to a 404 rather than telling them there is
        // no update. Windows arrives as an added key (#334).
        let platforms = doc["platforms"]
            .as_object()
            .expect("platforms is an object");
        assert_eq!(platforms.len(), 2);
    }

    #[test]
    fn an_absent_note_or_date_is_an_absent_key_not_a_null() {
        let bare = Feed::macos_universal("2026.8.114", None, None, artifact()).expect("valid");
        let doc = bare.to_json();
        assert!(
            doc.get("notes").is_none(),
            "notes must not be emitted at all"
        );
        assert!(doc.get("pub_date").is_none());
    }

    #[test]
    fn a_version_the_plugin_could_not_compare_is_refused() {
        for bad in [
            "v2026.8.114", // the tag, not the version
            "2026.08.1",   // padded month: semver forbids the leading zero
            "2026.8",      // two fields
            "2026.8.1.2",  // four
            "26.8.1",      // not a four-digit year
            "2026.8.x",
            "",
        ] {
            assert!(
                matches!(
                    Feed::macos_universal(bad, None, None, artifact()),
                    Err(FeedError::Version(_))
                ),
                "{bad} should have been refused"
            );
        }
    }

    #[test]
    fn a_date_the_plugin_could_not_parse_is_refused_because_it_breaks_every_field() {
        for bad in [
            "2026-08-15 12:00:00",
            "2026-08-15",
            "yesterday",
            "15/08/2026",
        ] {
            assert!(
                matches!(
                    Feed::macos_universal("2026.8.114", None, Some(bad.to_string()), artifact()),
                    Err(FeedError::PubDate(_))
                ),
                "{bad} should have been refused"
            );
        }
        for good in [
            "2026-08-15T12:00:00Z",
            "2026-08-15T12:00:00.123Z",
            "2026-08-15T12:00:00+01:00",
            "2026-08-15T12:00:00.5-05:00",
        ] {
            assert!(
                Feed::macos_universal("2026.8.114", None, Some(good.to_string()), artifact())
                    .is_ok(),
                "{good} should have been accepted"
            );
        }
    }

    #[test]
    fn an_entry_with_no_signature_is_refused_rather_than_published_unsigned() {
        let unsigned = Artifact {
            signature: String::new(),
            ..artifact()
        };
        assert!(matches!(
            Feed::macos_universal("2026.8.114", None, None, unsigned),
            Err(FeedError::MissingSignature(_))
        ));
    }

    #[test]
    fn a_url_that_is_not_an_absolute_https_url_is_refused() {
        for bad in [
            "",
            "https://",
            "/releases/latest",
            "http://example.invalid/a",
        ] {
            let a = Artifact {
                url: bad.to_string(),
                ..artifact()
            };
            assert!(
                matches!(
                    Feed::macos_universal("2026.8.114", None, None, a),
                    Err(FeedError::Url(_))
                ),
                "{bad} should have been refused"
            );
        }
    }

    /// The manifest has to survive a round trip through the plugin's own
    /// `platforms` lookup, so the keys are asserted as literal strings rather
    /// than through the constants that produced them.
    #[test]
    fn the_platform_keys_are_the_strings_the_plugin_asks_for() {
        let doc = feed().to_json();
        assert!(doc["platforms"]["darwin-aarch64"].is_object());
        assert!(doc["platforms"]["darwin-x86_64"].is_object());
    }
}
