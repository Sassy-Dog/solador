//! Pure parsing of Claude Code's local JSONL log lines. Port of
//! `ClaudeUsageLog` and the `UsageRecord`
//! half of `UsageModels`.
//!
//! Every helper here is side-effect free; file discovery lives in
//! [`crate::claude::walk`].

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// One billable API call distilled from a log line. Dedup is keyed on
/// [`UsageRecord::request_id`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// When the call happened.
    ///
    /// `None` when the line carried no parseable timestamp. Deliberately an
    /// `Option` rather than an epoch sentinel: an undatable record cannot be
    /// attributed to *any* window, and a sentinel would silently file it under
    /// whichever window happens to reach back far enough.
    pub timestamp: Option<DateTime<Utc>>,
    pub request_id: String,
    pub model: String,
    pub project: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl UsageRecord {
    /// All token kinds combined (input + output + cache write + cache read).
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.cache_read_tokens)
    }
}

/// The marker prefixes that make a path a worktree of some repo rather than a
/// project of its own. Everything from the marker onwards is dropped so all the
/// agents working one repo attribute to a single project.
const WORKTREE_MARKERS: [&str; 2] = ["/.claude/worktrees/", "/.warp-worktrees/"];

/// The name used when a line carries no usable `cwd`.
pub const UNKNOWN_PROJECT: &str = "unknown";

/// The model name used when a line carries no `message.model`.
pub const UNKNOWN_MODEL: &str = "unknown";

/// Derives a short project name from a session's `cwd`: strip any worktree
/// suffix, then take the last path component.
///
/// The `cwd` is a string *recorded in the log*, not a path on this machine, so
/// it is always parsed with POSIX separators regardless of the host OS — the
/// same reason this never touches the filesystem to resolve it.
#[must_use]
pub fn project_name(cwd: &str) -> String {
    if cwd.is_empty() {
        return UNKNOWN_PROJECT.to_string();
    }

    let mut path = cwd;
    for marker in WORKTREE_MARKERS {
        if let Some(index) = path.find(marker) {
            path = &path[..index];
        }
    }

    // `filter(non-empty)` handles both a trailing slash and a run of them, the
    // job the original's `standardizingPath` + empty-omitting `split` does there.
    path.split('/')
        .rfind(|component| !component.is_empty())
        .map_or_else(|| UNKNOWN_PROJECT.to_string(), str::to_string)
}

/// Parses a single JSONL line, or `None` when the line is not an assistant
/// message carrying usage, or is malformed. Never panics.
#[must_use]
pub fn parse_line(line: &str) -> Option<UsageRecord> {
    let raw: RawLine = serde_json::from_str(line).ok()?;
    if raw.kind.as_deref() != Some("assistant") {
        return None;
    }
    let message = raw.message?;
    let usage = message.usage?;

    Some(UsageRecord {
        timestamp: raw.timestamp.as_deref().and_then(parse_timestamp),
        request_id: raw.request_id.unwrap_or_default(),
        model: message.model.unwrap_or_else(|| UNKNOWN_MODEL.to_string()),
        project: project_name(raw.cwd.as_deref().unwrap_or_default()),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_creation_tokens: usage.cache_creation_input_tokens,
        cache_read_tokens: usage.cache_read_input_tokens,
    })
}

/// ISO-8601 with or without fractional seconds — `2026-05-29T13:18:22.932Z` and
/// `2026-05-29T13:18:22Z` both appear in the wild. `parse_from_rfc3339` accepts
/// either, so the two the original formatters collapse to one call here.
fn parse_timestamp(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// The subset of a log line this crate reads. Every field is optional because
/// the log is an external format we don't control; a line missing anything we
/// don't strictly need must still parse.
#[derive(Debug, Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    cwd: Option<String>,
    message: Option<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    model: Option<String>,
    usage: Option<RawUsage>,
}

