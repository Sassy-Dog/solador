//! The LOCAL and WT columns of the Repos panel: how many branches and how many
//! worktrees each tracked repo has *on this machine*.
//!
//! Port of the discovery + counting half of
//! `DevCanopy/Services/GitMonitor/GitWorktreeService.swift`. The Swift service
//! also parses per-worktree ahead/behind/dirty state for a panel that no longer
//! exists (the Git Worktrees panel folded into Repos); only the two counts the
//! Repos panel actually renders are ported.
//!
//! Read-only by construction: every git invocation here is a query
//! (`worktree list`, `for-each-ref`), never a command that writes. Nothing in
//! this module can modify a repository.
//!
//! **Unknown is not zero.** Both counts are `Option<u32>` and a git invocation
//! that fails yields `None`, which the panel renders as "—". Swift's
//! `localBranchCount` returns `0` on a git error; that is a fabricated number
//! and this deliberately does not copy it. `Some(0)` still means what it says:
//! a repo genuinely holding no local branches.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How far below a scan root a repo can be and still be found. `~/Repos/foo`
/// is depth 1, `~/Repos/group/foo` depth 2 — `GitWorktreeService(maxDepth: 3)`.
pub const MAX_DEPTH: usize = 3;

/// The directory scanned for repos, relative to the user's home.
/// `GitWorktreeService(roots: ["~/Repos"])`.
pub const DEFAULT_ROOT: &str = "Repos";

/// What one on-disk repo contributes to its panel row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalRepoCounts {
    /// `refs/heads` count. `None` when git could not be asked.
    pub local_branches: Option<u32>,
    /// Attached worktrees, the main one included. `None` when git could not be
    /// asked.
    pub worktrees: Option<u32>,
}

/// Normalize a repo or directory name for matching: lowercase, letters and
/// digits only.
///
/// Port of `PortfolioRepos.normalize`, and the *only* join between a tracked
/// slug and a directory on disk — which is what lets the slug
/// `acme/fly-wheel` find the folder `flywheel`.
#[must_use]
pub fn normalize(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The roots to scan on this machine — `~/Repos`, or nothing at all when the
/// platform will not name a home directory.
#[must_use]
pub fn default_roots() -> Vec<PathBuf> {
    dirs::home_dir()
        .map(|home| vec![home.join(DEFAULT_ROOT)])
        .unwrap_or_default()
}

/// Every repo found under `roots`, keyed by [`normalize`]d directory name.
///
/// A duplicate name (the same repo checked out under two roots) keeps the
/// first, matching the Swift dictionary's `uniquingKeysWith: { first, _ in
/// first }`.
///
/// Blocking: this walks the filesystem and spawns `git` once or twice per repo.
/// Callers run it on a blocking thread.
#[must_use]
pub fn scan(roots: &[PathBuf], max_depth: usize) -> BTreeMap<String, LocalRepoCounts> {
    let mut found = BTreeMap::new();
    for path in discover(roots, max_depth) {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        found
            .entry(normalize(name))
            .or_insert_with(|| counts(&path));
    }
    found
}

/// Directories holding a `.git` entry, at most `max_depth` below each root.
///
/// Does not descend *into* a repo: a submodule or a nested checkout inside one
/// is that repo's business, and a `.claude/worktrees/…` checkout would
/// otherwise register as a second repo of the same name.
#[must_use]
pub fn discover(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        if root.is_dir() {
            walk(root, 0, max_depth, &mut found);
        }
    }
    found
}

fn walk(dir: &Path, depth: usize, max_depth: usize, found: &mut Vec<PathBuf>) {
    // `.git` is a directory in a normal clone and a *file* in a linked
    // worktree, so this tests for existence rather than for a directory.
    if dir.join(".git").exists() {
        found.push(dir.to_path_buf());
        return;
    }
    if depth >= max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Sorted, so the "first wins" rule above is a property of the tree rather
    // than of whatever order the filesystem happened to hand back.
    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    children.sort();
    for child in children {
        walk(&child, depth + 1, max_depth, found);
    }
}

/// Both counts for one repo. Each is asked for separately so a failure in one
/// does not blank the other.
#[must_use]
pub fn counts(repo: &Path) -> LocalRepoCounts {
    LocalRepoCounts {
        local_branches: branch_count(repo),
        worktrees: worktree_count(repo),
    }
}

/// `refs/heads` count. Branches are repo-level — shared across every worktree —
/// so this asks once, at the repo root.
#[must_use]
pub fn branch_count(repo: &Path) -> Option<u32> {
    let out = git(repo, &["for-each-ref", "--format=%(refname)", "refs/heads"])?;
    Some(count_lines(&out))
}

/// Attached worktrees, the main checkout included. `git worktree list
/// --porcelain` emits one `worktree <path>` line per entry.
#[must_use]
pub fn worktree_count(repo: &Path) -> Option<u32> {
    let out = git(repo, &["worktree", "list", "--porcelain"])?;
    Some(count_worktree_lines(&out))
}

