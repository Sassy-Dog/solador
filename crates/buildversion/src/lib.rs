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

use std::path::Path;
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

    if let Some(v) = pinned_version() {
        println!("cargo:rustc-env={ENV_VAR}={v}");
        return;
    }

    // A shallow clone cannot be asked how many commits landed this month, and
    // it does not fail when asked — it answers with the truncated count. CI's
    // own workflow says so out loud: the bundle job pins `fetch-depth: 0`
    // because the default of 1 "makes both count 1 — the build would still
    // pass". Several other checkouts are still shallow, so deriving here
    // without this guard would stamp `<year>.<month>.1` into them and call it a
    // version. Refuse instead.
    if is_shallow() {
        println!("cargo:warning=marketing version unavailable: shallow clone (fetch-depth: 0 is required to count commits)");
        return;
    }

    match derive_version() {
        Some(v) => println!("cargo:rustc-env={ENV_VAR}={v}"),
        None => println!(
            "cargo:warning=marketing version unavailable: scripts/get-version-info.sh did not produce one"
        ),
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
    let v = std::env::var("MARKETING_VERSION").ok()?;
    let v = v.trim().to_string();
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
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        return None;
    }
    let abs = Path::new(&p);
    Some(if abs.is_absolute() {
        p
    } else {
        Path::new(&root).join(abs).to_string_lossy().into_owned()
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

fn is_shallow() -> bool {
    let Some(root) = repo_root() else {
        // Not a git checkout at all. `derive_version` will fail on its own and
        // report the honest reason; do not claim shallowness we did not observe.
        return false;
    };
    Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
}

fn derive_version() -> Option<String> {
    let root = repo_root()?;
    let script = Path::new(&root).join("scripts/get-version-info.sh");
    if !script.exists() {
        return None;
    }
    let out = Command::new("bash")
        .arg(&script)
        .arg("--version")
        .current_dir(&root)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}
