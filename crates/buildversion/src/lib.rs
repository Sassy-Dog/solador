//! The git-derived versions, published to a crate at build time.
//!
//! Called from a `build.rs`. Two crates use it and they ship on different
//! platforms — `app/src-tauri` (the cockpit) and `agent` (the metrics agent,
//! cross-compiled to four targets by `scripts/build-agent.sh`) — which is why
//! it lives here rather than being written twice. The plumbing is fiddly
//! enough (a worktree's `.git` *file*, the ref file a commit actually rewrites,
//! the shallow-clone refusal) that two copies would diverge at the first fix.
//!
//! **This does not compute a version.** `scripts/get-version-info.sh` is the
//! repo's §3 interface-contract owner and every version algorithm exists
//! exactly once, there (`docs/VERSIONING.md`). Re-deriving the
//! month-and-commit-count arithmetic here — or the agent's tag-and-commits
//! arithmetic — would be a second implementation, and the day the two disagree
//! the artifact and the version it reports disagree.
//!
//! **Two numbers, two entry points, nothing shared between them (#490).** The
//! cockpit's version is the marketing CalVer ([`emit_marketing_version`], pin
//! `MARKETING_VERSION`, script flag `--version`); the agent's is its own
//! ([`emit_agent_marketing_version`], pin `AGENT_MARKETING_VERSION`, flag
//! `--agent-version`). The agent entry point **ignores `MARKETING_VERSION`
//! entirely**: that pin is a desktop release's number, and honouring it would
//! stamp an app version into an agent that has not been released.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment variable the cockpit entry point publishes. Read back with
/// `option_env!("SOLADOR_MARKETING_VERSION")`.
pub const ENV_VAR: &str = "SOLADOR_MARKETING_VERSION";

/// The environment variable the agent entry point publishes. Read back with
/// `option_env!("SOLADOR_AGENT_MARKETING_VERSION")`. A different name from
/// [`ENV_VAR`] on purpose: `cargo:rustc-env` is per crate, so the two could
/// share one, but a crate reading the other number's name is a bug that should
/// not compile.
pub const AGENT_ENV_VAR: &str = "SOLADOR_AGENT_MARKETING_VERSION";

/// Which number is being derived. Everything that differs between the cockpit
/// and the agent lives here, so the ordering in [`resolve`] cannot differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Track {
    /// The cockpit's marketing CalVer.
    Cockpit,
    /// The agent's own version.
    Agent,
}

impl Track {
    /// The pin: an explicit version that wins over deriving one.
    fn pin_var(self) -> &'static str {
        match self {
            Track::Cockpit => "MARKETING_VERSION",
            Track::Agent => "AGENT_MARKETING_VERSION",
        }
    }

    /// The variable the resolved version is published under.
    fn published_as(self) -> &'static str {
        match self {
            Track::Cockpit => ENV_VAR,
            Track::Agent => AGENT_ENV_VAR,
        }
    }

    /// The `scripts/get-version-info.sh` flag that answers for this number.
    fn script_flag(self) -> &'static str {
        match self {
            Track::Cockpit => "--version",
            Track::Agent => "--agent-version",
        }
    }

    /// What the build log calls it.
    fn noun(self) -> &'static str {
        match self {
            Track::Cockpit => "marketing version",
            Track::Agent => "agent version",
        }
    }
}

/// Publish the git-derived CalVer to the calling crate as
/// [`ENV_VAR`], or publish **nothing at all** if it cannot be derived honestly.
///
/// Consumers read it through `option_env!`, so "not emitted" is a representable
/// state — the cockpit's About row renders `—`. That asymmetry is the whole
/// point: the value this replaced was `env!("CARGO_PKG_VERSION")`, a
/// real-looking version that has never been a release.
pub fn emit_marketing_version() {
    emit(Track::Cockpit);
}

/// Publish the agent's own version to the calling crate as [`AGENT_ENV_VAR`],
/// or publish **nothing at all** if it cannot be derived honestly: the agent's
/// `--version` refuses and `/v1/health` omits the key, rather than answering
/// with a stand-in.
///
/// The order is the cockpit's: the pin `AGENT_MARKETING_VERSION` first, then
/// the shallow-clone refusal, then `scripts/get-version-info.sh
/// --agent-version`. `MARKETING_VERSION` is never read. Because a source
/// build's version is `<base>+dev.<k>.g<sha>` and the base is a *tag*, the
/// re-run set also watches `packed-refs` and `refs/tags` (see
/// [`watched_paths`]): fetching an `agent-v*` tag moves the base without moving
/// `HEAD`.
pub fn emit_agent_marketing_version() {
    emit(Track::Agent);
}