/// A missing token field is a real zero — the log omits a counter that never
/// fired. A field present but not a number fails the whole line instead, so
/// contract skew shows up as a dropped record rather than as a silent zero.
#[derive(Debug, Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // MARK: - Project name derivation from cwd
    // Twins of `ClaudeUsageLogParsingTests`.

    #[test]
    fn project_name_from_a_plain_repo_path() {
        assert_eq!(
            project_name("/Users/dev/Repos/Acme/gadget/gadget-app"),
            "gadget-app"
        );
    }

    #[test]
    fn project_name_strips_the_claude_worktree_suffix() {
        assert_eq!(
            project_name(
                "/Users/dev/Repos/Acme/gadget/gadget-app/.claude/worktrees/agent-a186a09e"
            ),
            "gadget-app"
        );
    }

    #[test]
    fn project_name_strips_the_warp_worktree_suffix() {
        assert_eq!(
            project_name("/Users/dev/Repos/Acme/gadget/gadget-app/.warp-worktrees/foo-bar"),
            "gadget-app"
        );
    }

    /// Both agents of one repo must land in the same bucket — that is the whole
    /// point of stripping the suffix.
    #[test]
    fn every_worktree_of_a_repo_attributes_to_one_project() {
        let root = project_name("/Repos/widget");
        assert_eq!(
            project_name("/Repos/widget/.claude/worktrees/agent-aaa"),
            root
        );
        assert_eq!(project_name("/Repos/widget/.warp-worktrees/bbb"), root);
    }

    #[test]
    fn project_name_handles_a_trailing_slash() {
        assert_eq!(
            project_name("/Users/dev/Repos/pipe-fitting/"),
            "pipe-fitting"
        );
    }

    #[test]
    fn an_empty_or_rootless_cwd_falls_back_to_unknown() {
        assert_eq!(project_name(""), UNKNOWN_PROJECT);
        assert_eq!(project_name("/"), UNKNOWN_PROJECT);
        assert_eq!(project_name("///"), UNKNOWN_PROJECT);
    }

    // MARK: - Line parsing

    #[test]
    fn parses_an_assistant_usage_record() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-29T13:18:22.932Z","requestId":"req_abc","cwd":"/Users/dev/Repos/pipe-fitting","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":6,"output_tokens":475,"cache_creation_input_tokens":0,"cache_read_input_tokens":43049}}}"#;
        let record = parse_line(line).expect("an assistant line with usage parses");

        assert_eq!(record.request_id, "req_abc");
        assert_eq!(record.model, "claude-sonnet-4-5");
        assert_eq!(record.project, "pipe-fitting");
        assert_eq!(record.input_tokens, 6);
        assert_eq!(record.output_tokens, 475);
        assert_eq!(record.cache_creation_tokens, 0);
        assert_eq!(record.cache_read_tokens, 43_049);
        assert_eq!(
            record.timestamp,
            Some(
                DateTime::parse_from_rfc3339("2026-05-29T13:18:22.932Z")
                    .expect("fixture timestamp")
                    .with_timezone(&Utc)
            )
        );
    }

    /// The fractional-seconds half of the original's two formatters.
    #[test]
    fn parses_a_timestamp_without_fractional_seconds() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-29T13:18:22Z","requestId":"r","cwd":"/a","message":{"model":"m","usage":{"input_tokens":1}}}"#;
        let record = parse_line(line).expect("parses");
        assert_eq!(
            record.timestamp.map(|t| t.to_rfc3339()),
            Some("2026-05-29T13:18:22+00:00".to_string())
        );
    }

    /// An unparseable timestamp leaves the record undatable rather than
    /// pinning it to the epoch.
    #[test]
    fn an_unparseable_timestamp_is_unknown_not_the_epoch() {
        let line = r#"{"type":"assistant","timestamp":"yesterday","requestId":"r","cwd":"/a","message":{"model":"m","usage":{"input_tokens":1}}}"#;
        let record = parse_line(line).expect("parses");
        assert_eq!(record.timestamp, None);

        let missing = r#"{"type":"assistant","requestId":"r","cwd":"/a","message":{"model":"m","usage":{"input_tokens":1}}}"#;
        assert_eq!(parse_line(missing).expect("parses").timestamp, None);
    }

    #[test]
    fn skips_non_assistant_lines() {
        let line =
            r#"{"type":"user","timestamp":"2026-05-29T13:18:22.932Z","message":{"role":"user"}}"#;
        assert_eq!(parse_line(line), None);
    }

    #[test]
    fn skips_an_assistant_line_without_usage() {
        let line = r#"{"type":"assistant","timestamp":"2026-05-29T13:18:22.932Z","requestId":"x","cwd":"/a","message":{"model":"m"}}"#;
        assert_eq!(parse_line(line), None);
    }

    #[test]
    fn skips_malformed_lines() {
        assert_eq!(parse_line("not json at all"), None);
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("   "), None);
        assert_eq!(parse_line("[]"), None);
    }

    /// A missing counter is a real zero: the log omits a counter that never
    /// fired, and the line is still a genuine billable call.
    #[test]
    fn missing_token_counters_read_as_zero() {
        let line = r#"{"type":"assistant","requestId":"r","cwd":"/a","message":{"model":"m","usage":{"input_tokens":7}}}"#;
        let record = parse_line(line).expect("parses");
        assert_eq!(record.input_tokens, 7);
        assert_eq!(record.output_tokens, 0);
        assert_eq!(record.cache_creation_tokens, 0);
        assert_eq!(record.cache_read_tokens, 0);
        assert_eq!(record.total_tokens(), 7);
    }

    /// A counter present but not a number is contract skew, so the record is
    /// dropped rather than counted as zero tokens.
    #[test]
    fn a_non_numeric_token_counter_drops_the_record() {
        let line = r#"{"type":"assistant","requestId":"r","cwd":"/a","message":{"model":"m","usage":{"input_tokens":"lots"}}}"#;
        assert_eq!(parse_line(line), None);
    }

    #[test]
    fn a_line_without_a_model_or_cwd_still_parses_with_fallbacks() {
        let line = r#"{"type":"assistant","requestId":"r","message":{"usage":{"input_tokens":1}}}"#;
        let record = parse_line(line).expect("parses");
        assert_eq!(record.model, UNKNOWN_MODEL);
        assert_eq!(record.project, UNKNOWN_PROJECT);
    }
}
