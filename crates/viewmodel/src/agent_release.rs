//! Whether a host's agent is behind the newest **verified** agent release, and
//! how each answer is said (#489).
//!
//! # Eight states, and no two read alike
//!
//! The same discipline as [`crate::update`], for the same reason: "behind", "up
//! to date" and every way of *not knowing* are different claims, and rounding
//! any unknown to "up to date" is the unmeasured-metric-as-zero error in
//! another costume. Each host's line is exactly one of [`State`]:
//!
//! * [`State::NotChecked`] — no check has settled yet (the first frame, and an
//!   hour at worst at startup with no network).
//! * [`State::HostUnknown`] — this host has told us no version: no `/v1/health`
//!   read yet, or an agent that cannot name itself (`—`).
//! * [`State::SourceBuild`] — a `+dev` source build. It is deliberately not a
//!   CalVer, so there is nothing to compare, and it is never "behind".
//! * [`State::Unrecognised`] — a version that is neither a CalVer nor `+dev`
//!   (an agent from before the release train). Nothing to compare either.
//! * [`State::CheckFailed`] — the release feed could not be read or did not
//!   verify; carries the classified sentence from `crates/agentrelease`.
//! * [`State::NonePublished`] — the feed does not exist yet (a 404 on
//!   `agent-latest`): nothing has been published, which is a fact, not a fault.
//! * [`State::UpToDate`] — the host is at or above the newest verified release.
//! * [`State::Behind`] — a newer verified release exists.
//!
//! **Only [`State::Behind`] is amber, and nothing here is ever red.** A host
//! that is behind is still healthy. And on a host the org runs at a *pinned*
//! version (moved by a tracked re-pin, never by following latest) behind is the
//! expected state between pin bumps, so the sentence says that a newer release
//! *exists* (`2026.11.2 available`) and never that the host *should have*
//! updated itself.
//!
//! # The order of the questions
//!
//! The host's own version is asked first. A host with no comparable version has
//! nothing to be behind *with*, whatever the feed says, so it reports that and
//! the feed's state does not mask it. Only a comparable host reaches the feed.
//!
//! # CalVer is ordered numerically
//!
//! `(year, month, n)`, never lexically and never through
//! [`crate::update::is_newer`]'s semver comparator: `2026.9.49 < 2026.10.1`,
//! which a string compare gets backwards.

use crate::color;

/// What the shell has learned about the newest release. Mirrors the outcomes of
/// `crates/agentrelease` without depending on it: that crate carries an HTTP
/// stack, and the view layer does not need one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Latest {
    /// No check has completed. The default: a state nothing has looked at yet
    /// must not be mistaken for any verdict.
    #[default]
    NotChecked,
    /// The feed is not published yet.
    NonePublished,
    /// The newest release, from a feed whose signature verified.
    Published(String),
    /// The check failed; the classified, user-facing sentence.
    Failed(String),
}

/// A strict `YYYY.M.N` CalVer. Derived `Ord` is field order, which is exactly
/// `(year, month, n)` numerically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalVer(pub u32, pub u32, pub u32);

impl CalVer {
    /// Three dotted integers, a four-digit year, no leading zeroes. `None` for
    /// anything else — `+dev` builds, `v`-prefixed tags, a semver like `0.4.0`.
    #[must_use]
    pub fn parse(v: &str) -> Option<Self> {
        let fields: Vec<&str> = v.split('.').collect();
        if fields.len() != 3 || fields[0].len() != 4 {
            return None;
        }
        let mut nums = [0u32; 3];
        for (slot, f) in nums.iter_mut().zip(&fields) {
            if f.is_empty()
                || !f.bytes().all(|b| b.is_ascii_digit())
                || (f.len() > 1 && f.starts_with('0'))
            {
                return None;
            }
            *slot = f.parse().ok()?;
        }
        Some(CalVer(nums[0], nums[1], nums[2]))
    }
}

/// Which of the eight states a host's line is in. Stable, kebab-case
/// [`State::as_str`] names are what the frontend carries as a data attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    NotChecked,
    HostUnknown,
    SourceBuild,
    Unrecognised,
    CheckFailed,
    NonePublished,
    UpToDate,
    Behind,
}