fn emit(track: Track) {
    let root = repo_root().map(PathBuf::from);

    // Re-run when the pin changes, and when HEAD moves — and for the agent,
    // when a tag does.
    println!("cargo:rerun-if-env-changed={}", track.pin_var());
    if let Some(root) = root.as_deref() {
        for p in watched_paths(root, track) {
            println!("cargo:rerun-if-changed={p}");
        }
    }

    match resolve(root.as_deref(), pinned_version(track), track) {
        Resolution::Pinned(v) | Resolution::Derived(v) => {
            println!("cargo:rustc-env={}={v}", track.published_as());
        }
        Resolution::Shallow => println!(
            "cargo:warning={} unavailable: shallow clone (fetch-depth: 0 is required to count commits)",
            track.noun()
        ),
        Resolution::Unavailable(why) => {
            println!("cargo:warning={} unavailable: {why}", track.noun());
        }
    }
}

/// The paths whose change must re-run the build script.
///
/// Watching `HEAD` alone is not enough: committing rewrites the *ref file*
/// while `HEAD` keeps saying `ref: refs/heads/<branch>`, so a HEAD-only watch
/// catches branch switches and misses every commit — which is exactly what the
/// CalVer patch counts. Both paths come from `--git-path`, which resolves
/// through a worktree's `.git` *file*; this repo runs agents in worktrees
/// routinely.
///
/// The agent adds the places a tag can appear: `refs/tags` (a loose tag, from a
/// local mint or a fetch) and `packed-refs` (where `git pack-refs`, `gc` and a
/// clone leave them). Without them, fetching `agent-v2026.11.1` onto a checkout
/// that is already at that commit would move the version from
/// `<base>+dev.…` to `2026.11.1` and cargo would never notice.
///
/// **`HEAD` and the branch ref are watched whether or not the file exists, as
/// they always were** — after `git gc` or `pack-refs` the branch's loose ref is
/// gone, cargo then treats the missing path as always-dirty and re-runs the
/// script every build, and that is the *safe* direction: filtering the path out
/// would leave the next commit (which writes a loose ref again) unwatched, and
/// the version would go stale. **The two tag paths the agent adds are the
/// opposite case and are emitted only when they exist.** Cargo treats a watched
/// path that does not exist as always-dirty and re-runs the script — and rustc
/// — on every build (measured, and `agent/build.rs` records the same trap for
/// its own optional key file), and these two are optional: a repository that
/// has never packed its refs has no `packed-refs`, and one that has no tag yet
/// has no `refs/tags` directory. Either appears only through an operation that
/// also moves `HEAD` or rewrites the other, so the watch that remains still
/// catches it.
fn watched_paths(root: &Path, track: Track) -> Vec<String> {
    let mut rels = vec!["HEAD".to_string()];
    if let Some(r) = current_ref_at(root) {
        rels.push(r);
    }
    let mut paths: Vec<String> = rels
        .iter()
        .filter_map(|rel| git_path_at(root, rel))
        .collect();
    if track == Track::Agent {
        paths.extend(
            ["packed-refs", "refs/tags"]
                .iter()
                .filter_map(|rel| git_path_at(root, rel))
                .filter(|p| Path::new(p).exists()),
        );
    }
    paths
}

/// What [`emit`] decided, before it prints anything.
///
/// Split from the printing so the **ordering** is a value a test can hold:
/// a pin wins before the checkout is even looked at, a shallow checkout
/// refuses before the script is asked, and only then is the script's answer
/// consulted. Each arm is a different sentence to the build log, and two of
/// them publish nothing — which is the design, not a failure to handle.
#[derive(Debug, PartialEq, Eq)]
enum Resolution {
    /// The track's pin was set: emitted verbatim, whatever the checkout is.
    Pinned(String),
    /// The checkout is shallow: nothing is published, and the log says why.
    Shallow,
    /// `scripts/get-version-info.sh` answered for this track.
    Derived(String),
    /// No checkout, no script, or the script failed: nothing is published, and
    /// the string is the reason the build log gets — the script's own first
    /// line of stderr when it had one (a clone without the agent's base tag
    /// says `git fetch --tags` there), never a generic shrug.
    Unavailable(String),
}

