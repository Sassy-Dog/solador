//! Claude Code's own account of the plan's limits, as its **status line** hands
//! them to the operator's status-line command.
//!
//! The plan read (`api.anthropic.com/api/oauth/usage`, [`crate::claude`]) is
//! refused with 429s often enough that the Claude card is mostly a dash. Claude
//! Code gets the same 5-hour and weekly utilisation with no call of its own and
//! passes it to the status-line command as `rate_limits`
//! (<https://code.claude.com/docs/en/statusline>):
//!
//! ```json
//! "rate_limits": {
//!   "five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
//!   "seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 }
//! }
//! ```
//!
//! # Whose file, who writes it
//!
//! [`FILE_NAME`], in the directory `store.json` lives in. **The operator's own
//! status-line command writes it**: `docs/APP-INTERNALS.md` gives the snippet,
//! which keeps `rate_limits` and the instant it ran and nothing else from the
//! status-line input (which also carries paths and session ids), through a
//! temporary file and a rename. Solador never edits the operator's status line or
//! `~/.claude/settings.json`, and never writes this file.
//!
//! It is **not a credential**, and nothing here reads one. It is still parsed as
//! if it were someone else's: through a typed parse that names the fields it
//! keeps and skips every other key unread ([`parse`]), so a file that grows
//! fields, or that a snippet wrote too much into, does not carry them anywhere.
//!
//! # What it is allowed to say
//!
//! - An absent file, field or window is no window, never 0%.
//! - A window whose `resets_at` has passed is not shown ([`StatusLine::windows`]):
//!   its figure was true of a window that has since rolled over.
//! - A file written more than [`MAX_AGE_SECS`] ago is not used at all, and
//!   neither is one dated more than [`FUTURE_SKEW_SECS`] ahead of `now`.
//! - Only the two windows, no model-scoped week and no spend.
//!
//! No wall clock: `now` is an argument.

use std::path::Path;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

/// The file's name, in the directory `store.json` lives in.
pub const FILE_NAME: &str = "claude-statusline.json";

/// The oldest a file may be and still be used: a day. Past it, the figures
/// describe a session the operator has not had for a day, and the card would be
/// showing a number nobody can place; the card is then the dash with its
/// header, as it was before this existed. The edge is inclusive: a file exactly
/// this old is used.
pub const MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// How far ahead of `now` a write time may be before the file is distrusted. A
/// status line run a moment after the caller took `now` is not in the future.
pub const FUTURE_SKEW_SECS: u64 = 60;

/// One window as the status line reported it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Window {
    /// 0 to 100 as stated; finite and non-negative.
    pub used_percent: f64,
    /// Unix seconds, or `None` when the status line did not state one.
    pub resets_at: Option<i64>,
}

/// Which window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    FiveHour,
    SevenDay,
}

/// What the file holds, and when it was written.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatusLine {
    /// When the snippet wrote the file, Unix seconds.
    pub written_at: u64,
    pub five_hour: Option<Window>,
    pub seven_day: Option<Window>,
}

impl StatusLine {
    /// How old the file is at `now`, in seconds. Never negative: a write a moment
    /// ahead of `now` (see [`FUTURE_SKEW_SECS`]) is zero seconds old.
    #[must_use]
    pub const fn age_secs(&self, now: u64) -> u64 {
        now.saturating_sub(self.written_at)
    }

    /// The windows still open at `now`: the 5-hour one, then the week, each only
    /// when present and not yet reset.
    #[must_use]
    pub fn windows(&self, now: u64) -> Vec<(Kind, Window)> {
        let open = |window: &Window| {
            window
                .resets_at
                .is_none_or(|at| i64::try_from(now).is_ok_and(|now| at > now))
        };
        [
            (Kind::FiveHour, self.five_hour),
            (Kind::SevenDay, self.seven_day),
        ]
        .into_iter()
        .filter_map(|(kind, window)| window.filter(open).map(|window| (kind, window)))
        .collect()
    }
}

/// A field whose value is the wrong type is as good as absent, and does not
/// fail the file it sits in.
fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let raw = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(raw.and_then(|value| serde_json::from_value(value).ok()))
}

#[derive(Deserialize)]
struct RawWindow {
    #[serde(default, deserialize_with = "lenient")]
    used_percentage: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    resets_at: Option<i64>,
}

