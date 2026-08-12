//! Discovery and streaming of Claude Code's local JSONL logs. Port of the
//! off-actor half of `ClaudeUsageService`
//! — the file walk and line streaming, without the polling loop or the
//! published state, which belong to the shell.
//!
//! Performance shapes the design: there can be ~1600 files and hundreds of MB
//! on disk. Only files modified inside [`FRESH_WINDOW_DAYS`] are opened, and
//! each is read line-by-line rather than slurped.
//!
//! The log root is a parameter everywhere. [`default_projects_dir`] is only the
//! shell's starting point, so the tests here never touch a real home directory
//! and run identically on macOS and Windows.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, TimeDelta, Utc};

use super::log::{self, UsageRecord};

/// How far back a file's mtime may be and still be worth opening: one day of
/// slack over the widest window the summary reports (7d), so a file written
/// just before the boundary is never missed.
pub const FRESH_WINDOW_DAYS: i64 = 8;

/// The file extension Claude Code writes its session logs with.
const LOG_EXTENSION: &str = "jsonl";

/// Root of Claude Code's per-project logs for the current user:
/// `~/.claude/projects`.
///
/// `None` when the home directory cannot be determined — an unknown root, not a
/// guess at one. Callers should surface that rather than substituting a path.
#[must_use]
pub fn default_projects_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".claude").join("projects"))
}

/// Every usage record in `projects_dir`, from files modified within
/// [`FRESH_WINDOW_DAYS`] of `now`.
///
/// Resilient by design: an unreadable directory, an unreadable file, or a
/// malformed line is skipped rather than failing the walk. A missing root
/// yields no records — the shell reports "no ~/.claude/projects" from its own
/// existence check, the way the original does.
#[must_use]
pub fn collect_records(projects_dir: &Path, now: DateTime<Utc>) -> Vec<UsageRecord> {
    let cutoff = now - TimeDelta::days(FRESH_WINDOW_DAYS);
    let mut records = Vec::new();

    for file in fresh_log_files(projects_dir, cutoff) {
        append_records(&file, &mut records);
    }
    records
}

/// The `.jsonl` regular files under `root` that were modified at or after
/// `cutoff`, walked recursively with hidden entries skipped.
fn fresh_log_files(root: &Path, cutoff: DateTime<Utc>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];

    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if is_hidden(&entry.file_name()) {
                continue;
            }
            let path = entry.path();
            // `file_type()` reads the directory entry itself, so a symlink is
            // reported as a symlink and never followed — a cycle cannot hang
            // the walk.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && has_log_extension(&path) && is_fresh(&path, cutoff) {
                files.push(path);
            }
        }
    }
    // `read_dir` order is filesystem-defined; sorting keeps the record order —
    // and so the first-wins dedup — reproducible across machines and runs.
    files.sort();
    files
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.to_str().is_some_and(|n| n.starts_with('.'))
}

fn has_log_extension(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == LOG_EXTENSION)
}

/// Whether the file was modified at or after `cutoff`. A file whose mtime
/// cannot be read is treated as stale: it is skipped, not assumed fresh.
fn is_fresh(path: &Path, cutoff: DateTime<Utc>) -> bool {
    modified_at(path).is_some_and(|modified| modified >= cutoff)
}

fn modified_at(path: &Path) -> Option<DateTime<Utc>> {
    let modified: SystemTime = fs::metadata(path).ok()?.modified().ok()?;
    let secs = modified.duration_since(UNIX_EPOCH).ok()?.as_secs();
    DateTime::from_timestamp(i64::try_from(secs).ok()?, 0)
}