/// The decision, in order. `root` is the repository root when there is one;
/// `pin` is the already-normalised pin for `track`.
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
fn resolve(root: Option<&Path>, pin: Option<String>, track: Track) -> Resolution {
    if let Some(v) = pin {
        return Resolution::Pinned(v);
    }
    let Some(root) = root else {
        // Not a git checkout at all. `derive_version_at` would fail on its
        // own for the honest reason; do not claim shallowness never observed.
        return Resolution::Unavailable("not inside a git checkout".to_string());
    };

    // A shallow clone cannot be asked how many commits landed this month (or
    // lie since the agent's base), and it does not fail when asked — it
    // answers with the truncated count. CI's own workflow says so out loud: the
    // bundle job pins `fetch-depth: 0` because the default of 1 "makes both
    // count 1 — the build would still pass". Several other checkouts are still
    // shallow, so deriving here without this guard would stamp
    // `<year>.<month>.1` into them and call it a version. Refuse instead.
    if is_shallow_at(root) {
        return Resolution::Shallow;
    }

    match derive_version_at(root, track) {
        Ok(v) => Resolution::Derived(v),
        Err(why) => Resolution::Unavailable(why),
    }
}

/// An explicitly pinned version wins over deriving one.
///
/// `scripts/publish.sh` mints the tag first and then pins `MARKETING_VERSION`
/// for the build precisely so the artifact carries the version the tag carries
/// — "build.sh must not re-resolve it (the re-derive would diverge the day the
/// bump ladder first fires)". The same reasoning binds this helper, which is a
/// consumer of that pin and not a second opinion about it. The agent's release
/// build pins `AGENT_MARKETING_VERSION` from its `agent-v*` tag in the same
/// way, and **only** that name: the other track's pin is not this track's.
fn pinned_version(track: Track) -> Option<String> {
    pin_from(track, |name| std::env::var(name).ok())
}