fn lenient_window<'de, D>(deserializer: D) -> Result<Option<Window>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw: Option<RawWindow> = lenient(deserializer)?;
    Ok(raw.and_then(|raw| {
        let used_percent = raw
            .used_percentage
            .filter(|percent| percent.is_finite() && *percent >= 0.0)?;
        Some(Window {
            used_percent,
            resets_at: raw.resets_at,
        })
    }))
}

#[derive(Deserialize)]
struct RawLimits {
    #[serde(default, deserialize_with = "lenient_window")]
    five_hour: Option<Window>,
    #[serde(default, deserialize_with = "lenient_window")]
    seven_day: Option<Window>,
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default, deserialize_with = "lenient")]
    written_at: Option<u64>,
    #[serde(default)]
    rate_limits: Option<RawLimits>,
}

/// Parses the file's text. `None` for text that is not the file, a file with no
/// write time, one too old or dated ahead of `now` (see the module docs), and one
/// that states neither window.
///
/// The typed parse names `written_at`, `rate_limits.five_hour` and
/// `rate_limits.seven_day` with their two fields each, and skips every other key
/// unread.
#[must_use]
pub fn parse(text: &str, now: u64) -> Option<StatusLine> {
    let file: RawFile = serde_json::from_str(text).ok()?;
    let written_at = file.written_at?;
    let too_new = written_at > now.saturating_add(FUTURE_SKEW_SECS);
    let too_old = now.saturating_sub(written_at) > MAX_AGE_SECS;
    if too_new || too_old {
        return None;
    }
    let limits = file.rate_limits?;
    let status_line = StatusLine {
        written_at,
        five_hour: limits.five_hour,
        seven_day: limits.seven_day,
    };
    (status_line.five_hour.is_some() || status_line.seven_day.is_some()).then_some(status_line)
}

