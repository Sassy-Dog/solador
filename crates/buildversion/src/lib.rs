//! The git-derived marketing version, published to a crate at build time.
//!
//! Called from a `build.rs`. Two crates use it and they ship on different
//! platforms — `app/src-tauri` (the cockpit) and `agent` (the metrics agent,
//! cross-compiled to four targets by `scripts/build-agent.sh`) — which is why
//! it lives here rather than being written twice. The plumbing is fiddly
//! enough (a worktree's `.git` *file*, the ref file a commit actually rewrites,
//! the shallow-clone refusal) that two copies would diverge at the first fix.
//!
//! **This does not compute a version.** `scripts/get-version-info.sh` is the
//! repo's §3 interface-contract owner and the CalVer algorithm exists exactly
//! once, there (`docs/VERSIONING.md`). Re-deriving the month-and-commit-count
//! arithmetic here would be a second implementation, and the day the two
//! disagree the artifact and the version it reports disagree.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable this publishes. Read back with
/// `option_env!("SOLADOR_MARKETING_VERSION")`.
pub const ENV_VAR: &str = "SOLADOR_MARKETING_VERSION";

/// Publish the git-derived CalVer to the calling crate as
/// [`ENV_VAR`], or publish **nothing at all** if it cannot be derived honestly.
///
/// Consumers read it through `option_env!`, so "not emitted" is a representable
/// state — the cockpit's About row renders `—`, and the agent's `--version`
/// refuses rather than answering. That asymmetry is the whole point: the value
/// this replaced was `env!("CARGO_PKG_VERSION")`, a real-looking version that
/// has never been a release.
pub fn emit_marketing_version() {
    // Re-run when the pin changes, and when HEAD moves. Watching `HEAD` alone
    // is not enough: committing rewrites the *ref file* while `HEAD` keeps
    // saying `ref: refs/heads/<branch>`, so a HEAD-only watch catches branch
    // switches and misses every commit — which is exactly what the CalVer
    // patch counts. Both paths come from `--git-path`, which resolves through
    // a worktree's `.git` *file*; this repo runs agents in worktrees routinely.
    println!("cargo:rerun-if-env-changed=MARKETING_VERSION");
    for p in [git_path("HEAD"), current_ref_path()].into_iter().flatten() {
        println!("cargo:rerun-if-changed={p}");
    }

    let root = repo_root().map(PathBuf::from);
    match resolve(root.as_deref(), pinned_version()) {
        Resolution::Pinned(v) | Resolution::Derived(v) => {
            println!("cargo:rustc-env={ENV_VAR}={v}");
        }
        Resolution::Shallow => println!(
            "cargo:warning=marketing version unavailable: shallow clone (fetch-depth: 0 is required to count commits)"
        ),
        Resolution::Unavailable => println!(
            "cargo:warning=marketing version unavailable: scripts/get-version-info.sh did not produce one"
        ),
    }
}

/// What [`emit_marketing_version`] decided, before it prints anything.
///
/// Split from the printing so the **ordering** is a value a test can hold:
/// a pin wins before the checkout is even looked at, a shallow checkout
/// refuses before the script is asked, and only then is the script's answer
/// consulted. Each arm is a different sentence to the build log, and two of
/// them publish nothing — which is the design, not a failure to handle.
#[derive(Debug, PartialEq, Eq)]
enum Resolution {
    /// `MARKETING_VERSION` was set: emitted verbatim, whatever the checkout is.
    Pinned(String),
    /// The checkout is shallow: nothing is published, and the log says why.
    Shallow,
    /// `scripts/get-version-info.sh --version` answered.
    Derived(String),
    /// No checkout, no script, or the script failed: nothing is published.
    Unavailable,
}

