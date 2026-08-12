//! Token rollups from Claude Code's local JSONL logs. Rust port of
//! the original app.
//!
//! Layering, outermost first:
//! - [`walk`] — file discovery and line streaming under a log root.
//! - [`log`] — one line to one [`UsageRecord`], including the project name
//!   derived from the session's `cwd`.
//! - [`aggregate`] — records to the [`UsageSummary`] the panel renders.
//! - [`pricing`] — per-model rates, computed but never displayed.
//!
//! The whole path is a pure function of (log root, `now`, UTC offset): nothing
//! reads the wall clock and nothing caches, so a summary can be reproduced
//! exactly from a fixture tree.

pub mod aggregate;
pub mod log;
pub mod pricing;
pub mod walk;

use std::path::Path;

use chrono::{DateTime, FixedOffset, Utc};

pub use aggregate::{local_day_start, summarize, UsageBreakdown, UsageSummary, UsageTotals};
pub use log::{parse_line, project_name, UsageRecord, UNKNOWN_MODEL, UNKNOWN_PROJECT};
pub use pricing::ModelPricing;
pub use walk::{collect_records, default_projects_dir, FRESH_WINDOW_DAYS};

/// Read a log root and fold it into the panel's summary — the whole Claude
/// pipeline in one call.
///
/// `offset` is the local UTC offset the "today" window is measured in; see
/// [`aggregate::local_day_start`] for why it is a parameter.
#[must_use]
pub fn summarize_logs(
    projects_dir: &Path,
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> UsageSummary {
    summarize(&collect_records(projects_dir, now), now, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, UNIX_EPOCH};
    use tempfile::TempDir;

    /// Writes a log file and stamps its mtime at `at`, so the walk's freshness
    /// check is decided by the fixture rather than by the wall clock.
    fn write_log(path: &Path, contents: &str, at: DateTime<Utc>) {
        fs::write(path, contents).expect("write log");
        let seconds = u64::try_from(at.timestamp()).expect("a post-epoch instant");
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
            .expect("set mtime");
    }

    /// End-to-end over a fixture tree: two session files, one call recorded in
    /// both (the duplicate the dedup exists for), all folded into one summary.
    #[test]
    fn summarizes_a_log_tree_end_to_end() {
        let root = TempDir::new().expect("temp dir");
        // The same fixed reference instant the module's other twins use, so the
        // walk's freshness check and the records' timestamps agree without
        // either reading the wall clock.
        let now = DateTime::from_timestamp(1_780_142_400, 0).expect("valid reference instant");
        let stamp = now.to_rfc3339();

        let call = |id: &str, cwd: &str, input: u64| {
            format!(
                r#"{{"type":"assistant","timestamp":"{stamp}","requestId":"{id}","cwd":"{cwd}","message":{{"model":"claude-opus-4-7","usage":{{"input_tokens":{input}}}}}}}"#
            )
        };

        let sessions = root.path().join("widget");
        fs::create_dir_all(&sessions).expect("mkdir");
        write_log(
            &sessions.join("session-1.jsonl"),
            &format!(
                "{}\n{}\n",
                call("r1", "/Repos/widget", 100),
                call("r2", "/Repos/widget", 50)
            ),
            now,
        );
        // The same `r2` again, from a worktree of the same repo: deduped, and
        // its cwd attributes to the repo rather than to the worktree.
        write_log(
            &sessions.join("session-2.jsonl"),
            &format!(
                "{}\n{}\n",
                call("r2", "/Repos/widget/.claude/worktrees/agent-x", 50),
                call("r3", "/Repos/widget/.claude/worktrees/agent-x", 25)
            ),
            now,
        );

        let summary = summarize_logs(
            root.path(),
            now,
            FixedOffset::east_opt(0).expect("valid offset"),
        );

        assert_eq!(summary.last_7d.input_tokens, 175, "100 + 50 + 25, r2 once");
        assert_eq!(summary.today.input_tokens, 175);
        assert_eq!(
            summary.projects_last_7d.len(),
            1,
            "the worktree attributes to its repo"
        );
        assert_eq!(summary.projects_last_7d[0].name, "widget");
        assert_eq!(summary.models_last_7d[0].name, "claude-opus-4-7");
    }
}