/// Non-blank lines, as a count.
fn count_lines(out: &str) -> u32 {
    u32::try_from(out.lines().filter(|line| !line.trim().is_empty()).count()).unwrap_or(u32::MAX)
}

/// `worktree <path>` records in `--porcelain` output. Counted by prefix rather
/// than by blank-line-separated blocks so a trailing newline (or its absence)
/// cannot change the answer.
fn count_worktree_lines(out: &str) -> u32 {
    u32::try_from(
        out.lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

/// `git -C <repo> <args>`, stdout captured. `None` on any failure — the binary
/// is missing, the directory is not a repo, git exits non-zero, or the output
/// is not UTF-8.
///
/// `stderr` is discarded rather than piped into the panel: git's diagnostics
/// are for a terminal, and the panel's honest answer to "we could not count
/// this" is "—".
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new(GIT)
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Where git lives.
///
/// An absolute path on Unix for the same reason `containers::local` resolves
/// docker absolutely: a macOS GUI app inherits a `launchd` environment, and its
/// `PATH` is not the shell's. `/usr/bin/git` is the Command Line Tools shim and
/// is present on every Mac that can build this repo — Swift's
/// `GitWorktreeService` hard-codes exactly this path.
#[cfg(not(windows))]
const GIT: &str = "/usr/bin/git";

/// On Windows a GUI process inherits the user's real `PATH`, and git installs
/// under a version-stamped prefix with no single well-known location, so `PATH`
/// is the answer.
#[cfg(windows)]
const GIT: &str = "git";

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn temp() -> TempDir {
        TempDir::new().expect("temp dir")
    }

    /// A directory that *looks* like a repo to the scanner without being one —
    /// enough for the discovery tests, which are about the walk, not about git.
    fn fake_repo(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        fs::create_dir_all(path.join(".git")).expect("create .git");
        path
    }

    /// A real repository, so the counting tests exercise the actual git
    /// invocations rather than a mock of them. Environment overrides keep it
    /// independent of whatever the developer's global git config says.
    fn real_repo(parent: &Path, name: &str) -> PathBuf {
        let path = parent.join(name);
        fs::create_dir_all(&path).expect("create repo dir");
        run_git(&path, &["init", "--initial-branch=main"]);
        run_git(&path, &["commit", "--allow-empty", "-m", "root"]);
        path
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new(GIT)
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .expect("git runs in the test environment");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // MARK: - normalize

    /// The join that makes the slug `fly-wheel` find the folder
    /// `flywheel`. Without it the LOCAL/WT columns read "—" for every repo
    /// whose directory name is punctuated differently from its slug.
    #[test]
    fn normalize_strips_punctuation_and_case() {
        assert_eq!(normalize("fly-wheel"), "flywheel");
        assert_eq!(normalize("FlyWheel"), "flywheel");
        assert_eq!(normalize("pipe-fitting"), "pipefitting");
        assert_eq!(normalize("pipe_fitting "), "pipefitting");
        assert_eq!(normalize("cogwheel"), "cogwheel");
        assert_eq!(normalize("---"), "");
    }

    // MARK: - discovery

    #[test]
    fn a_directory_with_a_dot_git_entry_is_a_repo_root() {
        let dir = temp();
        let repo = fake_repo(dir.path(), "gadget");
        assert_eq!(discover(&[dir.path().to_path_buf()], MAX_DEPTH), vec![repo]);
    }

    /// A `.git` FILE (not directory) is what a linked worktree carries. It is
    /// still a repo root and must still be found.
    #[test]
    fn a_dot_git_file_also_marks_a_repo_root() {
        let dir = temp();
        let repo = dir.path().join("linked");
        fs::create_dir_all(&repo).expect("create");
        fs::write(repo.join(".git"), "gitdir: /elsewhere\n").expect("write .git file");
        assert_eq!(discover(&[dir.path().to_path_buf()], MAX_DEPTH), vec![repo]);
    }

    /// The whole point of stopping at a repo root: this repo keeps its agent
    /// worktrees under `.claude/worktrees/`, and descending into one would
    /// register a *second* repo with the same directory name.
    #[test]
    fn discovery_does_not_descend_into_a_repo() {
        let dir = temp();
        let repo = fake_repo(dir.path(), "widget");
        fake_repo(&repo.join("nested"), "widget");
        assert_eq!(
            discover(&[dir.path().to_path_buf()], MAX_DEPTH),
            vec![repo],
            "a checkout inside a repo is that repo's business"
        );
    }

    #[test]
    fn discovery_finds_repos_nested_under_a_group_directory() {
        let dir = temp();
        let repo = fake_repo(&dir.path().join("acme"), "platform");
        assert_eq!(discover(&[dir.path().to_path_buf()], MAX_DEPTH), vec![repo]);
    }

    #[test]
    fn discovery_stops_at_the_depth_limit() {
        let dir = temp();
        let deep = fake_repo(&dir.path().join("a").join("b").join("c"), "too-deep");
        assert!(deep.exists());
        assert!(
            discover(&[dir.path().to_path_buf()], MAX_DEPTH).is_empty(),
            "depth 4 is past maxDepth 3"
        );
        assert_eq!(discover(&[dir.path().to_path_buf()], 4), vec![deep]);
    }

    /// Hidden directories are skipped, which is what keeps the scan out of
    /// `~/Repos/.cache` and friends.
    #[test]
    fn discovery_skips_hidden_directories() {
        let dir = temp();
        fake_repo(&dir.path().join(".hidden"), "secret");
        assert!(discover(&[dir.path().to_path_buf()], MAX_DEPTH).is_empty());
    }

    #[test]
    fn a_missing_root_is_skipped_rather_than_failing_the_scan() {
        let dir = temp();
        let repo = fake_repo(dir.path(), "gadget");
        let roots = vec![dir.path().join("does-not-exist"), dir.path().to_path_buf()];
        assert_eq!(discover(&roots, MAX_DEPTH), vec![repo]);
    }

    // MARK: - scan (discovery + counting, keyed for the join)

    #[test]
    fn scan_keys_repos_by_their_normalized_directory_name() {
        let dir = temp();
        fake_repo(dir.path(), "pipe-fitting");
        let found = scan(&[dir.path().to_path_buf()], MAX_DEPTH);
        assert_eq!(found.keys().collect::<Vec<_>>(), vec!["pipefitting"]);
    }

    /// A directory that is not a real repo yields "—" on both columns, never a
    /// zero: we could not count, which is not the same as counting none.
    #[test]
    fn a_directory_git_cannot_read_counts_as_unknown_not_zero() {
        let dir = temp();
        fake_repo(dir.path(), "not-really-a-repo");
        let found = scan(&[dir.path().to_path_buf()], MAX_DEPTH);
        assert_eq!(
            found.get("notreallyarepo"),
            Some(&LocalRepoCounts {
                local_branches: None,
                worktrees: None,
            })
        );
    }

    // MARK: - counting, against a real repository

    #[test]
    fn counts_branches_and_worktrees_of_a_real_repository() {
        let dir = temp();
        let repo = real_repo(dir.path(), "gadget");

        assert_eq!(branch_count(&repo), Some(1), "just the initial branch");
        assert_eq!(worktree_count(&repo), Some(1), "just the main checkout");

        run_git(&repo, &["branch", "feat/one"]);
        run_git(&repo, &["branch", "feat/two"]);
        assert_eq!(branch_count(&repo), Some(3));

        let linked = dir.path().join("gadget-wt");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                linked.to_str().expect("utf-8 path"),
                "feat/one",
            ],
        );
        assert_eq!(worktree_count(&repo), Some(2), "main plus one linked");

        // …and the scan reports the same numbers through the join key.
        let found = scan(&[dir.path().to_path_buf()], MAX_DEPTH);
        assert_eq!(
            found.get("gadget"),
            Some(&LocalRepoCounts {
                local_branches: Some(3),
                worktrees: Some(2),
            })
        );
    }

    /// A brand-new repo with no commit has no `refs/heads` at all. That is a
    /// genuine zero and must survive as `Some(0)` — the "—" rule exists to
    /// protect unknowns, not to swallow real counts.
    #[test]
    fn a_repo_with_no_commits_reports_zero_branches_not_unknown() {
        let dir = temp();
        let repo = dir.path().join("fresh");
        fs::create_dir_all(&repo).expect("create");
        run_git(&repo, &["init", "--initial-branch=main"]);
        assert_eq!(branch_count(&repo), Some(0));
        assert_eq!(worktree_count(&repo), Some(1));
    }

    #[test]
    fn counting_a_directory_that_is_not_a_repo_is_unknown() {
        let dir = temp();
        assert_eq!(branch_count(dir.path()), None);
        assert_eq!(worktree_count(dir.path()), None);
    }

    // MARK: - output parsing

    #[test]
    fn line_counting_ignores_blank_and_trailing_lines() {
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines("refs/heads/main\n"), 1);
        assert_eq!(count_lines("refs/heads/main\nrefs/heads/x\n"), 2);
        assert_eq!(count_lines("refs/heads/main\n\n"), 1);
    }

    #[test]
    fn worktree_records_are_counted_by_their_leading_key() {
        let porcelain = "worktree /a\nHEAD abc\nbranch refs/heads/main\n\n\
                         worktree /b\nHEAD def\ndetached\n";
        assert_eq!(count_worktree_lines(porcelain), 2);
        // A path containing the word must not add a phantom record.
        assert_eq!(
            count_worktree_lines("worktree /a/worktree-x\nHEAD abc\n"),
            1
        );
        assert_eq!(count_worktree_lines(""), 0);
    }
}