/// The decision, in order. `root` is the repository root when there is one;
/// `pin` is the already-normalised `MARKETING_VERSION`.
///
/// **The pin is consulted first, before the checkout is looked at.** A pin is
/// the minted tag's own number, and this helper is a consumer of it rather
/// than a second opinion (see [`pinned_version`]): no observation of the
/// checkout — shallow, detached, scriptless — may override it, because the
/// re-derive would diverge from the tag the day the ladder first bumps or the
/// month rolls. The shallow arm exists for the *unpinned* builds — CI's test
/// jobs, which check out at depth 1, and a local clone taken the same way;
/// release checkouts are full by design (`release.yml` pins `fetch-depth: 0`
/// on every leg, and `assert-release-tag.sh` refuses a shallow one before the
/// pin is ever written).
fn resolve(root: Option<&Path>, pin: Option<String>) -> Resolution {
    if let Some(v) = pin {
        return Resolution::Pinned(v);
    }
    let Some(root) = root else {
        // Not a git checkout at all. `derive_version_at` would fail on its
        // own for the honest reason; do not claim shallowness never observed.
        return Resolution::Unavailable;
    };

    // A shallow clone cannot be asked how many commits landed this month, and
    // it does not fail when asked — it answers with the truncated count. CI's
    // own workflow says so out loud: the bundle job pins `fetch-depth: 0`
    // because the default of 1 "makes both count 1 — the build would still
    // pass". Several other checkouts are still shallow, so deriving here
    // without this guard would stamp `<year>.<month>.1` into them and call it a
    // version. Refuse instead.
    if is_shallow_at(root) {
        return Resolution::Shallow;
    }

    match derive_version_at(root) {
        Some(v) => Resolution::Derived(v),
        None => Resolution::Unavailable,
    }
}

/// An explicitly pinned version wins over deriving one.
///
/// `scripts/publish.sh` mints the tag first and then pins `MARKETING_VERSION`
/// for the build precisely so the artifact carries the version the tag carries
/// — "build.sh must not re-resolve it (the re-derive would diverge the day the
/// bump ladder first fires)". The same reasoning binds this helper, which is a
/// consumer of that pin and not a second opinion about it.
fn pinned_version() -> Option<String> {
    normalize_pin(std::env::var("MARKETING_VERSION").ok())
}

/// The pin's parsing, split out from reading the environment so it can be
/// tested without a process-wide `set_var` race.
///
/// **Whitespace-only is unset, not a version.** A pin arrives through shell and
/// CI plumbing where an empty value and an absent one are easy to confuse
/// (`MARKETING_VERSION=` in a job env, a trailing newline from a command
/// substitution), and treating either as a real pin would stamp an empty string
/// into a binary — a version that is not merely wrong but unspellable.
fn normalize_pin(raw: Option<String>) -> Option<String> {
    let v = raw?.trim().to_string();
    (!v.is_empty()).then_some(v)
}

/// The calling crate's manifest directory — cargo sets it for every build
/// script, and it is a path *inside* the repo, which is all `git rev-parse`
/// needs. Deliberately `env::var` rather than `env!`: the macro would bake in
/// **this** crate's directory at the time this crate was compiled, which is a
/// different (and, under a vendored build, wrong) answer.
fn manifest_dir() -> Option<String> {
    std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .filter(|s| !s.is_empty())
}

fn repo_root() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(manifest_dir()?)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn git_path(rel: &str) -> Option<String> {
    let root = repo_root()?;
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", rel])
        .current_dir(&root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    absolutize(&root, String::from_utf8_lossy(&out.stdout).trim())
}

/// Resolve what `git rev-parse --git-path` printed against the repo root.
///
/// It answers relatively from an ordinary checkout (`.git/HEAD`) and absolutely
/// from a worktree, and `cargo:rerun-if-changed=` needs a path cargo can
/// actually stat — a relative one is resolved against the *crate* directory,
/// not the repo root, so it would silently watch a file that does not exist and
/// the version would go stale rather than fail.
fn absolutize(root: &str, printed: &str) -> Option<String> {
    if printed.is_empty() {
        return None;
    }
    let p = Path::new(printed);
    Some(if p.is_absolute() {
        printed.to_string()
    } else {
        Path::new(root).join(p).to_string_lossy().into_owned()
    })
}

/// Path of the ref `HEAD` points at, or `None` on a detached HEAD — where there
/// is no ref file to watch and `HEAD` itself already carries the commit.
fn current_ref_path() -> Option<String> {
    let root = repo_root()?;
    let out = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let r = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!r.is_empty()).then(|| git_path(&r))?
}