/// Reads [`FILE_NAME`] in `dir`. `None` when it is absent, unreadable or not
/// usable ([`parse`]).
#[must_use]
pub fn read(dir: &Path, now: u64) -> Option<StatusLine> {
    let text = std::fs::read_to_string(dir.join(FILE_NAME)).ok()?;
    parse(&text, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_792_843_200;

    fn file(written_at: u64, limits: &str) -> String {
        format!("{{\"written_at\":{written_at},\"rate_limits\":{limits}}}")
    }

    const BOTH: &str = r#"{"five_hour":{"used_percentage":23.5,"resets_at":1792850000},
        "seven_day":{"used_percentage":41.2,"resets_at":1793100000}}"#;

    fn kinds(line: &StatusLine, now: u64) -> Vec<Kind> {
        line.windows(now)
            .into_iter()
            .map(|(kind, _)| kind)
            .collect()
    }

    #[test]
    fn both_windows_are_read_with_their_figures_and_the_write_time() {
        let line = parse(&file(NOW - 120, BOTH), NOW).unwrap();
        assert_eq!(line.written_at, NOW - 120);
        assert_eq!(line.age_secs(NOW), 120);
        assert_eq!(
            line.five_hour,
            Some(Window {
                used_percent: 23.5,
                resets_at: Some(1_792_850_000)
            })
        );
        assert_eq!(line.seven_day.map(|w| w.used_percent), Some(41.2));
        assert_eq!(kinds(&line, NOW), [Kind::FiveHour, Kind::SevenDay]);
    }

    /// `StatusLine` has no field to put a session id or a path in, and the
    /// parse names none; this proves a file full of them still reads, and that
    /// a model-scoped week or a spend limit is not mistaken for a window.
    #[test]
    fn every_other_key_is_ignored_wherever_it_sits() {
        let text = r#"{
            "written_at": 1792843100,
            "session_id": "abc",
            "cwd": "/somewhere/private",
            "model": {"id": "x"},
            "rate_limits": {
                "five_hour": {"used_percentage": 10, "resets_at": 1792850000, "note": "x"},
                "seven_day": {"used_percentage": 20, "resets_at": 1793100000},
                "spend_limit": {"used": 5},
                "opus_week": {"used_percentage": 99, "resets_at": 1793100000}
            }
        }"#;
        let line = parse(text, NOW).unwrap();
        assert_eq!(line.five_hour.unwrap().used_percent, 10.0);
        assert_eq!(line.seven_day.unwrap().used_percent, 20.0);
        assert_eq!(line.windows(NOW).len(), 2, "no model-scoped week, no spend");
    }

    /// The exact line the documented status-line snippet wrote for a sample
    /// input carrying a session id, a working directory and a cost.
    #[test]
    fn what_the_documented_snippet_writes_is_read() {
        let written = r#"{"written_at":1792843150,"rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1792850000},"seven_day":{"used_percentage":41.2,"resets_at":1793100000}}}"#;
        let line = parse(written, NOW).unwrap();
        assert_eq!(line.written_at, 1_792_843_150);
        assert_eq!(kinds(&line, NOW), [Kind::FiveHour, Kind::SevenDay]);
    }

    #[test]
    fn an_absent_window_is_no_window_never_zero_percent() {
        let only_week = file(
            NOW - 5,
            r#"{"seven_day":{"used_percentage":41.2,"resets_at":1793100000}}"#,
        );
        let line = parse(&only_week, NOW).unwrap();
        assert_eq!(line.five_hour, None);
        assert_eq!(kinds(&line, NOW), [Kind::SevenDay]);

        for limits in [
            "{}",
            "null",
            r#"{"five_hour":null}"#,
            r#"{"five_hour":{}}"#,
            r#"{"five_hour":{"resets_at":1792850000}}"#,
            r#"{"five_hour":{"used_percentage":"lots","resets_at":1792850000}}"#,
            r#"{"five_hour":{"used_percentage":-4,"resets_at":1792850000}}"#,
            r#"{"five_hour":"23.5"}"#,
        ] {
            assert_eq!(parse(&file(NOW - 5, limits), NOW), None, "{limits}");
        }
        // The whole field appears only after a session's first API response.
        assert_eq!(parse(&format!("{{\"written_at\":{}}}", NOW - 5), NOW), None);
    }

    #[test]
    fn a_window_that_has_reset_is_not_shown() {
        let line = parse(&file(NOW - 5, BOTH), NOW).unwrap();
        let five_hour_reset = 1_792_850_000;
        assert_eq!(
            kinds(&line, five_hour_reset - 1),
            [Kind::FiveHour, Kind::SevenDay]
        );
        assert_eq!(
            kinds(&line, five_hour_reset),
            [Kind::SevenDay],
            "a reset at `now` is a reset"
        );
        assert_eq!(kinds(&line, 1_793_100_000), Vec::<Kind>::new());
    }

    #[test]
    fn a_window_that_states_no_reset_is_shown_as_stated() {
        let text = file(NOW - 5, r#"{"five_hour":{"used_percentage":12}}"#);
        let line = parse(&text, NOW).unwrap();
        assert_eq!(line.windows(NOW)[0].1.resets_at, None);
    }

    #[test]
    fn a_file_older_than_a_day_is_not_used_and_exactly_a_day_is() {
        assert!(parse(&file(NOW - MAX_AGE_SECS, BOTH), NOW).is_some());
        assert_eq!(parse(&file(NOW - MAX_AGE_SECS - 1, BOTH), NOW), None);
    }

    #[test]
    fn a_write_time_from_the_future_is_distrusted_beyond_a_minutes_skew() {
        assert!(parse(&file(NOW + FUTURE_SKEW_SECS, BOTH), NOW).is_some());
        assert_eq!(parse(&file(NOW + FUTURE_SKEW_SECS + 1, BOTH), NOW), None);
        let line = parse(&file(NOW + 30, BOTH), NOW).unwrap();
        assert_eq!(line.age_secs(NOW), 0);
    }

    #[test]
    fn a_file_with_no_write_time_or_that_is_not_json_is_not_used() {
        assert_eq!(parse(&format!("{{\"rate_limits\":{BOTH}}}"), NOW), None);
        assert_eq!(
            parse("{\"written_at\":\"now\",\"rate_limits\":{}}", NOW),
            None
        );
        for junk in ["", "nope", "[]", "42"] {
            assert_eq!(parse(junk, NOW), None, "{junk:?}");
        }
    }

    #[test]
    fn read_finds_the_file_in_the_directory_and_none_when_it_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), NOW), None);
        std::fs::write(dir.path().join(FILE_NAME), file(NOW - 60, BOTH)).unwrap();
        assert_eq!(read(dir.path(), NOW).unwrap().written_at, NOW - 60);
    }
}