impl State {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            State::NotChecked => "not-checked",
            State::HostUnknown => "host-unknown",
            State::SourceBuild => "source-build",
            State::Unrecognised => "unrecognised",
            State::CheckFailed => "check-failed",
            State::NonePublished => "none-published",
            State::UpToDate => "up-to-date",
            State::Behind => "behind",
        }
    }
}

/// One host's rendered line: its state, the sentence, and the colour that
/// qualifies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub state: State,
    pub text: String,
    pub color: u32,
}

/// The line for a host reporting `host_version` (`None` is "no version heard",
/// never an empty string standing in for one) against what is known of the
/// newest release.
#[must_use]
pub fn line(host_version: Option<&str>, latest: &Latest) -> Line {
    let make = |state, text: String, color| Line { state, text, color };

    let Some(host) = host_version else {
        return make(
            State::HostUnknown,
            "Agent version — · not compared with releases".to_string(),
            color::MUTED,
        );
    };
    // `+dev` before the CalVer parse, as `agent/src/update.rs` orders it: a
    // source build is a fact about the host, not a malformed version.
    if host.contains("+dev") {
        return make(
            State::SourceBuild,
            format!("Agent v{host} · source build, not compared with releases"),
            color::INK,
        );
    }
    let Some(have) = CalVer::parse(host) else {
        return make(
            State::Unrecognised,
            format!("Agent v{host} · not a release version, not compared with releases"),
            color::INK,
        );
    };

    match latest {
        Latest::NotChecked => make(
            State::NotChecked,
            format!("Agent v{host} · checking for a newer release…"),
            color::MUTED,
        ),
        Latest::NonePublished => make(
            State::NonePublished,
            format!("Agent v{host} · no agent release published yet"),
            color::MUTED,
        ),
        Latest::Failed(reason) => make(
            State::CheckFailed,
            format!("Agent v{host} · release check failed — {reason}"),
            color::INK,
        ),
        Latest::Published(newest) => match CalVer::parse(newest) {
            // Unreachable from a verified feed, which `crates/agentrelease`
            // holds to a CalVer; but a view that panics, or guesses, on a
            // version it cannot order is worse than one that says so.
            None => make(
                State::CheckFailed,
                format!("Agent v{host} · release check failed — the newest release's version couldn't be read"),
                color::INK,
            ),
            Some(newest_calver) if have < newest_calver => make(
                State::Behind,
                // "available", not "update": a pinned host is meant to sit
                // here until its tracked re-pin.
                format!("Agent v{host} · {newest} available"),
                color::AMBER,
            ),
            Some(_) => make(
                State::UpToDate,
                format!("Agent v{host} · up to date"),
                color::GREEN_DIM,
            ),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn published(v: &str) -> Latest {
        Latest::Published(v.to_string())
    }

    #[test]
    fn calver_orders_numerically_never_lexically() {
        let at = |s| CalVer::parse(s).unwrap();
        assert!(at("2026.10.13") < at("2026.11.1"));
        assert!(at("2026.9.49") < at("2026.10.1"));
        // The lexical order gets both of these backwards.
        assert!("2026.9.49" > "2026.10.1");
        assert!(at("2026.2.9") < at("2026.2.10"));
        assert!(at("2026.12.99") < at("2027.1.1"));
        assert_eq!(at("2026.9.9"), at("2026.9.9"));
    }

    #[test]
    fn calver_parse_is_strict_and_refuses_what_is_not_one() {
        for bad in [
            "",
            "0.4.0",
            "2026.9",
            "2026.9.9.1",
            "v2026.9.9",
            "2026.09.9",
            "2026.9.9+dev.1.gabc",
            "2026.x.9",
            "—",
        ] {
            assert_eq!(CalVer::parse(bad), None, "{bad:?}");
        }
        assert_eq!(CalVer::parse("2026.10.1"), Some(CalVer(2026, 10, 1)));
    }

    #[test]
    fn behind_is_amber_and_names_the_newer_release_without_a_demand() {
        let l = line(Some("2026.10.13"), &published("2026.11.1"));
        assert_eq!(l.state, State::Behind);
        assert_eq!(l.color, color::AMBER);
        assert_eq!(l.text, "Agent v2026.10.13 · 2026.11.1 available");
        // Informational: a pinned host is *meant* to be here.
        let lower = l.text.to_lowercase();
        for demand in ["should", "must", "update now", "outdated", "stale"] {
            assert!(!lower.contains(demand), "{}", l.text);
        }
    }

    #[test]
    fn behind_compares_numerically_across_a_digit_boundary() {
        let l = line(Some("2026.9.49"), &published("2026.10.1"));
        assert_eq!(l.state, State::Behind);
        let l = line(Some("2026.10.1"), &published("2026.9.49"));
        assert_eq!(l.state, State::UpToDate);
    }

    #[test]
    fn up_to_date_means_at_or_above_the_newest_verified_release() {
        for host in ["2026.11.2", "2026.12.1"] {
            let l = line(Some(host), &published("2026.11.2"));
            assert_eq!(l.state, State::UpToDate, "{host}");
            assert_eq!(l.color, color::GREEN_DIM);
            assert_eq!(l.text, format!("Agent v{host} · up to date"));
        }
    }

    #[test]
    fn a_host_with_no_version_is_unknown_whatever_the_feed_says() {
        for latest in [
            Latest::NotChecked,
            Latest::NonePublished,
            published("2026.11.2"),
            Latest::Failed("couldn't reach the agent release feed".into()),
        ] {
            let l = line(None, &latest);
            assert_eq!(l.state, State::HostUnknown, "{latest:?}");
            assert!(l.text.contains('—'));
            assert_ne!(l.color, color::AMBER);
        }
    }

    #[test]
    fn a_source_build_never_compares_and_is_never_behind() {
        let l = line(Some("2026.9.9+dev.3.gabc1234"), &published("2027.1.1"));
        assert_eq!(l.state, State::SourceBuild);
        assert!(l.text.contains("source build"));
        assert_ne!(l.color, color::AMBER);
        // Not even with no feed at all.
        assert_eq!(
            line(Some("2026.9.9+dev.3.gabc1234"), &Latest::NotChecked).state,
            State::SourceBuild
        );
    }

    #[test]
    fn a_version_that_is_not_a_calver_is_unrecognised_and_never_compared() {
        let l = line(Some("0.4.0"), &published("2026.11.2"));
        assert_eq!(l.state, State::Unrecognised);
        assert_ne!(l.color, color::AMBER);
    }

    #[test]
    fn the_feed_states_each_have_their_own_sentence() {
        let not_checked = line(Some("2026.9.9"), &Latest::NotChecked);
        let none = line(Some("2026.9.9"), &Latest::NonePublished);
        let failed = line(
            Some("2026.9.9"),
            &Latest::Failed("couldn't reach the agent release feed".into()),
        );
        assert_eq!(not_checked.state, State::NotChecked);
        assert_eq!(none.state, State::NonePublished);
        assert_eq!(failed.state, State::CheckFailed);
        assert!(none.text.contains("no agent release published yet"));
        assert!(failed
            .text
            .contains("couldn't reach the agent release feed"));
        assert!(!failed.text.contains("up to date"));
        assert_ne!(not_checked.text, none.text);
        assert_ne!(none.text, failed.text);
    }

    #[test]
    fn an_unorderable_published_version_is_a_failed_check_not_a_verdict() {
        let l = line(Some("2026.9.9"), &published("not-a-version"));
        assert_eq!(l.state, State::CheckFailed);
        assert_ne!(l.color, color::AMBER);
    }

    #[test]
    fn only_behind_is_amber_and_nothing_is_ever_red() {
        let failed = Latest::Failed("x".into());
        let cases: Vec<Line> = vec![
            line(Some("2026.9.9"), &Latest::NotChecked),
            line(None, &Latest::NotChecked),
            line(Some("2026.9.9+dev.1.gabc"), &Latest::NotChecked),
            line(Some("0.4.0"), &Latest::NotChecked),
            line(Some("2026.9.9"), &failed),
            line(Some("2026.9.9"), &Latest::NonePublished),
            line(Some("2026.9.9"), &published("2026.9.9")),
            line(Some("2026.9.9"), &published("2026.9.10")),
        ];
        assert_eq!(cases.len(), 8);
        for l in &cases {
            assert_ne!(l.color, color::RED, "{l:?}");
            assert_eq!(l.color == color::AMBER, l.state == State::Behind, "{l:?}");
        }
        // Every state is represented, with a distinct name.
        let mut names: Vec<&str> = cases.iter().map(|l| l.state.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn the_default_latest_is_not_checked() {
        assert_eq!(Latest::default(), Latest::NotChecked);
    }
}