/// Whether the checkout at `root` is a shallow clone.
///
/// `true` only on git's own word (`--is-shallow-repository` printing `true`).
/// A directory that is not a checkout, or a git that cannot be run, is
/// `false`: shallowness is a fact about a repository, and there is none here
/// to be shallow — the derivation then fails on its own for the honest reason
/// rather than this function claiming something it did not observe.
fn is_shallow_at(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
}

fn derive_version_at(root: &Path) -> Option<String> {
    let script = root.join("scripts/get-version-info.sh");
    if !script.exists() {
        return None;
    }
    let out = Command::new("bash")
        .arg(&script)
        .arg("--version")
        .current_dir(root)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_taken_verbatim_once_trimmed() {
        assert_eq!(
            normalize_pin(Some("2026.9.3".to_string())),
            Some("2026.9.3".to_string())
        );
        // A command substitution's trailing newline is not part of the version.
        assert_eq!(
            normalize_pin(Some("  2026.9.3\n".to_string())),
            Some("2026.9.3".to_string())
        );
    }

    /// `MARKETING_VERSION=` in a job env is *unset*, not a version. Returning
    /// `Some("")` here would stamp an empty string into a binary and skip the
    /// derivation that would have produced a real one.
    #[test]
    fn an_empty_or_blank_pin_is_absent_rather_than_a_version() {
        assert_eq!(normalize_pin(None), None);
        assert_eq!(normalize_pin(Some(String::new())), None);
        assert_eq!(normalize_pin(Some("   \t\n".to_string())), None);
    }

    /// Compared as a `Path`, not as a string: `Path::join` emits the
    /// platform's separator, so Windows produces `/repo\.git/HEAD` and an
    /// assertion against the literal `"/repo/.git/HEAD"` fails there for a
    /// reason that has nothing to do with what this function is for. The
    /// contract is "the relative path was resolved against the root", and a
    /// `Path` comparison is what states that without pinning a separator.
    #[test]
    fn a_relative_git_path_is_resolved_against_the_repo_root() {
        let joined = absolutize("/repo", ".git/HEAD").expect("a relative path resolves to a path");
        assert_eq!(Path::new(&joined), Path::new("/repo").join(".git/HEAD"));
    }

    /// A worktree's `git rev-parse --git-path` answers absolutely — joining
    /// that onto the root would produce `/repo//elsewhere/...`, a path nothing
    /// watches, and the version would then never be recomputed on a commit.
    #[test]
    fn an_absolute_git_path_is_left_alone() {
        assert_eq!(
            absolutize("/repo", "/elsewhere/.git/worktrees/w/HEAD"),
            Some("/elsewhere/.git/worktrees/w/HEAD".to_string())
        );
    }

    #[test]
    fn an_empty_git_path_is_none_rather_than_the_repo_root() {
        assert_eq!(absolutize("/repo", ""), None);
    }

    // ---- real repositories, in a temp dir, through the real git ------------
    //
    // No `tempfile`: this crate carries no dependencies (Cargo.toml says why),
    // and a dev-dependency here would still be a second thing to keep in step
    // across two build graphs. A unique directory under `std::env::temp_dir()`
    // and a Drop guard are all these cases need.

    struct TempDir(PathBuf);

    /// Distinct per call within this process. Cargo runs tests on parallel
    /// threads, and macOS's clock is microsecond-granular, so two fixtures
    /// built in the same microsecond by the same pid used to share a name —
    /// measured at roughly one run in three. The counter is what makes the
    /// name unique; the clock only keeps names from colliding across runs.
    static TEMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the clock is after 1970")
                .as_nanos();
            let seq = TEMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "solador-buildversion-{label}-{}-{seq}-{nanos}",
                std::process::id()
            ));
            // `create_dir`, not `_all`: an existing directory here is a
            // collision, and it must fail HERE with the path named rather
            // than inside the first `git init` with git's own message.
            std::fs::create_dir(&dir)
                .unwrap_or_else(|e| panic!("create temp dir {}: {e}", dir.display()));
            TempDir(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Run git in `dir`, panicking with its stderr on failure so a broken
    /// fixture reads as what it is rather than as a failed assertion later.
    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            // A fixture identity, so `commit` works on a machine with none
            // configured (CI) and never touches the developer's global config.
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            // An inherited GIT_DIR (a `git rebase -x` exports one) would point
            // every command below at the real checkout. This strips it for
            // fixture CONSTRUCTION only — the functions under test run git
            // with the inherited environment, as production does — so the
            // fixture also proves, right after `init`, that no such variable
            // is in force (see `repos`).
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            // Nor may the developer's global config reach the fixture: a
            // `commit.gpgsign = true` or a `core.hooksPath` there would fail
            // every `commit` here for a reason that has nothing to do with
            // this crate. Both need git >= 2.32; `init -b` already needs 2.28.
            .env("GIT_CONFIG_GLOBAL", dir.join("no-global-gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git is on PATH");
        assert!(
            out.status.success(),
            "git {} in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A `file://` URL for `path`, which is what `git clone --depth` needs: a
    /// plain local path is cloned by hardlinking and `--depth` is ignored
    /// with a warning, so the "shallow" clone would not be shallow at all.
    fn file_url(path: &Path) -> String {
        let p = path.to_string_lossy().replace('\\', "/");
        if p.starts_with('/') {
            format!("file://{p}")
        } else {
            format!("file:///{p}")
        }
    }

    /// An origin with two commits, a full clone of it, and a `--depth 1`
    /// clone of it — the three shapes the refusal distinguishes.
    struct Repos {
        _tmp: TempDir,
        origin: PathBuf,
        full: PathBuf,
        shallow: PathBuf,
    }

    fn repos() -> Repos {
        let tmp = TempDir::new("repos");
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        git(&origin, &["init", "-q", "-b", "main"]);
        // The functions under test inherit this process's environment, as the
        // build script does. An inherited GIT_DIR would make every one of
        // their answers about some other repository, so prove there is none
        // in force: from a repository's top level, `--git-dir` answers the
        // relative `.git` for its own dir and an inherited GIT_DIR's path
        // otherwise (the shell suite's `versioning-test.sh` runs the same
        // control).
        let out = Command::new("git")
            .args(["rev-parse", "--git-dir"])
            .current_dir(&origin)
            .output()
            .expect("git is on PATH");
        let git_dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert_eq!(
            git_dir, ".git",
            "fixture: git in the fixture answers for '{git_dir}' — a GIT_DIR/GIT_WORK_TREE \
             is set in this process's environment (a `git rebase -x` exports one); unset it"
        );
        git(&origin, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&origin, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let url = file_url(&origin);
        let full = tmp.path().join("full");
        git(tmp.path(), &["clone", "-q", "--no-local", &url, "full"]);
        let shallow = tmp.path().join("shallow");
        git(
            tmp.path(),
            &["clone", "-q", "--no-local", "--depth", "1", &url, "shallow"],
        );
        Repos {
            _tmp: tmp,
            origin,
            full,
            shallow,
        }
    }

    #[test]
    fn a_depth_one_clone_is_shallow_and_its_origin_and_full_clone_are_not() {
        let r = repos();
        assert!(is_shallow_at(&r.shallow), "a --depth 1 clone is shallow");
        assert!(
            !is_shallow_at(&r.origin),
            "the origin it was cloned from is not"
        );
        assert!(!is_shallow_at(&r.full), "nor is a full clone of it");
        // The fixture is the shape it claims: the shallow clone really did
        // lose history, which is what makes its "commits this month" a lie.
        let count = |dir: &Path| {
            let out = Command::new("git")
                .args(["rev-list", "--count", "HEAD"])
                .current_dir(dir)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        assert_eq!(count(&r.full), "2");
        assert_eq!(
            count(&r.shallow),
            "1",
            "the truncated count the guard exists to refuse"
        );
    }

    /// "Not a checkout" is not "shallow". Claiming shallowness here would send
    /// a `fetch-depth: 0` remedy to someone whose problem is that there is no
    /// repository at all.
    #[test]
    fn a_directory_that_is_not_a_checkout_is_not_shallow() {
        let tmp = TempDir::new("notrepo");
        // `is_shallow_at` asks git in the directory itself, with the
        // inherited environment, and git discovers upward. On every CI runner
        // the temp dir is outside any checkout; on a developer machine whose
        // TMPDIR sits inside a repository git would find THAT repository and
        // answer for it, and this test would then be about the wrong thing.
        // So the probe asks exactly the question `is_shallow_at` will ask —
        // no ceiling, same discovery — and names the cause if it finds one.
        let out = Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "fixture: the temp dir {} is inside the checkout {} — this test needs a TMPDIR outside any repository",
            tmp.path().display(),
            String::from_utf8_lossy(&out.stdout).trim()
        );
        assert!(!is_shallow_at(tmp.path()));
    }

    /// The ordering that matters: a pin wins **before** the checkout is
    /// consulted — no observation of the checkout may override the minted
    /// tag's own number. Swapping the first two arms of `resolve` leaves
    /// every other test green and makes a pinned build in a shallow checkout
    /// version-less.
    #[test]
    fn a_pin_beats_a_shallow_checkout() {
        let r = repos();
        assert_eq!(
            resolve(Some(&r.shallow), Some("2030.1.9".to_string())),
            Resolution::Pinned("2030.1.9".to_string())
        );
        // ...and the same shallow checkout without a pin refuses.
        assert_eq!(resolve(Some(&r.shallow), None), Resolution::Shallow);
        // A pin also beats "no checkout at all".
        assert_eq!(
            resolve(None, Some("2030.1.9".to_string())),
            Resolution::Pinned("2030.1.9".to_string())
        );
        assert_eq!(resolve(None, None), Resolution::Unavailable);
    }

    /// A full checkout with no `scripts/get-version-info.sh` is
    /// `Unavailable`, never a made-up number: the script is the one place the
    /// CalVer is computed, and its absence is the honest reason.
    #[test]
    fn a_full_checkout_without_the_script_is_unavailable_not_a_version() {
        let r = repos();
        assert_eq!(resolve(Some(&r.full), None), Resolution::Unavailable);
    }

    /// The delegation itself: the repo's real `scripts/get-version-info.sh`
    /// copied into the full clone derives a CalVer through `resolve`.
    ///
    /// Not run on Windows, and the reason is a gap rather than a principle:
    /// `derive_version_at` runs `bash <native path>` there too (a local
    /// Windows dev build reaches it — the release leg is pinned and the
    /// `windows-tests` checkout is shallow, so neither CI path does), and
    /// whether Git Bash takes that path is untested in this repo. A red here
    /// would be a `derive_version_at` finding, out of this crate's scope.
    #[cfg(unix)]
    #[test]
    fn a_full_checkout_with_the_script_derives_a_calver() {
        // The script reads its seams and the pin from the inherited
        // environment, and the assertion below cannot tell an echoed pin from
        // a derivation — so a shell that exports one makes this test
        // meaningless. Say so rather than pass.
        for seam in [
            "MARKETING_VERSION",
            "VERSION_PATCH_OVERRIDE",
            "VERSION_DATE_OVERRIDE",
        ] {
            assert!(
                std::env::var_os(seam).is_none(),
                "{seam} is set in this process's environment; unset it to run this test"
            );
        }
        let r = repos();
        let this_repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script = this_repo.join("scripts/get-version-info.sh");
        assert!(script.exists(), "the mint is at {}", script.display());
        std::fs::create_dir_all(r.full.join("scripts")).unwrap();
        std::fs::copy(&script, r.full.join("scripts/get-version-info.sh")).unwrap();
        match resolve(Some(&r.full), None) {
            Resolution::Derived(v) => {
                let parts: Vec<&str> = v.split('.').collect();
                assert_eq!(parts.len(), 3, "YYYY.M.P, got {v}");
                assert!(
                    parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
                    "YYYY.M.P, got {v}"
                );
                // The fixture's two commits were made moments ago, so "commits
                // this month" is 2 — which no echoed pin would say. (A run
                // straddling midnight UTC on the first of a month could count
                // 1 or 2; that window is under a second, once a month.)
                assert!(
                    parts[2] == "2" || parts[2] == "1",
                    "the patch is the fixture's commit count this month, got {v}"
                );
            }
            other => panic!("expected Derived, got {other:?}"),
        }
        // And the same script in the SHALLOW clone is never asked: the
        // refusal comes first.
        std::fs::create_dir_all(r.shallow.join("scripts")).unwrap();
        std::fs::copy(&script, r.shallow.join("scripts/get-version-info.sh")).unwrap();
        assert_eq!(resolve(Some(&r.shallow), None), Resolution::Shallow);
    }
}