/// Streams one file line-by-line, appending the usage records it carries.
/// Read errors and malformed lines are skipped; a partially readable file still
/// contributes everything before the failure.
fn append_records(path: &Path, records: &mut Vec<UsageRecord>) {
    let Ok(file) = File::open(path) else {
        return;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Some(record) = log::parse_line(&line) {
            records.push(record);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// The fixed reference "now" the Claude twins share: 2026-05-30T12:00:00Z.
    /// Every fixture file's mtime is set relative to it, so nothing here reads
    /// the wall clock and the suite cannot go flaky at a day boundary.
    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp(1_780_142_400, 0).expect("valid reference instant")
    }

    /// One assistant line carrying usage.
    fn line(request_id: &str, cwd: &str, input: u64) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"2026-05-29T11:00:00.000Z","requestId":"{request_id}","cwd":"{cwd}","message":{{"model":"claude-sonnet-4-5","usage":{{"input_tokens":{input}}}}}}}"#
        )
    }

    /// Writes `contents` to `root/relative` and stamps its mtime at [`now`],
    /// creating parent directories.
    fn write_file(root: &Path, relative: &str, contents: &str) -> PathBuf {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        let mut file = File::create(&path).expect("create file");
        file.write_all(contents.as_bytes()).expect("write file");
        drop(file);
        set_mtime(&path, now());
        path
    }

    /// Stamps a file's mtime at an exact instant, so freshness is a property of
    /// the fixture rather than of how long the test took to run.
    fn set_mtime(path: &Path, at: DateTime<Utc>) {
        let seconds = u64::try_from(at.timestamp()).expect("a post-epoch instant");
        // Opened for writing on purpose: Windows needs write access to change
        // file times, where a read-only handle is enough on Unix.
        fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(seconds))
            .expect("set mtime");
    }

    /// Backdates a file's mtime by `days` before [`now`].
    fn backdate(path: &Path, days: i64) {
        set_mtime(path, now() - TimeDelta::days(days));
    }

    fn collect_now(root: &Path) -> Vec<UsageRecord> {
        collect_records(root, now())
    }

    #[test]
    fn collects_records_from_nested_jsonl_files() {
        let root = TempDir::new().expect("temp dir");
        write_file(
            root.path(),
            "project-a/session-1.jsonl",
            &format!(
                "{}\n{}\n",
                line("r1", "/Repos/alpha", 10),
                line("r2", "/Repos/alpha", 20)
            ),
        );
        write_file(
            root.path(),
            "project-b/nested/session-2.jsonl",
            &format!("{}\n", line("r3", "/Repos/bravo", 5)),
        );

        let records = collect_now(root.path());
        assert_eq!(records.len(), 3);
        assert_eq!(
            records.iter().map(|r| r.input_tokens).sum::<u64>(),
            35,
            "every nested file contributes"
        );
        assert!(records.iter().any(|r| r.project == "bravo"));
    }

    /// A trailing line without a newline still counts — the original streams the
    /// leftover buffer for exactly this reason.
    #[test]
    fn reads_a_final_line_that_has_no_trailing_newline() {
        let root = TempDir::new().expect("temp dir");
        write_file(
            root.path(),
            "s.jsonl",
            &format!(
                "{}\n{}",
                line("r1", "/Repos/a", 1),
                line("r2", "/Repos/a", 2)
            ),
        );
        assert_eq!(collect_now(root.path()).len(), 2);
    }

    #[test]
    fn ignores_files_that_are_not_jsonl() {
        let root = TempDir::new().expect("temp dir");
        write_file(root.path(), "notes.txt", &line("r1", "/Repos/a", 10));
        write_file(root.path(), "session.json", &line("r2", "/Repos/a", 10));
        write_file(root.path(), "session.jsonl", &line("r3", "/Repos/a", 10));

        let records = collect_now(root.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "r3");
    }

    /// Files older than the freshness window are never opened. The window has a
    /// day of slack over the 7d summary window, so a 6-day-old file is still in
    /// and a 9-day-old one is out.
    #[test]
    fn skips_files_modified_outside_the_freshness_window() {
        let root = TempDir::new().expect("temp dir");
        let fresh = write_file(root.path(), "fresh.jsonl", &line("fresh", "/Repos/a", 1));
        let stale = write_file(root.path(), "stale.jsonl", &line("stale", "/Repos/a", 1));
        backdate(&fresh, 6);
        backdate(&stale, 9);

        let records = collect_now(root.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "fresh");
    }

    /// The boundary is the caller's `now`, not the wall clock: the same tree
    /// read from a later `now` goes silent.
    #[test]
    fn freshness_is_measured_against_the_supplied_now() {
        let root = TempDir::new().expect("temp dir");
        write_file(root.path(), "s.jsonl", &line("r1", "/Repos/a", 1));

        assert_eq!(collect_records(root.path(), now()).len(), 1);
        let much_later = now() + TimeDelta::days(FRESH_WINDOW_DAYS + 1);
        assert!(
            collect_records(root.path(), much_later).is_empty(),
            "a file older than the window relative to `now` is not opened"
        );
    }

    #[test]
    fn skips_hidden_files_and_directories() {
        let root = TempDir::new().expect("temp dir");
        write_file(root.path(), ".hidden.jsonl", &line("hidden", "/Repos/a", 1));
        write_file(
            root.path(),
            ".cache/inside.jsonl",
            &line("in_hidden_dir", "/Repos/a", 1),
        );
        write_file(
            root.path(),
            "visible.jsonl",
            &line("visible", "/Repos/a", 1),
        );

        let records = collect_now(root.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "visible");
    }

    /// Malformed and non-usage lines are skipped without taking the file's
    /// good lines with them.
    #[test]
    fn a_malformed_line_does_not_discard_the_rest_of_the_file() {
        let root = TempDir::new().expect("temp dir");
        let contents = format!(
            "{}\nnot json at all\n{{\"type\":\"user\"}}\n\n{}\n",
            line("good1", "/Repos/a", 1),
            line("good2", "/Repos/a", 2)
        );
        write_file(root.path(), "s.jsonl", &contents);

        let ids: Vec<String> = collect_now(root.path())
            .into_iter()
            .map(|r| r.request_id)
            .collect();
        assert_eq!(ids, ["good1", "good2"]);
    }

    /// A missing root is not a crash and not an error — it is simply no
    /// records. The shell reports the missing directory separately.
    #[test]
    fn a_missing_root_yields_no_records() {
        let root = TempDir::new().expect("temp dir");
        let absent = root.path().join("does-not-exist");
        assert!(collect_records(&absent, now()).is_empty());
    }

    #[test]
    fn an_empty_root_yields_no_records() {
        let root = TempDir::new().expect("temp dir");
        assert!(collect_now(root.path()).is_empty());
    }

    /// Record order must not depend on `read_dir` order, or the first-wins
    /// dedup would resolve differently on different machines.
    #[test]
    fn records_come_back_in_a_deterministic_file_order() {
        let root = TempDir::new().expect("temp dir");
        for name in ["c", "a", "b"] {
            write_file(
                root.path(),
                &format!("{name}.jsonl"),
                &line(name, "/Repos/a", 1),
            );
        }
        let ids: Vec<String> = collect_now(root.path())
            .into_iter()
            .map(|r| r.request_id)
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    /// The default root is derived, never hard-coded, and always ends at
    /// `.claude/projects` — the one thing the shell depends on.
    #[test]
    fn the_default_projects_dir_sits_under_the_home_directory() {
        let Some(dir) = default_projects_dir() else {
            // No home directory on this machine: an unknown root, which the
            // function correctly reports as `None`.
            return;
        };
        assert!(
            dir.ends_with(Path::new(".claude").join("projects")),
            "got {dir:?}"
        );
        assert!(
            dirs::home_dir().is_some_and(|home| dir.starts_with(home)),
            "got {dir:?}"
        );
    }
}