/// The pin for `track`, read through `lookup` — the environment in
/// production, a table in a test, which is what lets a test show that the agent
/// ignores `MARKETING_VERSION` without a process-wide `set_var` race.
fn pin_from(track: Track, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    normalize_pin(lookup(track.pin_var()))
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

fn git_path_at(root: &Path, rel: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--git-path", rel])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    absolutize(
        &root.to_string_lossy(),
        String::from_utf8_lossy(&out.stdout).trim(),
    )
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

/// The ref `HEAD` points at (`refs/heads/<branch>`), or `None` on a detached
/// HEAD — where there is no ref file to watch and `HEAD` itself already
/// carries the commit.
fn current_ref_at(root: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let r = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!r.is_empty()).then_some(r)
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

/// What the script answered, or **why it did not**: `Err` carries the first
/// line of its stderr when it wrote one (`get-version-info.sh` names the cause —
/// no base tag in this checkout, a shallow clone — and says what to do), else a
/// plain statement of which step failed. The reason is what a person reads in
/// the build log, so losing it leaves them a version-less binary and no lead.
fn derive_version_at(root: &Path, track: Track) -> Result<String, String> {
    let script = root.join("scripts/get-version-info.sh");
    if !script.exists() {
        return Err("scripts/get-version-info.sh is not in this checkout".to_string());
    }
    let out = Command::new("bash")
        .arg(&script)
        .arg(track.script_flag())
        .current_dir(root)
        .output()
        .map_err(|e| format!("could not run bash scripts/get-version-info.sh: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty());
        return Err(match first {
            Some(line) => format!(
                "scripts/get-version-info.sh {}: {}",
                track.script_flag(),
                line.trim()
            ),
            None => format!(
                "scripts/get-version-info.sh {} failed without saying why",
                track.script_flag()
            ),
        });
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        return Err(format!(
            "scripts/get-version-info.sh {} printed nothing",
            track.script_flag()
        ));
    }
    Ok(version)
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
        for track in [Track::Cockpit, Track::Agent] {
            let r = repos();
            assert_eq!(
                resolve(Some(&r.shallow), Some("2030.1.9".to_string()), track),
                Resolution::Pinned("2030.1.9".to_string()),
                "{track:?}"
            );
            // ...and the same shallow checkout without a pin refuses.
            assert_eq!(
                resolve(Some(&r.shallow), None, track),
                Resolution::Shallow,
                "{track:?}"
            );
            // A pin also beats "no checkout at all".
            assert_eq!(
                resolve(None, Some("2030.1.9".to_string()), track),
                Resolution::Pinned("2030.1.9".to_string()),
                "{track:?}"
            );
            assert_eq!(
                resolve(None, None, track),
                Resolution::Unavailable("not inside a git checkout".to_string()),
                "{track:?}"
            );
        }
    }

    /// A full checkout with no `scripts/get-version-info.sh` is
    /// `Unavailable`, never a made-up number: the script is the one place the
    /// CalVer is computed, and its absence is the honest reason — which the
    /// resolution carries, so the build log can say it.
    #[test]
    fn a_full_checkout_without_the_script_is_unavailable_not_a_version() {
        let r = repos();
        for track in [Track::Cockpit, Track::Agent] {
            match resolve(Some(&r.full), None, track) {
                Resolution::Unavailable(why) => assert!(
                    why.contains("scripts/get-version-info.sh is not in this checkout"),
                    "{track:?}: {why}"
                ),
                other => panic!("{track:?}: expected Unavailable, got {other:?}"),
            }
        }
    }

    /// The script's OWN reason reaches the build log: an agent build in a full
    /// clone that never fetched the base tag says so — and what to run — rather
    /// than a generic "did not produce one". Unix-only like the other tests
    /// that run the real script.
    #[cfg(unix)]
    #[test]
    fn the_scripts_reason_for_refusing_is_carried_into_the_resolution() {
        for seam in ["VERSION_DATE_OVERRIDE", "AGENT_MARKETING_VERSION"] {
            assert!(
                std::env::var_os(seam).is_none(),
                "{seam} is set in this process's environment; unset it to run this test"
            );
        }
        let r = repos();
        install_scripts(&r.full);
        // `repos()`'s full clone has no tags, and no `agent-v*` tag is reachable
        // either, so the base is the legacy one — which this clone lacks.
        match resolve(Some(&r.full), None, Track::Agent) {
            Resolution::Unavailable(why) => {
                assert!(why.contains("--agent-version"), "{why}");
                assert!(why.contains("is not in this checkout"), "{why}");
                assert!(why.contains("git fetch --tags"), "{why}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// **The agent ignores `MARKETING_VERSION`** (#490): that pin is a desktop
    /// release's number. Read through a lookup table rather than the process
    /// environment, so this is not a `set_var` race — the production lookup is
    /// `std::env::var` and nothing else.
    #[test]
    fn the_agent_pin_is_its_own_and_marketing_version_is_ignored() {
        let only_marketing = |name: &str| (name == "MARKETING_VERSION").then(|| "2030.1.9".into());
        let only_agent =
            |name: &str| (name == "AGENT_MARKETING_VERSION").then(|| "2031.2.3".into());
        let both = |name: &str| match name {
            "MARKETING_VERSION" => Some("2030.1.9".to_string()),
            "AGENT_MARKETING_VERSION" => Some("2031.2.3".to_string()),
            _ => None,
        };

        assert_eq!(
            pin_from(Track::Cockpit, only_marketing),
            Some("2030.1.9".into())
        );
        assert_eq!(
            pin_from(Track::Agent, only_marketing),
            None,
            "a desktop pin must not stamp an app version into the agent"
        );
        assert_eq!(pin_from(Track::Agent, only_agent), Some("2031.2.3".into()));
        assert_eq!(
            pin_from(Track::Cockpit, only_agent),
            None,
            "and the agent's pin is not the cockpit's"
        );
        // Both set: each track takes its own.
        assert_eq!(pin_from(Track::Cockpit, both), Some("2030.1.9".into()));
        assert_eq!(pin_from(Track::Agent, both), Some("2031.2.3".into()));
        // The same whitespace rule on both.
        assert_eq!(pin_from(Track::Agent, |_| Some("  \n".to_string())), None);
        // They publish under different names and ask the script different
        // questions, so neither number can answer for the other.
        assert_ne!(Track::Cockpit.published_as(), Track::Agent.published_as());
        assert_eq!(Track::Cockpit.script_flag(), "--version");
        assert_eq!(Track::Agent.script_flag(), "--agent-version");
    }

    /// With only `MARKETING_VERSION` in play, a shallow clone is still refused
    /// for the agent — the cockpit's pin does not rescue it into a version.
    /// (The pin the agent DOES honour is covered by `a_pin_beats_a_shallow_checkout`.)
    #[test]
    fn a_shallow_clone_is_refused_for_the_agent_even_with_marketing_version_set() {
        let r = repos();
        let pin = pin_from(Track::Agent, |name| {
            (name == "MARKETING_VERSION").then(|| "2030.1.9".to_string())
        });
        assert_eq!(
            resolve(Some(&r.shallow), pin, Track::Agent),
            Resolution::Shallow
        );
        let pin = pin_from(Track::Cockpit, |name| {
            (name == "MARKETING_VERSION").then(|| "2030.1.9".to_string())
        });
        assert_eq!(
            resolve(Some(&r.shallow), pin, Track::Cockpit),
            Resolution::Pinned("2030.1.9".to_string()),
            "while the cockpit's own pin still wins"
        );
    }

    /// `git`'s stdout, trimmed, for the cases that need an answer rather than a
    /// side effect. (Unix-only like its one caller, which runs the shell script.)
    #[cfg(unix)]
    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
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
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The tag-watching half of the agent entry point: a tag can move the
    /// agent's version without moving `HEAD`, so the agent's re-run set carries
    /// `refs/tags` — and `packed-refs` once refs are packed — and the
    /// cockpit's, whose version no tag can move, carries neither. A watched
    /// path that does not exist re-runs the build on every build, so nothing
    /// absent is emitted.
    #[test]
    fn the_agent_watches_tags_and_the_cockpit_does_not() {
        let r = repos();
        let ends =
            |paths: &[String], tail: &str| paths.iter().any(|p| Path::new(p).ends_with(tail));

        let agent = watched_paths(&r.full, Track::Agent);
        let cockpit = watched_paths(&r.full, Track::Cockpit);
        assert!(ends(&agent, "HEAD"), "{agent:?}");
        assert!(ends(&agent, "refs/heads/main"), "{agent:?}");
        assert!(
            ends(&agent, "refs/tags"),
            "the agent watches tags: {agent:?}"
        );
        assert!(ends(&cockpit, "HEAD"), "{cockpit:?}");
        assert!(ends(&cockpit, "refs/heads/main"), "{cockpit:?}");
        assert!(!ends(&cockpit, "refs/tags"), "{cockpit:?}");
        assert!(!ends(&cockpit, "packed-refs"), "{cockpit:?}");
        // The two OPTIONAL paths are emitted only when they exist (a missing
        // watched path makes cargo re-run every build); `HEAD` and the branch
        // ref are not optional and are tested below.
        for p in &agent {
            if Path::new(p).ends_with("refs/tags") || Path::new(p).ends_with("packed-refs") {
                assert!(
                    Path::new(p).exists(),
                    "{p} does not exist and would make cargo re-run every build"
                );
            }
        }

        // Whether `packed-refs` exists depends on how the clone was made, so
        // force each state rather than assume one.
        let packed = r.full.join(".git/packed-refs");
        let _ = std::fs::remove_file(&packed);
        assert!(
            !ends(&watched_paths(&r.full, Track::Agent), "packed-refs"),
            "an absent packed-refs is not watched"
        );
        git(&r.full, &["tag", "agent-v2026.11.1"]);
        git(&r.full, &["pack-refs", "--all"]);
        assert!(packed.exists(), "fixture: pack-refs wrote packed-refs");
        let agent = watched_paths(&r.full, Track::Agent);
        assert!(ends(&agent, "packed-refs"), "{agent:?}");
        assert!(
            !ends(&watched_paths(&r.full, Track::Cockpit), "packed-refs"),
            "the cockpit still does not"
        );

        // After packing, the branch's loose ref is GONE — and is still watched,
        // for both tracks: the next commit writes it again, and a watch that
        // dropped the path because it was missing would never see that commit.
        // (Cargo re-runs the script every build meanwhile, which is the safe
        // direction and what the code did before the agent existed.)
        assert!(
            !r.full.join(".git/refs/heads/main").exists(),
            "fixture: pack-refs removed the loose branch ref"
        );
        for track in [Track::Agent, Track::Cockpit] {
            let paths = watched_paths(&r.full, track);
            assert!(ends(&paths, "refs/heads/main"), "{track:?}: {paths:?}");
            assert!(ends(&paths, "HEAD"), "{track:?}: {paths:?}");
        }
    }

    /// The legacy base the agent derives against when no `agent-v*` tag is
    /// reachable: what the script itself would read — the environment's
    /// `LAST_COMBINED_AGENT_RELEASE` (`./dev` exports it, by sourcing
    /// `scripts/config.sh`) else that file's line. Computed the same way here
    /// so the test holds in both worlds.
    #[cfg(unix)]
    fn legacy_base() -> String {
        if let Some(v) = std::env::var("LAST_COMBINED_AGENT_RELEASE")
            .ok()
            .filter(|s| !s.is_empty())
        {
            return v;
        }
        let config = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/config.sh"),
        )
        .expect("scripts/config.sh");
        config
            .lines()
            .find_map(|l| l.strip_prefix("export LAST_COMBINED_AGENT_RELEASE=\""))
            .map(|v| v.trim_end_matches('"').to_string())
            .expect("config.sh exports LAST_COMBINED_AGENT_RELEASE")
    }

    /// Copy the repo's real scripts into a clone, so `derive_version_at` runs
    /// the real algorithm there.
    #[cfg(unix)]
    fn install_scripts(into: &Path) {
        let this_repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        std::fs::create_dir_all(into.join("scripts")).unwrap();
        for name in ["get-version-info.sh", "config.sh"] {
            let from = this_repo.join("scripts").join(name);
            assert!(from.exists(), "{} is missing", from.display());
            std::fs::copy(&from, into.join("scripts").join(name)).unwrap();
        }
    }

    /// The agent's delegation, against a real clone and the real script: with
    /// no `agent-v*` tag the version is `<legacy base>+dev.<k>.g<sha>`; an
    /// `agent-v*` tag at HEAD is that tag's version; and the cockpit's version
    /// of the same clone is a different question with a different answer.
    #[cfg(unix)]
    #[test]
    fn the_agent_derives_its_own_version_through_the_script() {
        for seam in [
            "MARKETING_VERSION",
            "AGENT_MARKETING_VERSION",
            "VERSION_PATCH_OVERRIDE",
            "VERSION_DATE_OVERRIDE",
        ] {
            assert!(
                std::env::var_os(seam).is_none(),
                "{seam} is set in this process's environment; unset it to run this test"
            );
        }
        let r = repos();
        install_scripts(&r.full);
        let base = legacy_base();
        let base_version = base.trim_start_matches('v').to_string();
        // The legacy base is a `v*` tag one commit before HEAD, as the bridge
        // release is for every checkout after it.
        git(&r.full, &["tag", &base, "HEAD~1"]);
        let sha = git_out(&r.full, &["rev-parse", "--short", "HEAD"]);

        assert_eq!(
            resolve(Some(&r.full), None, Track::Agent),
            Resolution::Derived(format!("{base_version}+dev.1.g{sha}")),
            "a source build is <base>+dev.<k>.g<sha>"
        );
        // Not the cockpit's number: that one counts this month's commits.
        let cockpit = resolve(Some(&r.full), None, Track::Cockpit);
        assert!(
            matches!(&cockpit, Resolution::Derived(v) if !v.contains("+dev")),
            "{cockpit:?}"
        );

        // An agent tag at HEAD is the version itself, and a reachable one is
        // the base of what follows it.
        git(&r.full, &["tag", "agent-v2026.11.1"]);
        assert_eq!(
            resolve(Some(&r.full), None, Track::Agent),
            Resolution::Derived("2026.11.1".to_string())
        );
        git(&r.full, &["commit", "-q", "--allow-empty", "-m", "three"]);
        let sha = git_out(&r.full, &["rev-parse", "--short", "HEAD"]);
        assert_eq!(
            resolve(Some(&r.full), None, Track::Agent),
            Resolution::Derived(format!("2026.11.1+dev.1.g{sha}")),
            "the highest reachable agent-v* tag is the base once one exists"
        );

        // And the same script in the SHALLOW clone is never asked.
        install_scripts(&r.shallow);
        assert_eq!(
            resolve(Some(&r.shallow), None, Track::Agent),
            Resolution::Shallow
        );
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
        match resolve(Some(&r.full), None, Track::Cockpit) {
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
        assert_eq!(
            resolve(Some(&r.shallow), None, Track::Cockpit),
            Resolution::Shallow
        );
    }
}
