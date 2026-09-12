//! `solador-agent update` and `solador-agent rollback` (#393, part of #381).
//!
//! An installed agent updating **itself**, in the binary rather than in a
//! shell script, so it behaves identically on macOS and Linux and is
//! testable in Rust. `docs/AGENT-DISTRIBUTION.md` §§4–5 is the design; what
//! follows is the contract as built.
//!
//! # What an update does, in order
//!
//! 1. Resolves the **installed service** through #392's contract — reading
//!    only: the executable path out of the systemd user unit's `ExecStart=`
//!    (Linux) or the LaunchAgent plist's `ProgramArguments` (macOS), the env
//!    file beside it, and the token/bind/port the running service was
//!    started with — read from that file the way `EnvironmentFile=` and the
//!    launcher read it, never `source`d. Root is refused outright; an
//!    install directory this user cannot write to (the pre-#392 `/opt`
//!    layout is the usual case) is refused with the installer's explicit
//!    migration step, before anything changes.
//! 2. Takes the **transaction lock** (`<bin>.update.lock` beside the
//!    resolved binary, `flock`-style, non-blocking). A second `update` or
//!    `rollback` on the same install — a manual one racing #394's scheduled
//!    job, say — reports *busy* and changes nothing. The lock is released by
//!    process exit, so a crashed updater cannot wedge the next one. Then the
//!    service manager must **answer** (`systemctl --user` / the `gui/<uid>`
//!    domain), and must not name this process as the service: a manager
//!    discovered unreachable at the restart, after the swap, is the
//!    half-applied update this whole design exists to prevent.
//! 3. Resolves the **concrete release** behind `/releases/latest` and fetches
//!    that tag's `agent-latest.json` and `.minisig`, so two independently
//!    moving redirects can never pair one release's feed with another's
//!    signature. The feed's **exact bytes** are verified under the compiled-in
//!    trust set before they are decoded; a re-serialised object is never
//!    what gets verified. The feed's `version` must be the tag's.
//! 4. Hashes the **installed executable** and compares it with the entry for
//!    this host's triple. Equal bytes mean *already current*: no binary
//!    download, no `.new`, no `.prev`, no restart — even when the feed's
//!    version differs. Bytes on disk are not a running service, though, so
//!    the health endpoint is asked for the version those bytes claim; a
//!    service serving something else (an earlier run interrupted between
//!    its swap and its restart) is a distinct failure that names the
//!    restart as the remedy, and a service serving them is exit 0.
//! 5. Otherwise the feed must be **newer** than the installed CalVer. Every
//!    release's feed signature is valid on its own, so an older, validly
//!    signed pair replayed onto a newer release would otherwise read as "the
//!    newest release wants these bytes". An installed binary that carries no
//!    version cannot be compared and is refused, not assumed older.
//! 6. Downloads the raw executable **into memory** and verifies its plain
//!    minisign signature (under either trusted key) *and* its SHA-256 against
//!    the authenticated entry — both, because they answer different
//!    questions — before a byte reaches disk.
//! 7. Stages the verified bytes as `<bin>.new` (mode 0755), executes the
//!    staged candidate's `--version`, and requires the feed's CalVer back. A
//!    candidate that cannot name the expected version is removed unrun.
//! 8. Copies the live executable to `<bin>.prev` (through a sibling and a
//!    rename, so a crash mid-copy cannot leave a truncated rollback anchor)
//!    and **renames** `.new` over the live path. A running executable is
//!    never overwritten in place (Linux answers `ETXTBSY`; arm64 macOS kills
//!    the process whose signed pages changed) and the live path is never
//!    absent for even an instant.
//! 9. Restarts the **metrics** service — never the process running this
//!    command, which is a separate process by construction and refuses to
//!    proceed if the service manager says it *is* the service — and polls
//!    the authenticated `/v1/health` until it reports the installed CalVer.
//!    A service-manager success, or an HTTP 200 carrying a stale or absent
//!    `version`, is not an installed update.
//! 10. If the restart or that verification fails, `.prev` is restored
//!     through the same stage-and-rename, the service is restarted again and
//!     checked for the previous version (or, for a `.prev` that carries none,
//!     for liveness). The command exits non-zero **either way** — a failed
//!     update is a failure even when recovery worked — and a recovery that
//!     itself failed is a *different* failure, named as such, never reported
//!     as a successful rollback.
//!
//! `rollback` is the explicit, offline form of step 10: no feed request,
//! refuses when there is no `.prev` without touching the live path, and
//! keeps the displaced live binary as the new `.prev` so it is itself
//! reversible.
//!
//! # Trust
//!
//! The public keys are **compiled in** by `build.rs` from
//! `agent/release-signing-key.pub` and, when committed,
//! `agent/release-signing-key-next.pub`: never fetched, never read from disk
//! at run time, never trust-on-first-use. Two accepted keys are what make a
//! rotation a release rather than a recall (§5); they do not revoke a
//! compromised key and they are not an anti-replay mechanism — the
//! newer-than check above is what covers replay. A build whose two keys are
//! the same key is refused at the first verification, because one key listed
//! twice is one key.
//!
//! # What this never does
//!
//! Log or transmit the bearer token anywhere but the local health probe;
//! escalate privilege (`sudo`, a helper); evaluate the env file as shell;
//! fall back to a source build, another release, or a TOFU key; or report a
//! state it did not observe.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use minisign_verify::{PublicKey, Signature};
use serde::Deserialize;
use sha2::{Digest, Sha256};

include!(concat!(env!("OUT_DIR"), "/trusted_keys.rs"));

/// Where releases live. The updater constructs every URL it fetches from
/// this base itself and holds the feed's own `url` fields to the same
/// construction; a feed can name no other host.
pub const RELEASE_BASE: &str = "https://github.com/Sassy-Dog/solador";

/// The feed's asset name on a release, and its signature's trusted comment.
pub const FEED_ASSET: &str = "agent-latest.json";

/// Every agent binary's asset name starts with this.
pub const BINARY_PREFIX: &str = "solador-agent";

/// The four published targets. A host whose triple is not one of these has
/// no update, not a different one.
pub const TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
];

/// The systemd user unit (Linux) — `install.sh` renders it, this restarts it.
pub const SYSTEMD_UNIT: &str = "solador-agent";

/// The LaunchAgent label (macOS). `SOLADOR_AGENT_LAUNCHD_LABEL` overrides it
/// for the test harness, which bootstraps a throwaway service beside any real
/// one; the override is validated like the installer validates it.
pub const LAUNCHD_LABEL: &str = "app.solador.agent";

/// The feed is small; a document this size is not a feed.
const FEED_CAP: usize = 1024 * 1024;

/// A release binary is a few MB. A body past this is not one, and is refused
/// before it fills memory.
const BINARY_CAP: usize = 64 * 1024 * 1024;

/// Bound on a candidate's `--version` — a candidate that hangs is refused,
/// not waited on forever.
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

/// Post-restart verification: `lib.sh`'s fifteen one-second polls.
pub const HEALTH_ATTEMPTS: u32 = 15;
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(1);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Everything `update` and `rollback` can refuse or fail on. One variant per
/// operator-facing outcome, because "busy", "the feed is forged", "the new
/// binary did not come up and the old one is back" and "the new binary did
/// not come up and the old one is NOT back" are four different mornings.
#[derive(Debug)]
pub enum UpdateError {
    /// Not one of the four published targets.
    UnsupportedPlatform { os: String, arch: String },
    /// #392's install contract could not be resolved, or was refused (a
    /// root-owned `/opt` layout, a unit that starts something else).
    Install(String),
    /// Another `update`/`rollback` holds the transaction lock. `holder` is
    /// the note the holder wrote into it (`pid=… since=…`), when readable.
    Busy {
        lock: PathBuf,
        holder: Option<String>,
    },
    /// This process *is* the metrics service; restarting it would kill the
    /// updater mid-transaction.
    IsTheService { pid: u32 },
    /// Running as root. The install is user-owned and the service is a user
    /// unit / LaunchAgent; root would target root's manager and leave
    /// root-owned siblings beside the user's binary.
    Privileged { euid: u32 },
    /// Discovery or a download failed at the transport.
    Network { what: String, reason: String },
    /// `/releases/latest` did not land on a CalVer tag.
    Discovery(String),
    /// The compiled-in trust set is unusable (never expected past CI).
    Trust(String),
    /// A signature does not cover its bytes under any trusted key, or names
    /// another file.
    Rejected { object: String, reason: String },
    /// The feed's bytes verified but are not the document the contract
    /// describes.
    FeedMalformed(String),
    /// The feed's `version` is not the release it was fetched from.
    FeedTag { tag: String, version: String },
    /// This host's triple has no entry.
    TargetMissing(String),
    /// A feed entry's URL is not the asset on the resolved release.
    FeedUrl { target: String, url: String },
    /// The installed executable carries no version, so "is the feed newer"
    /// cannot be answered.
    InstalledVersionUnknown { path: PathBuf, reason: String },
    /// The installed executable names a version that is not a CalVer (a
    /// `MARKETING_VERSION=dev` pin, say); not comparable, and not the same
    /// state as "older".
    InstalledVersionNotCalVer { path: PathBuf, version: String },
    /// The installed bytes are the feed's, but the service is not serving
    /// them — an earlier run interrupted between the swap and the restart,
    /// or a restart that never happened. Nothing was changed; the remedy is
    /// a restart, not a download.
    AlreadyCurrentNotServing {
        version: String,
        reason: String,
        inspect: String,
    },
    /// The feed is not newer than what is installed: a replay, or an
    /// operator pointing an install at an older release (which is
    /// `install.sh`'s pinned form, not this command's).
    NotNewer { installed: String, feed: String },
    /// The downloaded bytes verified under a trusted key, and hash to
    /// something other than the entry says — a valid signature over the
    /// wrong binary.
    HashMismatch { expected: String, actual: String },
    /// The staged candidate could not name the expected version.
    Candidate { reason: String },
    /// A filesystem step failed before the live path changed.
    Io { what: String, reason: String },
    /// The update reached the live path and failed afterwards; the previous
    /// executable was restored and verified.
    UpdateFailedRecovered {
        failure: String,
        restored: String,
        inspect: String,
    },
    /// The update reached the live path, failed afterwards, and recovery
    /// failed too. Both are named; neither is a success. `live` says what
    /// is at the live path now — the candidate, when the restore's own
    /// rename failed, or the previous binary, when its restart or health
    /// check did — because the operator's next move depends on which.
    UpdateFailedRecoveryFailed {
        failure: String,
        recovery: String,
        live: String,
        inspect: String,
    },
    /// `rollback` with nothing to roll back to.
    NoPrevious(PathBuf),
    /// `rollback` swapped the binaries and the service did not come back.
    RollbackUnhealthy { reason: String, inspect: String },
    /// `rollback` put the previous binary live but could not move the
    /// displaced one into `.prev`: three files, named, so nothing is guessed.
    RollbackHalfDone {
        live: PathBuf,
        prev: PathBuf,
        displaced: PathBuf,
        reason: String,
    },
}

impl UpdateError {
    /// The process exit code this outcome maps to. Distinct codes because
    /// #394's scheduled job needs to tell them apart: `1` failed and nothing
    /// changed (a refusal, a network error, a rejected signature); `3`
    /// failed **and** the previous binary could not be put back — the
    /// service may be down; `4` no applicable release — the feed is not
    /// newer than what is installed, which a from-source host running ahead
    /// of the last tag reports every day and nobody should be paged for;
    /// `5` failed and the previous binary is back and serving — a human
    /// should look at why, but nothing is broken; `75` busy, try later.
    #[must_use]
    pub fn exit_code(&self) -> i32 {
        match self {
            UpdateError::Busy { .. } => 75,
            UpdateError::UpdateFailedRecoveryFailed { .. } => 3,
            UpdateError::NotNewer { .. } => 4,
            UpdateError::UpdateFailedRecovered { .. } => 5,
            _ => 1,
        }
    }
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::UnsupportedPlatform { os, arch } => write!(
                f,
                "no published agent for {os}/{arch}; the four targets are {}",
                TARGETS.join(", ")
            ),
            UpdateError::Install(m) => write!(f, "{m}"),
            UpdateError::Busy { lock, holder } => write!(
                f,
                "another solador-agent update or rollback holds {}{} — nothing was changed; \
                 let it finish and try again",
                lock.display(),
                holder
                    .as_deref()
                    .map(|h| format!(" ({h}, unix seconds)"))
                    .unwrap_or_default()
            ),
            UpdateError::Privileged { euid } => write!(
                f,
                "running as root (euid {euid}); the agent is a user-owned install with a user \
                 service, and root would target root's service manager and leave root-owned \
                 files beside the user's binary. Run this as the user the service runs as, \
                 without sudo"
            ),
            UpdateError::IsTheService { pid } => write!(
                f,
                "this process (pid {pid}) IS the metrics service; restarting the service \
                 would kill the updater mid-transaction. Run `solador-agent update` from a \
                 shell or from the separate maintenance job, never as the service's ExecStart"
            ),
            UpdateError::Network { what, reason } => write!(f, "{what}: {reason}"),
            UpdateError::Discovery(m) => write!(f, "{m}"),
            UpdateError::Trust(m) => write!(
                f,
                "the compiled-in trust set is unusable ({m}); this build cannot verify anything \
                 and refuses to update"
            ),
            UpdateError::Rejected { object, reason } => write!(
                f,
                "SIGNATURE VERIFICATION FAILED for {object}: {reason}. Nothing was executed, \
                 installed, or restarted. A tampered or corrupted download, a key this build \
                 does not trust, or a mislabelled asset all land here; do not work around it"
            ),
            UpdateError::FeedMalformed(m) => {
                write!(
                    f,
                    "agent-latest.json verified but is not an agent feed: {m}"
                )
            }
            UpdateError::FeedTag { tag, version } => write!(
                f,
                "agent-latest.json on release {tag} says version {version}; refusing a feed \
                 that does not name the release it was fetched from"
            ),
            UpdateError::TargetMissing(t) => {
                write!(f, "the feed has no entry for this host's target {t}")
            }
            UpdateError::FeedUrl { target, url } => write!(
                f,
                "the {target} entry's url '{url}' is not the asset on the resolved release; \
                 refusing to download from anywhere the release base did not name"
            ),
            UpdateError::InstalledVersionUnknown { path, reason } => write!(
                f,
                "{} could not name its version ({reason}), so whether the feed is newer \
                 cannot be answered. A build from a shallow checkout carries none; re-run \
                 agent/deploy/install.sh to move this host onto a published release",
                path.display()
            ),
            UpdateError::InstalledVersionNotCalVer { path, version } => write!(
                f,
                "{} reports version '{version}', which is not the YYYY.M.N CalVer a release \
                 carries, so it cannot be compared with the feed. Re-run \
                 agent/deploy/install.sh to move this host onto a published release",
                path.display()
            ),
            UpdateError::AlreadyCurrentNotServing {
                version,
                reason,
                inspect,
            } => write!(
                f,
                "the installed binary is already the latest release's ({version}), but the \
                 running service is not serving it: {reason}. Nothing was downloaded or \
                 changed; restart the service (or run `solador-agent rollback` if the \
                 previous binary is wanted back) and check it.\n{inspect}"
            ),
            UpdateError::NotNewer { installed, feed } => write!(
                f,
                "the latest published agent is {feed} and this host runs {installed}; not \
                 newer, so nothing to do. A deliberate downgrade is agent/deploy/install.sh's \
                 pinned-release form (agent/README.md, 'Install'), never this command's"
            ),
            UpdateError::HashMismatch { expected, actual } => write!(
                f,
                "the downloaded binary verifies under a trusted key but hashes to {actual}, \
                 not the {expected} its feed entry names — a valid signature over the wrong \
                 binary. Nothing was installed"
            ),
            UpdateError::Candidate { reason } => write!(
                f,
                "the verified candidate was rejected before installation: {reason}. It was \
                 removed; the live binary and .prev are untouched"
            ),
            UpdateError::Io { what, reason } => write!(f, "{what}: {reason}"),
            UpdateError::UpdateFailedRecovered {
                failure,
                restored,
                inspect,
            } => write!(
                f,
                "UPDATE FAILED: {failure}. The previous binary was restored and the service \
                 is back on it ({restored}). This is a failed update, not a success; the new \
                 release is not running here.\n{inspect}"
            ),
            UpdateError::UpdateFailedRecoveryFailed {
                failure,
                recovery,
                live,
                inspect,
            } => write!(
                f,
                "UPDATE FAILED: {failure}. RECOVERY ALSO FAILED: {recovery}. At the live \
                 path now: {live}. The service may be down; inspect it now.\n{inspect}"
            ),
            UpdateError::NoPrevious(p) => write!(
                f,
                "no previous binary at {} — nothing to roll back to; the live binary was \
                 not touched",
                p.display()
            ),
            UpdateError::RollbackUnhealthy { reason, inspect } => write!(
                f,
                "rollback swapped the binaries but the service did not come back: {reason}. \
                 Running `rollback` again swaps them back.\n{inspect}"
            ),
            UpdateError::RollbackHalfDone {
                live,
                prev,
                displaced,
                reason,
            } => write!(
                f,
                "rollback put the previous binary live at {} but could not move the displaced \
                 one into place as {} ({reason}); it is at {}. The service was NOT restarted. \
                 Move that file to {} by hand, then run `solador-agent rollback` (to return \
                 to it) or restart the service (to stay on what is live)",
                live.display(),
                prev.display(),
                displaced.display(),
                prev.display()
            ),
        }
    }
}

impl std::error::Error for UpdateError {}

// ---------------------------------------------------------------------------
// Trust set
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct TrustedKey {
    /// The id `minisign -V` prints, read out of the key bytes.
    id: String,
    key: PublicKey,
}

/// The public keys this build accepts signatures from: one or two, never
/// zero, never the same key twice.
#[derive(Debug)]
pub struct Trust {
    keys: Vec<TrustedKey>,
}

impl Trust {
    /// The keys `build.rs` compiled in from `agent/release-signing-key*.pub`.
    pub fn compiled_in() -> Result<Self, UpdateError> {
        Self::from_texts(TRUSTED_PUBLIC_KEYS)
    }

    /// Build a trust set from public key file texts (two lines, the first an
    /// `untrusted comment:`).
    pub fn from_texts(texts: &[&str]) -> Result<Self, UpdateError> {
        if texts.is_empty() {
            return Err(UpdateError::Trust("no public keys".to_string()));
        }
        let mut keys: Vec<TrustedKey> = Vec::with_capacity(texts.len());
        for text in texts {
            let key = PublicKey::decode(text)
                .map_err(|e| UpdateError::Trust(format!("not a minisign public key: {e}")))?;
            let id = key_id(text)?;
            if keys.iter().any(|k| k.id == id) {
                return Err(UpdateError::Trust(format!(
                    "key {id} is listed twice; one key twice is one key, not a rotation window"
                )));
            }
            keys.push(TrustedKey { id, key });
        }
        Ok(Trust { keys })
    }

    /// The ids of the trusted keys, in the order they were compiled in.
    #[must_use]
    pub fn key_ids(&self) -> Vec<String> {
        self.keys.iter().map(|k| k.id.clone()).collect()
    }

    /// Verify `signature` (a plain minisign signature text) over `payload`
    /// as a signature made for the file named `object`, under any trusted
    /// key. Returns the id of the key that verified.
    ///
    /// `Ok` is the only success. The trusted comment is covered by the global
    /// signature, so once the bytes verify it is the signer's own statement
    /// of which file this is — and a signature that verifies over these bytes
    /// but names another asset is a mislabelled artifact, refused.
    pub fn verify(
        &self,
        object: &str,
        signature: &str,
        payload: &[u8],
    ) -> Result<String, UpdateError> {
        let sig = Signature::decode(signature).map_err(|e| UpdateError::Rejected {
            object: object.to_string(),
            reason: format!("not a minisign signature ({e})"),
        })?;
        for k in &self.keys {
            // `false`: prehashed signatures only. The pinned `rsign2` and the
            // reference `minisign` both prehash by default; the legacy
            // algorithm is refused rather than tolerated.
            match k.key.verify(payload, &sig, false) {
                Ok(()) => {
                    if sig.trusted_comment() != object {
                        return Err(UpdateError::Rejected {
                            object: object.to_string(),
                            reason: format!(
                                "the signature verifies but was made for '{}'",
                                sig.trusted_comment()
                            ),
                        });
                    }
                    return Ok(k.id.clone());
                }
                // Not this key; the signature names another key id.
                Err(minisign_verify::Error::UnexpectedKeyId) => continue,
                Err(e) => {
                    return Err(UpdateError::Rejected {
                        object: object.to_string(),
                        reason: format!("does not verify under trusted key {} ({e})", k.id),
                    })
                }
            }
        }
        Err(UpdateError::Rejected {
            object: object.to_string(),
            reason: format!(
                "signed by a key this build does not trust (trusted: {})",
                self.key_ids().join(", ")
            ),
        })
    }
}

/// The key id minisign prints for a public key — eight bytes of the key
/// itself, little-endian, uppercase hex — read out of the base64 line, never
/// out of the comment above it.
pub fn key_id(pubkey_text: &str) -> Result<String, UpdateError> {
    use base64::Engine as _;
    let line = pubkey_text
        .lines()
        .nth(1)
        .ok_or_else(|| UpdateError::Trust("public key has no key line".to_string()))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(line.trim())
        .map_err(|e| UpdateError::Trust(format!("public key line is not base64: {e}")))?;
    if raw.len() != 42 {
        return Err(UpdateError::Trust(format!(
            "public key is {} bytes, expected 42",
            raw.len()
        )));
    }
    let mut id = raw[2..10].to_vec();
    id.reverse();
    Ok(hex::encode_upper(id))
}

// ---------------------------------------------------------------------------
// Versions and targets
// ---------------------------------------------------------------------------

/// A `YYYY.M.P` CalVer, parsed strictly: three dotted integers, a four-digit
/// year, no leading zeroes (`scripts/get-version-info.sh`'s shape, the same
/// rule the producer applies).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalVer(pub u32, pub u32, pub u32);

impl CalVer {
    /// `None` for anything that is not the shape above — including the
    /// empty string, `v`-prefixed tags, and the agent's wire-contract semver.
    #[must_use]
    pub fn parse(v: &str) -> Option<Self> {
        let fields: Vec<&str> = v.split('.').collect();
        if fields.len() != 3 || fields[0].len() != 4 {
            return None;
        }
        let mut nums = [0u32; 3];
        for (i, f) in fields.iter().enumerate() {
            if f.is_empty()
                || !f.bytes().all(|b| b.is_ascii_digit())
                || (f.len() > 1 && f.starts_with('0'))
            {
                return None;
            }
            nums[i] = f.parse().ok()?;
        }
        Some(CalVer(nums[0], nums[1], nums[2]))
    }
}

/// The published triple for a host, from `std::env::consts`-shaped inputs.
/// A Linux source build whose own target was gnu maps onto the musl artifact
/// deliberately: that is the published replacement for it.
pub fn target_for(os: &str, arch: &str) -> Result<&'static str, UpdateError> {
    let triple = match (os, arch) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        _ => {
            return Err(UpdateError::UnsupportedPlatform {
                os: os.to_string(),
                arch: arch.to_string(),
            })
        }
    };
    Ok(triple)
}

/// This host's published triple.
pub fn host_target() -> Result<&'static str, UpdateError> {
    target_for(std::env::consts::OS, std::env::consts::ARCH)
}

/// `solador-agent-<version>-<triple>`.
#[must_use]
pub fn asset_name(version: &str, target: &str) -> String {
    format!("{BINARY_PREFIX}-{version}-{target}")
}

/// SHA-256 of `bytes`, 64 lowercase hex characters — the feed's spelling.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

// ---------------------------------------------------------------------------
// The feed, consumer side
// ---------------------------------------------------------------------------

/// One feed entry. **No `deny_unknown_fields`**: the producer may add keys
/// (a rotation-window key id is the obvious one) and a consumer that refuses
/// them cannot update past the release that adds them — the choice
/// `docs/AGENT-DISTRIBUTION.md` §2 leaves to this side.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Target {
    pub url: String,
    pub signature: String,
    pub sha256: String,
}

/// A verified, decoded `agent-latest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Feed {
    pub version: String,
    pub targets: BTreeMap<String, Target>,
}

impl Feed {
    /// Decode bytes that have **already been verified** and hold them to the
    /// contract's shape: a CalVer version, a `targets` map whose entries each
    /// carry a signature that parses and a 64-hex hash. Extra JSON keys are
    /// ignored; extra triples are ignored (this host reads its own).
    pub fn parse(bytes: &[u8]) -> Result<Self, UpdateError> {
        let feed: Feed =
            serde_json::from_slice(bytes).map_err(|e| UpdateError::FeedMalformed(e.to_string()))?;
        if CalVer::parse(&feed.version).is_none() {
            return Err(UpdateError::FeedMalformed(format!(
                "version '{}' is not a YYYY.M.P CalVer",
                feed.version
            )));
        }
        if feed.targets.is_empty() {
            return Err(UpdateError::FeedMalformed("no targets".to_string()));
        }
        for (target, entry) in &feed.targets {
            if !is_sha256_hex(&entry.sha256) {
                return Err(UpdateError::FeedMalformed(format!(
                    "the {target} entry's sha256 '{}' is not 64 lowercase hex characters",
                    entry.sha256
                )));
            }
            Signature::decode(&entry.signature).map_err(|e| {
                UpdateError::FeedMalformed(format!(
                    "the {target} entry's signature is not a minisign signature ({e})"
                ))
            })?;
        }
        Ok(feed)
    }
}

/// The consumer's check, in the consumer's order: the **exact served bytes**
/// against the detached signature under the trust set, then decode.
pub fn verify_feed(
    trust: &Trust,
    feed_bytes: &[u8],
    feed_signature: &str,
) -> Result<(Feed, String), UpdateError> {
    let key = trust.verify(FEED_ASSET, feed_signature, feed_bytes)?;
    let feed = Feed::parse(feed_bytes)?;
    Ok((feed, key))
}

/// The entry for `target` on a feed fetched from release `tag` off `base`,
/// with its URL held to exactly the asset this updater would construct
/// itself. A feed cannot send the download anywhere else.
pub fn select_target<'f>(
    feed: &'f Feed,
    target: &str,
    base: &str,
    tag: &str,
) -> Result<&'f Target, UpdateError> {
    let entry = feed
        .targets
        .get(target)
        .ok_or_else(|| UpdateError::TargetMissing(target.to_string()))?;
    let expected = format!(
        "{base}/releases/download/{tag}/{}",
        asset_name(&feed.version, target)
    );
    if entry.url != expected {
        return Err(UpdateError::FeedUrl {
            target: target.to_string(),
            url: entry.url.clone(),
        });
    }
    Ok(entry)
}

/// Is `tag` the release `feed` names? `v<version>` is the repo's scheme.
pub fn check_feed_tag(feed: &Feed, tag: &str) -> Result<(), UpdateError> {
    if tag != format!("v{}", feed.version) {
        return Err(UpdateError::FeedTag {
            tag: tag.to_string(),
            version: feed.version.clone(),
        });
    }
    Ok(())
}

/// Verify a downloaded binary against its feed entry: signature under a
/// trusted key **and** hash equality. Both, because they answer different
/// questions — the signature says *we* published these bytes, the hash says
/// they are the bytes *this feed entry* is about.
pub fn verify_binary(
    trust: &Trust,
    asset: &str,
    entry: &Target,
    bytes: &[u8],
) -> Result<String, UpdateError> {
    let key = trust.verify(asset, &entry.signature, bytes)?;
    let actual = sha256_hex(bytes);
    if actual != entry.sha256 {
        return Err(UpdateError::HashMismatch {
            expected: entry.sha256.clone(),
            actual,
        });
    }
    Ok(key)
}

/// A release tag: `v` + CalVer, and nothing else (not a branch, not the
/// `releases` landing page a redirect hands back when a repo has none).
#[must_use]
pub fn is_release_tag(tag: &str) -> bool {
    tag.strip_prefix('v')
        .map(|v| CalVer::parse(v).is_some())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// The installed service (#392's contract)
// ---------------------------------------------------------------------------

/// How the metrics service is restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Service {
    /// `systemctl --user restart <unit>`.
    Systemd { unit: String },
    /// `launchctl kickstart -k gui/<uid>/<label>`.
    Launchd { label: String, uid: u32 },
}

/// Restart and inspect the metrics service. A trait so the transaction can
/// be driven in tests against a fake manager; [`Service`] is the real one.
pub trait ServiceControl {
    /// One line naming the service, for messages.
    fn describe(&self) -> String;
    /// Can this process reach the service manager at all? Asked BEFORE any
    /// download or swap: a `sudo -u` shell or a cron job with no
    /// `XDG_RUNTIME_DIR` has no user manager, and finding that out at the
    /// restart — after the live path changed — is the half-applied update
    /// the whole design exists to prevent. `Err` says what to do.
    fn preflight(&self) -> Result<(), String>;
    /// Restart the service. `Err` carries what the manager said.
    fn restart(&self) -> Result<(), String>;
    /// The service's main process id, when the manager reports one.
    fn main_pid(&self) -> Option<u32>;
    /// Where to look when this service is not doing what it should: the
    /// manager's status command and the log. Appended to every failure that
    /// leaves the operator with a service to inspect.
    fn inspect_hint(&self, log: Option<&Path>) -> String;
}

impl ServiceControl for Service {
    fn describe(&self) -> String {
        match self {
            Service::Systemd { unit } => format!("systemd user unit {unit}"),
            Service::Launchd { label, uid } => format!("LaunchAgent gui/{uid}/{label}"),
        }
    }

    fn preflight(&self) -> Result<(), String> {
        let (program, args, remedy): (&str, Vec<String>, &str) = match self {
            // The same probe install.sh's preflight uses.
            Service::Systemd { .. } => (
                "systemctl",
                vec!["--user".into(), "show-environment".into()],
                "cannot reach this user's systemd manager (systemctl --user). Run this from \
                 a real login session for this user (not via sudo -u or su), or set \
                 XDG_RUNTIME_DIR=/run/user/$(id -u)",
            ),
            Service::Launchd { uid, .. } => (
                "launchctl",
                vec!["print".into(), format!("gui/{uid}")],
                "no launchd gui domain for this user — no login session, so there is no \
                 LaunchAgent to restart. Log in at the console (or via Screen Sharing) as \
                 this user",
            ),
        };
        let ok = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            Err(format!("{remedy} (`{program} {}` failed)", args.join(" ")))
        }
    }

    fn restart(&self) -> Result<(), String> {
        let (program, args): (&str, Vec<String>) = match self {
            Service::Systemd { unit } => (
                "systemctl",
                vec!["--user".into(), "restart".into(), unit.clone()],
            ),
            Service::Launchd { label, uid } => (
                "launchctl",
                vec![
                    "kickstart".into(),
                    "-k".into(),
                    format!("gui/{uid}/{label}"),
                ],
            ),
        };
        let out = Command::new(program)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {program}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{program} {} exited {}: {}",
                args.join(" "),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    fn main_pid(&self) -> Option<u32> {
        match self {
            Service::Systemd { unit } => {
                let out = Command::new("systemctl")
                    .args(["--user", "show", "-p", "MainPID", "--value", unit])
                    .stdin(Stdio::null())
                    .output()
                    .ok()?;
                let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
                (pid != 0).then_some(pid)
            }
            Service::Launchd { label, uid } => {
                let out = Command::new("launchctl")
                    .args(["print", &format!("gui/{uid}/{label}")])
                    .stdin(Stdio::null())
                    .output()
                    .ok()?;
                if !out.status.success() {
                    return None;
                }
                launchctl_pid(&String::from_utf8_lossy(&out.stdout))
            }
        }
    }

    fn inspect_hint(&self, log: Option<&Path>) -> String {
        match self {
            Service::Systemd { unit } => format!(
                "Inspect:  systemctl --user status {unit}\n          journalctl --user -u {unit} -n 50 --no-pager"
            ),
            Service::Launchd { label, uid } => format!(
                "Inspect:  launchctl print gui/{uid}/{label}\n          tail -n 50 \"{}\"",
                log.map(|p| p.display().to_string())
                    .unwrap_or_else(|| "~/Library/Logs/solador-agent.log".to_string())
            ),
        }
    }
}

/// The `pid = N` line of `launchctl print`, if the service is running.
fn launchctl_pid(print_output: &str) -> Option<u32> {
    print_output.lines().find_map(|line| {
        let line = line.trim();
        line.strip_prefix("pid = ")
            .and_then(|rest| rest.trim().parse::<u32>().ok())
    })
}

/// The resolved install: the executable the service starts, the env file it
/// reads, and the service itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub binary: PathBuf,
    pub env_file: PathBuf,
    pub service: Service,
    /// Where the service's output lands, when the install names it: the
    /// LaunchAgent's `ProgramArguments[3]`. systemd's journal has no path.
    pub log: Option<PathBuf>,
}

impl Install {
    /// `<bin>.new`, `<bin>.prev`, `<bin>.update.lock`: the siblings the
    /// transaction uses, all beside the live path so the final rename is
    /// within one directory.
    #[must_use]
    pub fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self
            .binary
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(suffix);
        self.binary.with_file_name(name)
    }
}

/// The `ExecStart=` executable of a rendered `solador-agent.service`: the
/// whole value when it is bare, the quoted part when the installer quoted it
/// (a HOME with a space). systemd's `-`/`@`/`+`/`!`/`:` prefixes are not
/// something the template writes; a line carrying one is not ours.
pub fn systemd_exec_start(unit_text: &str) -> Option<PathBuf> {
    let value = unit_text
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))?;
    let value = value.trim();
    let path = if let Some(rest) = value.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        value.split_whitespace().next()?
    };
    if path.is_empty() || !path.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(path))
}

/// The `EnvironmentFile=` of a rendered unit, `%h` resolved to `home`.
pub fn systemd_environment_file(unit_text: &str, home: &Path) -> Option<PathBuf> {
    let value = unit_text
        .lines()
        .find_map(|l| l.strip_prefix("EnvironmentFile="))?
        .trim();
    let value = value.strip_prefix('-').unwrap_or(value);
    if value.is_empty() {
        return None;
    }
    let resolved = value.replace("%h", &home.to_string_lossy());
    if !resolved.starts_with('/') {
        return None;
    }
    Some(PathBuf::from(resolved))
}

/// The binary, env file and (when present) log file out of a LaunchAgent
/// plist rendered from `app.solador.agent.plist`, given as JSON
/// (`plutil -convert json`): `ProgramArguments` is `[launcher, binary, env
/// file, log file]`. The log is optional in the return because a plist a
/// human trimmed still names a service worth updating; it is only ever
/// used to say where to look.
pub fn launchd_program_arguments(
    plist_json: &str,
) -> Result<(PathBuf, PathBuf, Option<PathBuf>), String> {
    let v: serde_json::Value =
        serde_json::from_str(plist_json).map_err(|e| format!("plist is not JSON: {e}"))?;
    let args = v
        .get("ProgramArguments")
        .and_then(|a| a.as_array())
        .ok_or_else(|| "plist has no ProgramArguments array".to_string())?;
    let arg = |i: usize, what: &str| -> Result<PathBuf, String> {
        let s = args
            .get(i)
            .and_then(|s| s.as_str())
            .ok_or_else(|| format!("ProgramArguments[{i}] ({what}) is missing"))?;
        if !s.starts_with('/') {
            return Err(format!(
                "ProgramArguments[{i}] ({what}) '{s}' is not absolute"
            ));
        }
        Ok(PathBuf::from(s))
    };
    let log = args
        .get(3)
        .and_then(|s| s.as_str())
        .filter(|s| s.starts_with('/'))
        .map(PathBuf::from);
    Ok((arg(1, "the agent binary")?, arg(2, "the env file")?, log))
}

/// The installer's own rule for a launchd label override: it becomes a file
/// name and a plist value, so it is letters, digits, `.`, `_` and `-`, and
/// starts with a letter or digit.
pub fn valid_launchd_label(label: &str) -> bool {
    let mut chars = label.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphanumeric())
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Resolve #392's install on this host: the unit or plist under `home`,
/// the executable it starts, the env file it reads. Refuses, actionably and
/// before any change, an install this user cannot replace.
pub fn resolve_install(home: &Path, launchd_label: &str) -> Result<Install, UpdateError> {
    let install = match std::env::consts::OS {
        "linux" => resolve_systemd(home)?,
        "macos" => resolve_launchd(home, launchd_label)?,
        other => {
            return Err(UpdateError::UnsupportedPlatform {
                os: other.to_string(),
                arch: std::env::consts::ARCH.to_string(),
            })
        }
    };
    check_install_replaceable(&install)?;
    Ok(install)
}

fn resolve_systemd(home: &Path) -> Result<Install, UpdateError> {
    let unit_path = home
        .join(".config/systemd/user")
        .join(format!("{SYSTEMD_UNIT}.service"));
    let text = fs::read_to_string(&unit_path).map_err(|e| {
        UpdateError::Install(format!(
            "no installed agent: {} is unreadable ({e}). Install with agent/deploy/install.sh first",
            unit_path.display()
        ))
    })?;
    let binary = systemd_exec_start(&text).ok_or_else(|| {
        UpdateError::Install(format!(
            "{} has no usable ExecStart= line; re-run agent/deploy/install.sh to regenerate it",
            unit_path.display()
        ))
    })?;
    let env_file = systemd_environment_file(&text, home).ok_or_else(|| {
        UpdateError::Install(format!(
            "{} has no usable EnvironmentFile= line; re-run agent/deploy/install.sh to regenerate it",
            unit_path.display()
        ))
    })?;
    Ok(Install {
        binary,
        env_file,
        service: Service::Systemd {
            unit: SYSTEMD_UNIT.to_string(),
        },
        log: None,
    })
}

fn resolve_launchd(home: &Path, label: &str) -> Result<Install, UpdateError> {
    if !valid_launchd_label(label) {
        return Err(UpdateError::Install(format!(
            "launchd label '{label}' may contain only letters, digits, '.', '_' and '-', and \
             must start with a letter or digit"
        )));
    }
    let plist = home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist"));
    if !plist.is_file() {
        return Err(UpdateError::Install(format!(
            "no installed agent: {} not found. Install with agent/deploy/install.sh first",
            plist.display()
        )));
    }
    // plutil, on every macOS: the plist is XML the installer rendered, and
    // reading it back through the system's own parser is what keeps this
    // from growing a plist parser of its own.
    let out = Command::new("plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(&plist)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| UpdateError::Install(format!("could not run plutil: {e}")))?;
    if !out.status.success() {
        return Err(UpdateError::Install(format!(
            "plutil could not read {}: {}",
            plist.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let (binary, env_file, log) = launchd_program_arguments(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| {
            UpdateError::Install(format!(
                "{} is not the LaunchAgent install.sh renders ({e}); re-run agent/deploy/install.sh",
                plist.display()
            ))
        })?;
    Ok(Install {
        binary,
        env_file,
        service: Service::Launchd {
            label: label.to_string(),
            uid: current_uid(),
        },
        log,
    })
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid has no preconditions and cannot fail.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// Can this user stage beside and rename over the installed executable? The
/// pre-#392 `/opt` layout is root-owned, and the answer there is the
/// installer's explicit migration — never `sudo` from here.
pub fn check_install_replaceable(install: &Install) -> Result<(), UpdateError> {
    if !install.binary.is_file() {
        return Err(UpdateError::Install(format!(
            "the service starts {} but there is no such file; re-run agent/deploy/install.sh",
            install.binary.display()
        )));
    }
    let dir = install.binary.parent().ok_or_else(|| {
        UpdateError::Install(format!(
            "{} has no parent directory",
            install.binary.display()
        ))
    })?;
    if !dir_writable(dir) {
        return Err(UpdateError::Install(format!(
            "{} is not writable by this user, so the agent at {} cannot be replaced without \
             a privilege this command never takes. If this is the pre-#392 /opt layout, \
             migrate it explicitly first:  agent/deploy/install.sh --migrate-from-opt",
            dir.display(),
            install.binary.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn dir_writable(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a valid NUL-terminated path; access() reads it and nothing else.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
fn dir_writable(dir: &Path) -> bool {
    !fs::metadata(dir)
        .map(|m| m.permissions().readonly())
        .unwrap_or(true)
}

// ---------------------------------------------------------------------------
// The serving configuration (the env file)
// ---------------------------------------------------------------------------

/// What the metrics service was started with: the token the probe needs,
/// the bind and port it listens on. `Debug` redacts the token; nothing else
/// prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Serving {
    token: String,
    pub bind: String,
    pub port: u16,
}

impl fmt::Debug for Serving {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Serving")
            .field("token", &"<redacted>")
            .field("bind", &self.bind)
            .field("port", &self.port)
            .finish()
    }
}

impl Serving {
    /// The token, for the one place it goes: the `Authorization` header of
    /// the health probe.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The URL the health probe dials — `lib.sh`'s `health_url`: a wildcard
    /// bind is not an address you can dial, so loopback; an IPv6 literal is
    /// bracketed.
    #[must_use]
    pub fn health_url(&self) -> String {
        health_url(&self.bind, self.port)
    }
}

/// Parse an env file the way systemd's `EnvironmentFile=` and the launcher
/// read it: `KEY=value` lines, last occurrence wins, trailing CR dropped,
/// surrounding whitespace trimmed, one matching pair of quotes removed. Never
/// evaluated as shell — the token is user-controlled text.
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let mut value = value.trim();
        if value.len() >= 2
            && ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
        {
            value = &value[1..value.len() - 1];
        }
        map.insert(key.to_string(), value.to_string());
    }
    map
}

/// Read the serving configuration out of the env file. The token and the
/// bind are required (see below for why the bind is); the port mirrors the
/// agent's own reading of it — `SOLADOR_AGENT_PORT` that does not parse is
/// 7878 to the agent, so it is 7878 to the probe.
pub fn read_serving(env_file: &Path) -> Result<Serving, UpdateError> {
    let text = fs::read_to_string(env_file).map_err(|e| {
        UpdateError::Install(format!(
            "cannot read the env file {} ({e}); the health check needs the token, bind and \
             port the service was started with",
            env_file.display()
        ))
    })?;
    let map = parse_env_file(&text);
    let token = map
        .get("SOLADOR_AGENT_TOKEN")
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| {
            UpdateError::Install(format!(
                "{} has no SOLADOR_AGENT_TOKEN; cannot verify the service after a restart",
                env_file.display()
            ))
        })?;
    // Required, not defaulted: with no bind the agent detects a tailnet
    // address at start that this command cannot dial, and a probe of
    // loopback would then swap, fail health, restore and exit 3 on a
    // perfectly healthy host. install.sh always writes the key; a hand-made
    // env file without it is refused, naming the fix.
    let bind = map
        .get("SOLADOR_AGENT_BIND")
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty())
        .ok_or_else(|| {
            UpdateError::Install(format!(
                "{} names no SOLADOR_AGENT_BIND; the service binds a detected tailnet address \
                 this command cannot dial. Add SOLADOR_AGENT_BIND=<the address the agent \
                 listens on> to the env file (install.sh always writes it)",
                env_file.display()
            ))
        })?;
    let port = map
        .get("SOLADOR_AGENT_PORT")
        .and_then(|p| p.trim().parse().ok())
        .unwrap_or(7878);
    Ok(Serving { token, bind, port })
}

/// `lib.sh`'s `health_url`, verbatim.
#[must_use]
pub fn health_url(bind: &str, port: u16) -> String {
    let host = match bind {
        "" | "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "[::1]".to_string(),
        b if b.contains(':') && !b.starts_with('[') => format!("[{b}]"),
        b => b.to_string(),
    };
    format!("http://{host}:{port}/v1/health")
}

// ---------------------------------------------------------------------------
// The transaction lock
// ---------------------------------------------------------------------------

/// The lock every `update` and `rollback` on one install takes. Held for
/// the process's lifetime — including the health poll and any recovery — and
/// released by the OS when the process ends, however it ends.
#[derive(Debug)]
pub struct TransactionLock {
    _file: File,
    pub path: PathBuf,
}

/// How long a lock whose note names THIS process is retried for before
/// it is reported busy (see [`TransactionLock::acquire`]).
const STALE_OWN_LOCK_RETRY_BUDGET: Duration = Duration::from_secs(2);

impl TransactionLock {
    /// Take the lock, or report busy without waiting for another process.
    /// The file is created if absent and never removed: a lock file that is
    /// unlinked while another process holds a lock on it is how two
    /// processes come to hold "the" lock at once.
    ///
    /// **One kind of busy is waited out**: a `flock` lives as long as *any*
    /// reference to its open file description does, and a child process
    /// forked by another thread while our own lock's descriptor was open
    /// inherits such a reference until its exec (or exit) — `CLOEXEC` closes
    /// at exec, not at fork. So a lock this process released a moment ago
    /// can still read as held, with our own pid in its note. That is not
    /// another transaction; it is our stale reference, and it is retried
    /// with a short bounded backoff — the same shape, for the same reason,
    /// as the `ETXTBSY` retry in [`spawn_version_probe`] that `cargo` and
    /// `rustup` carry. A note naming any *other* pid, or none, is another
    /// process's transaction and stays an immediate busy: the
    /// cross-process guarantee is untouched.
    pub fn acquire(path: PathBuf) -> Result<Self, UpdateError> {
        let started = Instant::now();
        loop {
            match Self::try_acquire(&path)? {
                Ok(lock) => return Ok(lock),
                Err(holder) => {
                    let ours = holder
                        .as_deref()
                        .is_some_and(|h| h.starts_with(&format!("pid={} ", std::process::id())));
                    if ours && started.elapsed() < STALE_OWN_LOCK_RETRY_BUDGET {
                        std::thread::sleep(Duration::from_millis(25));
                        continue;
                    }
                    return Err(UpdateError::Busy { lock: path, holder });
                }
            }
        }
    }

    /// One attempt. The outer `Ok(Err(holder))` is "held by someone", with
    /// the note read out of the file.
    fn try_acquire(path: &Path) -> Result<Result<Self, Option<String>>, UpdateError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| UpdateError::Io {
                what: format!("cannot open the transaction lock {}", path.display()),
                reason: e.to_string(),
            })?;
        match file.try_lock() {
            Ok(()) => {
                // Who holds it and since when, for the *next* caller's
                // "busy" line. Best effort: a lock whose note could not be
                // written is still a lock.
                let mut file = file;
                let _ = file.set_len(0).and_then(|()| {
                    use std::io::Seek as _;
                    file.seek(std::io::SeekFrom::Start(0)).map(|_| ())
                });
                let _ = writeln!(file, "pid={} since={}", std::process::id(), unix_time_now());
                let _ = file.sync_all();
                Ok(Ok(TransactionLock {
                    _file: file,
                    path: path.to_path_buf(),
                }))
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let holder = fs::read_to_string(path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                Ok(Err(holder))
            }
            Err(std::fs::TryLockError::Error(e)) => Err(UpdateError::Io {
                what: format!("cannot lock {}", path.display()),
                reason: e.to_string(),
            }),
        }
    }
}

/// Seconds since the Unix epoch, for the lock's holder note.
fn unix_time_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Running a binary's --version
// ---------------------------------------------------------------------------

/// How long a freshly written executable is retried for on `ETXTBSY`
/// before that is reported as the reason (see [`spawn_version_probe`]).
const ETXTBSY_RETRY_BUDGET: Duration = Duration::from_secs(2);

/// Spawn `<path> --version`, retrying on **`ETXTBSY`** for a bounded while.
///
/// Linux refuses to exec a file that any process still holds open for
/// writing. Every write handle this crate takes on a candidate is flushed,
/// synced and dropped before the spawn (`stage_candidate`) — but a child
/// process *forked by another thread* during that write window inherits
/// the descriptor, and `O_CLOEXEC` closes it only at the child's own exec,
/// not at fork. So the first spawn of a just-written file can race such a
/// child and fail with `ETXTBSY` on Linux; macOS has no such check, which
/// is why the race never showed locally and did in CI. The metrics service
/// manager spawning the fake agent in the test suite is one such child;
/// in production, anything on the host that spawns while staging runs is.
/// `cargo` (`cargo_util::paths::open` / its `ETXTBSY` retry in
/// `cargo-util/src/process_builder.rs`) and `rustup` (`utils::raw::open_file`
/// retries around `exec`) carry the same bounded retry for the same reason.
/// A file that stays busy past the budget is reported as busy — never
/// waited on forever, never silently skipped.
fn spawn_version_probe(path: &Path) -> Result<std::process::Child, String> {
    let started = Instant::now();
    loop {
        match Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(e)
                if e.kind() == std::io::ErrorKind::ExecutableFileBusy
                    && started.elapsed() < ETXTBSY_RETRY_BUDGET =>
            {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                return Err(format!("could not execute {}: {e}", path.display()));
            }
        }
    }
}

/// Ask an executable its version — `--version` prints one line and nothing
/// else — with a bound. `Err` names why there is none: it could not be
/// started (`Exec format error`, a `noexec` mount, still busy after the
/// `ETXTBSY` retry budget), exited non-zero (a build from a shallow checkout
/// refuses), printed nothing, or did not answer in time. An unknown
/// version, with its reason, never a stand-in.
pub fn binary_version(path: &Path, timeout: Duration) -> Result<String, String> {
    let mut child = spawn_version_probe(path)?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`--version` did not answer within {}s",
                    timeout.as_secs()
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("waiting for `--version`: {e}"));
            }
        }
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut err);
    }
    if !status.success() {
        let said = err.lines().next().unwrap_or("").trim();
        return Err(format!(
            "`--version` exited {status}{}",
            if said.is_empty() {
                String::new()
            } else {
                format!(" ({said})")
            }
        ));
    }
    let first = out.lines().next().unwrap_or("").trim();
    if first.is_empty() {
        return Err("`--version` printed nothing".to_string());
    }
    Ok(first.to_string())
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Parsed, not split: `http://127.0.0.1:1@evil.example/` has a loopback
/// *userinfo* and an evil host, and a string split on `:` reads it as
/// loopback. Unreachable today (the base is a compiled-in constant) and a
/// hole for the first change that makes it configurable, so it is closed
/// now: scheme `http`, no userinfo, loopback host.
fn is_loopback_base(base: &str) -> bool {
    match reqwest::Url::parse(base) {
        Ok(url) => {
            url.scheme() == "http"
                && url.username().is_empty()
                && url.password().is_none()
                && url.host_str().is_some_and(is_loopback_host)
        }
        Err(_) => false,
    }
}

fn user_agent(running_version: Option<&str>) -> String {
    format!(
        "solador-agent-update/{}",
        running_version.unwrap_or("unversioned")
    )
}

/// Is a release base acceptable? `https://` always; plain `http://` only on
/// loopback, which is test infrastructure and nothing a shipped binary is
/// ever configured with (`main.rs` passes the `RELEASE_BASE` constant).
pub fn check_release_base(base: &str) -> Result<bool, UpdateError> {
    let loopback = is_loopback_base(base);
    if !loopback && !base.starts_with("https://") {
        return Err(UpdateError::Network {
            what: "release base".to_string(),
            reason: format!("'{base}' is not https:// (loopback http is for tests only)"),
        });
    }
    Ok(loopback)
}

/// May a redirect be followed? Onto `https` always; onto `http` only when
/// the base itself is loopback **and the redirect stays on a loopback
/// host** — a loopback test server is not a licence to fetch from anywhere
/// over plain http; and never past ten hops. A refused hop is an error,
/// never a silent stop with a partial body.
pub fn redirect_allowed(
    loopback: bool,
    scheme: &str,
    host: Option<&str>,
    hops: usize,
) -> Result<(), String> {
    if hops >= 10 {
        return Err("too many redirects".to_string());
    }
    if scheme == "https" {
        return Ok(());
    }
    if scheme == "http" && loopback && host.is_some_and(is_loopback_host) {
        return Ok(());
    }
    Err(format!(
        "refusing to follow a redirect to {scheme}://{}",
        host.unwrap_or("")
    ))
}

fn is_loopback_host(host: &str) -> bool {
    host == "127.0.0.1" || host == "localhost" || host == "[::1]" || host == "::1"
}

/// A client for the release base: HTTPS only, following redirects only onto
/// HTTPS — unless the base is loopback HTTP (see [`check_release_base`]).
/// The system proxy variables (`HTTPS_PROXY` and friends) are honoured for
/// this client, which is the one that leaves the host.
fn release_client(
    base: &str,
    running_version: Option<&str>,
) -> Result<reqwest::Client, UpdateError> {
    let loopback = check_release_base(base)?;
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let url = attempt.url().clone();
        match redirect_allowed(
            loopback,
            url.scheme(),
            url.host_str(),
            attempt.previous().len(),
        ) {
            Ok(()) => attempt.follow(),
            Err(why) => attempt.error(why),
        }
    });
    reqwest::Client::builder()
        .user_agent(user_agent(running_version))
        .redirect(policy)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| UpdateError::Network {
            what: "http client".to_string(),
            reason: e.to_string(),
        })
}

/// The client for the local health probe: plain HTTP, no redirects, short
/// per-attempt bounds (`lib.sh`: connect 2s, total 5s), and **no proxy** —
/// an `HTTP_PROXY` in the environment without a `NO_PROXY` for loopback
/// would route the probe of this host's own service through a proxy and
/// roll back every update.
fn health_client(running_version: Option<&str>) -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .user_agent(user_agent(running_version))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| UpdateError::Network {
            what: "http client".to_string(),
            reason: e.to_string(),
        })
}

/// Resolve the concrete release behind `/releases/latest`: the redirect's
/// `Location` must be `<base>/releases/tag/<vCalVer>`. Not followed — read.
pub async fn resolve_latest_tag(
    client: &reqwest::Client,
    base: &str,
) -> Result<String, UpdateError> {
    let url = format!("{base}/releases/latest");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| UpdateError::Network {
            what: format!("resolving {url}"),
            reason: error_chain(&e),
        })?;
    let status = resp.status();
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let Some(location) = location.filter(|_| status.is_redirection()) else {
        return Err(UpdateError::Discovery(format!(
            "{url} answered {status} with no redirect; expected a redirect to the latest \
             published release (a repository with no published release lands here)"
        )));
    };
    let prefix = format!("{base}/releases/tag/");
    let Some(tag) = location.strip_prefix(&prefix) else {
        return Err(UpdateError::Discovery(format!(
            "{url} redirected to '{location}', not to a release tag under {prefix}"
        )));
    };
    if !is_release_tag(tag) {
        return Err(UpdateError::Discovery(format!(
            "'{tag}' is not a release tag (expected vYYYY.M.N)"
        )));
    }
    Ok(tag.to_string())
}

/// GET one release asset into memory, refusing a body past `cap`.
pub async fn fetch_asset(
    client: &reqwest::Client,
    url: &str,
    cap: usize,
) -> Result<Vec<u8>, UpdateError> {
    let net = |reason: String| UpdateError::Network {
        what: format!("downloading {url}"),
        reason,
    };
    let mut resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| net(error_chain(&e)))?;
    if !resp.status().is_success() {
        return Err(net(format!(
            "HTTP {} (the release has no such asset, or it is not reachable)",
            resp.status()
        )));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > cap {
            return Err(net(format!("{len} bytes is past the {cap}-byte cap")));
        }
    }
    let mut buf = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| net(error_chain(&e)))? {
        if buf.len() + chunk.len() > cap {
            return Err(net(format!("body is past the {cap}-byte cap")));
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// What the health poll must see before it is satisfied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// `/v1/health` reports exactly this version. An absent `version` key
    /// never satisfies it.
    Version(String),
    /// `/v1/health` answers **without** a version: the expectation for a
    /// previous binary that could not name one. An answer that *does* carry
    /// a version is not that binary — it is the displaced process still
    /// holding the socket (the bootout race `lib.sh` documents) — and is
    /// held as a mismatch, not accepted as "online".
    Online,
}

/// Poll the authenticated `/v1/health` until `expect` is met. `Ok` carries
/// the version the endpoint reported (if any); `Err` carries what was
/// observed, never the token. `report` is told each time the observation
/// *changes* — fifteen identical lines say nothing a summary does not —
/// and the summary carries the elapsed time.
pub async fn wait_for_health(
    client: &reqwest::Client,
    serving: &Serving,
    expect: &Expect,
    attempts: u32,
    interval: Duration,
    report: &mut dyn FnMut(&str),
) -> Result<Option<String>, String> {
    let url = serving.health_url();
    let started = Instant::now();
    let mut last = String::new();
    for attempt in 1..=attempts {
        let resp = client.get(&url).bearer_auth(serving.token()).send().await;
        let observation = match resp {
            Ok(resp) if resp.status().is_success() => {
                let body = resp.text().await.unwrap_or_default();
                let reported = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.get("version")
                            .and_then(|s| s.as_str())
                            .map(str::to_string)
                    });
                match (expect, &reported) {
                    (Expect::Online, None) => return Ok(None),
                    (Expect::Online, Some(got)) => format!(
                        "{url} reports version {got}, but the restored binary carries none — \
                         that is the displaced process still answering"
                    ),
                    (Expect::Version(want), Some(got)) if got == want => return Ok(reported),
                    (Expect::Version(want), Some(got)) => {
                        format!("{url} reports version {got}, want {want}")
                    }
                    (Expect::Version(want), None) => {
                        format!("{url} answered but reports no version (want {want})")
                    }
                }
            }
            Ok(resp) => format!(
                "{url} answered HTTP {} (a 401 means the token in the env file is not the \
                 one the running agent holds)",
                resp.status()
            ),
            Err(e) => format!("{url}: {}", transport_summary(e)),
        };
        if observation != last {
            report(&format!(
                "    attempt {attempt}/{attempts} ({:.0}s): {observation}",
                started.elapsed().as_secs_f64()
            ));
            last = observation;
        }
        if attempt < attempts {
            tokio::time::sleep(interval).await;
        }
    }
    Err(format!(
        "not verified within {attempts} attempts over {:.0}s; last: {last}",
        started.elapsed().as_secs_f64()
    ))
}

/// A reqwest error with its causes: the top-level `Display` of a refused
/// redirect is "error sending request", and the refusal's own words — the
/// policy's "refusing to follow a redirect to http://" — sit one source
/// down. Release URLs carry no token, so the chain is safe to print.
fn error_chain(e: &reqwest::Error) -> String {
    let mut parts = vec![e.to_string()];
    let mut source = std::error::Error::source(e);
    while let Some(inner) = source {
        let text = inner.to_string();
        if !parts.iter().any(|p| p.contains(&text)) {
            parts.push(text);
        }
        source = inner.source();
    }
    parts.join(": ")
}

/// One line of meaning for a reqwest error, with no URL in it (the URL is
/// already in the message and never carries the token, but the habit is
/// the rule).
fn transport_summary(e: reqwest::Error) -> String {
    if e.is_connect() {
        "could not connect — nothing listening at that address/port, or no route to it".to_string()
    } else if e.is_timeout() {
        "timed out — the address may be unreachable (a tailnet bind with Tailscale down, say)"
            .to_string()
    } else {
        let s = e.without_url().to_string();
        if s.is_empty() {
            "request failed".to_string()
        } else {
            s
        }
    }
}

// ---------------------------------------------------------------------------
// The transaction
// ---------------------------------------------------------------------------

/// Everything `run_update` / `run_rollback` need, resolved by the caller
/// (main.rs for the real thing, the tests for a temporary install tree and
/// a loopback release).
pub struct Context<'a> {
    pub release_base: String,
    pub trust: Trust,
    pub install: Install,
    pub serving: Serving,
    pub service: &'a dyn ServiceControl,
    /// This host's published triple.
    pub target: &'static str,
    /// The CalVer of the binary running this command, for the User-Agent.
    pub running_version: Option<String>,
    pub health_attempts: u32,
    pub health_interval: Duration,
    /// Progress lines. Never handed the token.
    pub report: &'a mut dyn FnMut(&str),
}

/// A successful `update`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    /// The installed bytes are the feed's bytes. Nothing was downloaded,
    /// staged or restarted.
    AlreadyCurrent { version: String, sha256: String },
    /// The new binary is installed, restarted and verified serving `to`.
    Updated {
        from: String,
        to: String,
        /// The key id the binary verified under.
        key: String,
    },
}

/// A successful `rollback`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackOutcome {
    /// What the restored binary answered `--version`, if it could.
    pub restored_version: Option<String>,
    /// What `/v1/health` reported after the restart, if anything.
    pub served_version: Option<String>,
}

/// `solador-agent update`. See the module docs for the order of operations;
/// every step here is one of those numbered steps.
pub async fn run_update(ctx: &mut Context<'_>) -> Result<UpdateOutcome, UpdateError> {
    let lock_path = ctx.install.sibling(".update.lock");
    let _lock = TransactionLock::acquire(lock_path)?;
    refuse_if_we_are_the_service(ctx.service)?;
    ctx.service.preflight().map_err(UpdateError::Install)?;
    let target = ctx.target;
    let inspect = ctx.service.inspect_hint(ctx.install.log.as_deref());

    // Both HTTP clients are built here, before anything is fetched and long
    // before the swap: a client that cannot be built must be a refusal with
    // nothing changed, never a failure discovered with the candidate live.
    let client = release_client(&ctx.release_base, ctx.running_version.as_deref())?;
    let health = health_client(ctx.running_version.as_deref())?;

    // 3. The concrete release, and its feed — exact bytes verified first.
    let discovery = reqwest::Client::builder()
        .user_agent(user_agent(ctx.running_version.as_deref()))
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| UpdateError::Network {
            what: "http client".to_string(),
            reason: e.to_string(),
        })?;
    let tag = resolve_latest_tag(&discovery, &ctx.release_base).await?;
    (ctx.report)(&format!("==> Latest published release: {tag}"));

    let feed_url = format!("{}/releases/download/{tag}/{FEED_ASSET}", ctx.release_base);
    let feed_bytes = fetch_asset(&client, &feed_url, FEED_CAP).await?;
    let sig_bytes = fetch_asset(&client, &format!("{feed_url}.minisig"), FEED_CAP).await?;
    let sig_text = String::from_utf8(sig_bytes).map_err(|_| UpdateError::Rejected {
        object: FEED_ASSET.to_string(),
        reason: "the signature file is not UTF-8 text".to_string(),
    })?;
    let (feed, feed_key) = verify_feed(&ctx.trust, &feed_bytes, &sig_text)?;
    check_feed_tag(&feed, &tag)?;
    (ctx.report)(&format!(
        "==> {FEED_ASSET} verified under key {feed_key}; feed version {}",
        feed.version
    ));
    let entry = select_target(&feed, target, &ctx.release_base, &tag)?;
    let asset = asset_name(&feed.version, target);

    // 4. Equal bytes: already current, exit 0, nothing else happens.
    let installed_bytes = fs::read(&ctx.install.binary).map_err(|e| UpdateError::Io {
        what: format!(
            "reading the installed binary {}",
            ctx.install.binary.display()
        ),
        reason: e.to_string(),
    })?;
    let installed_hash = sha256_hex(&installed_bytes);
    drop(installed_bytes);
    if installed_hash == entry.sha256 {
        (ctx.report)(&format!(
            "==> Already current: {} is byte-for-byte the {} the feed names ({installed_hash})",
            ctx.install.binary.display(),
            asset
        ));
        // Bytes on disk are not a running service. An earlier run
        // interrupted between its swap and its restart leaves exactly these
        // bytes with the old process still serving, and "already current,
        // exit 0" forever after would be the fabricated state. So the
        // service is asked — for the version the bytes themselves claim,
        // not the feed's, since equal bytes could only disagree with the
        // feed's number if the feed were wrong about them — and nothing is
        // restarted either way: the remedy is a restart the operator sees.
        // A binary that cannot name its version cannot be verified serving
        // and is refused here for the same reason step 5 refuses it: an
        // unverifiable "current" is not exit 0.
        let v = binary_version(&ctx.install.binary, VERSION_TIMEOUT).map_err(|reason| {
            UpdateError::InstalledVersionUnknown {
                path: ctx.install.binary.clone(),
                reason,
            }
        })?;
        (ctx.report)(&format!(
            "==> Verifying {} is serving {v} ...",
            ctx.serving.health_url()
        ));
        if let Err(reason) = wait_for_health(
            &health,
            &ctx.serving,
            &Expect::Version(v.clone()),
            ctx.health_attempts,
            ctx.health_interval,
            ctx.report,
        )
        .await
        {
            return Err(UpdateError::AlreadyCurrentNotServing {
                version: v,
                reason,
                inspect,
            });
        }
        (ctx.report)(&format!(
            "==> Health OK: {} reports version {v}",
            ctx.serving.health_url()
        ));
        return Ok(UpdateOutcome::AlreadyCurrent {
            version: feed.version.clone(),
            sha256: installed_hash,
        });
    }

    // 5. Newer, or nothing.
    let installed_version =
        binary_version(&ctx.install.binary, VERSION_TIMEOUT).map_err(|reason| {
            UpdateError::InstalledVersionUnknown {
                path: ctx.install.binary.clone(),
                reason,
            }
        })?;
    let Some(have) = CalVer::parse(&installed_version) else {
        return Err(UpdateError::InstalledVersionNotCalVer {
            path: ctx.install.binary.clone(),
            version: installed_version,
        });
    };
    // The feed's version was shape-checked by `Feed::parse`.
    let want = CalVer::parse(&feed.version).ok_or_else(|| {
        UpdateError::FeedMalformed(format!("version '{}' is not a CalVer", feed.version))
    })?;
    if want <= have {
        return Err(UpdateError::NotNewer {
            installed: installed_version,
            feed: feed.version.clone(),
        });
    }
    (ctx.report)(&format!(
        "==> Installed {} ({installed_version}) differs from {asset}; downloading",
        ctx.install.binary.display()
    ));

    // 6. The bytes, verified in memory before anything touches disk.
    let bytes = fetch_asset(&client, &entry.url, BINARY_CAP).await?;
    let key = verify_binary(&ctx.trust, &asset, entry, &bytes)?;
    (ctx.report)(&format!(
        "==> {asset} verified: signature under key {key}, sha256 {}",
        entry.sha256
    ));

    // 7. Stage, and make the candidate name its version before it can be
    //    installed.
    let new_path = ctx.install.sibling(".new");
    stage_candidate(&new_path, &bytes)?;
    drop(bytes);
    match binary_version(&new_path, VERSION_TIMEOUT) {
        Ok(v) if v == feed.version => {}
        got => {
            let removed = match fs::remove_file(&new_path) {
                Ok(()) => String::new(),
                Err(e) => format!(" (and it could NOT be removed: {e}; delete it by hand)"),
            };
            return Err(UpdateError::Candidate {
                reason: match got {
                    Ok(v) => format!(
                        "{} answered --version '{v}', the feed says {}{removed}",
                        new_path.display(),
                        feed.version
                    ),
                    Err(why) => format!("{} — {why}{removed}", new_path.display()),
                },
            });
        }
    }
    (ctx.report)(&format!(
        "==> Staged {} and it reports {}",
        new_path.display(),
        feed.version
    ));

    // 8. .prev, then the atomic rename. From here on a failure is recovered,
    //    not merely reported.
    let prev_path = ctx.install.sibling(".prev");
    preserve_previous(&ctx.install.binary, &prev_path)?;
    fs::rename(&new_path, &ctx.install.binary).map_err(|e| UpdateError::Io {
        what: format!(
            "renaming {} over {}",
            new_path.display(),
            ctx.install.binary.display()
        ),
        reason: e.to_string(),
    })?;
    (ctx.report)(&format!(
        "==> Installed {} (previous kept as {})",
        ctx.install.binary.display(),
        prev_path.display()
    ));

    // 9. Restart the METRICS service and require the new version served.
    let failure =
        match restart_and_verify(ctx, &health, &Expect::Version(feed.version.clone())).await {
            Ok(_) => {
                (ctx.report)(&format!(
                    "==> Health OK: {} reports version {}",
                    ctx.serving.health_url(),
                    feed.version
                ));
                return Ok(UpdateOutcome::Updated {
                    from: installed_version,
                    to: feed.version,
                    key,
                });
            }
            Err(f) => f,
        };

    // 10. Recover. The previous binary's version is known (step 5), so it is
    //     required back; the service coming up on something else is not a
    //     recovery.
    (ctx.report)(&format!(
        "==> UPDATE FAILED ({failure}); restoring {}",
        prev_path.display()
    ));
    match restore_previous(
        ctx,
        &health,
        &prev_path,
        &Expect::Version(installed_version.clone()),
        &feed.version,
        &installed_version,
    )
    .await
    {
        Ok(()) => {
            (ctx.report)(&format!(
                "==> FAILED: the update to {} did not verify; the service is back on \
                 {installed_version} (recovery verified). Exit 5.",
                feed.version
            ));
            Err(UpdateError::UpdateFailedRecovered {
                failure,
                restored: installed_version,
                inspect,
            })
        }
        Err((recovery, live)) => {
            (ctx.report)(&format!(
                "==> FAILED: the update to {} did not verify AND the recovery failed ({recovery}). \
                 At the live path: {live}. Exit 3.",
                feed.version
            ));
            Err(UpdateError::UpdateFailedRecoveryFailed {
                failure,
                recovery,
                live,
                inspect,
            })
        }
    }
}

/// `solador-agent rollback`: offline, missing-backup-safe, reversible.
pub async fn run_rollback(ctx: &mut Context<'_>) -> Result<RollbackOutcome, UpdateError> {
    let lock_path = ctx.install.sibling(".update.lock");
    let _lock = TransactionLock::acquire(lock_path)?;
    refuse_if_we_are_the_service(ctx.service)?;
    ctx.service.preflight().map_err(UpdateError::Install)?;
    let inspect = ctx.service.inspect_hint(ctx.install.log.as_deref());

    let prev_path = ctx.install.sibling(".prev");
    if !prev_path.is_file() {
        return Err(UpdateError::NoPrevious(prev_path));
    }
    // Built before the swap, for the same reason run_update builds its
    // clients before it fetches.
    let health = health_client(ctx.running_version.as_deref())?;
    // Staged first (mode 0755, whatever mode .prev carries — a hand-placed
    // .prev may well be 0644), and asked its version from there. Known where
    // it can be known: a previous binary that names its version is held to
    // it after the restart; one that cannot (a source build from a shallow
    // checkout) is checked for liveness, and the outcome says which.
    let new_path = ctx.install.sibling(".new");
    copy_executable(&prev_path, &new_path)?;
    let restored_version = match binary_version(&new_path, VERSION_TIMEOUT) {
        Ok(v) => {
            (ctx.report)(&format!(
                "==> Rolling back to {} ({v})",
                prev_path.display()
            ));
            Some(v)
        }
        Err(why) => {
            (ctx.report)(&format!(
                "==> Rolling back to {} (it carries no version — {why}; liveness will be \
                 verified, not a version)",
                prev_path.display()
            ));
            None
        }
    };

    // Swap live and .prev so the rollback is itself reversible: the binary
    // rolled back over becomes the new .prev. Staged and renamed, never
    // written in place; the live path is never absent.
    let displaced = ctx.install.sibling(".rollback-displaced");
    // A leftover from a half-done rollback is the displaced binary's ONLY
    // copy; overwriting it here and reporting a clean rollback would be
    // data loss dressed as success. Refuse until the operator has done the
    // move the earlier message printed.
    if displaced.exists() {
        let _ = fs::remove_file(&new_path);
        return Err(UpdateError::RollbackHalfDone {
            live: ctx.install.binary.clone(),
            prev: prev_path.clone(),
            displaced: displaced.clone(),
            reason: "an earlier rollback left this file behind and it was not moved".to_string(),
        });
    }
    copy_executable(&ctx.install.binary, &displaced)?;
    fs::rename(&new_path, &ctx.install.binary).map_err(|e| UpdateError::Io {
        what: format!(
            "renaming {} over {}",
            new_path.display(),
            ctx.install.binary.display()
        ),
        reason: e.to_string(),
    })?;
    // The live path has changed from here on, so a failure is not an `Io`
    // "before the live path changed": it is a half-done swap, and the
    // displaced binary's only copy is the file that did not move.
    fs::rename(&displaced, &prev_path).map_err(|e| UpdateError::RollbackHalfDone {
        live: ctx.install.binary.clone(),
        prev: prev_path.clone(),
        displaced: displaced.clone(),
        reason: e.to_string(),
    })?;
    (ctx.report)(&format!(
        "==> Restored {}; the displaced binary is now {}",
        ctx.install.binary.display(),
        prev_path.display()
    ));

    let expect = match &restored_version {
        Some(v) => Expect::Version(v.clone()),
        None => Expect::Online,
    };
    let served_version = restart_and_verify(ctx, &health, &expect)
        .await
        .map_err(|reason| UpdateError::RollbackUnhealthy { reason, inspect })?;
    (ctx.report)(&format!(
        "==> Health OK: {} is back online{}",
        ctx.serving.health_url(),
        served_version
            .as_deref()
            .map(|v| format!(", reports version {v}"))
            .unwrap_or_else(|| ", reports no version".to_string())
    ));
    Ok(RollbackOutcome {
        restored_version,
        served_version,
    })
}

/// Refuse to run with root's effective uid. The install is user-owned and
/// the service is that user's; root would target root's own manager and
/// leave root-owned `.new`/`.prev`/lock files beside the user's binary.
/// Takes the euid so the rule is testable without being root.
pub fn refuse_privileged(euid: u32) -> Result<(), UpdateError> {
    if euid == 0 {
        return Err(UpdateError::Privileged { euid });
    }
    Ok(())
}

/// This process's effective uid (`0` where the platform has no such thing,
/// which is never a supported install).
#[cfg(unix)]
#[must_use]
pub fn current_euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
#[must_use]
pub fn current_euid() -> u32 {
    1
}

fn refuse_if_we_are_the_service(service: &dyn ServiceControl) -> Result<(), UpdateError> {
    let me = std::process::id();
    if service.main_pid() == Some(me) {
        return Err(UpdateError::IsTheService { pid: me });
    }
    Ok(())
}

/// Write verified bytes as an executable candidate. Any stale `.new` (a
/// crashed earlier run; we hold the lock, so nobody owns it) is removed
/// first; the file is created fresh, mode 0755, and fsynced.
fn stage_candidate(new_path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let io = |what: String, e: std::io::Error| UpdateError::Io {
        what,
        reason: e.to_string(),
    };
    match fs::remove_file(new_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io(format!("removing a stale {}", new_path.display()), e)),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o755);
    }
    let mut file = options
        .open(new_path)
        .map_err(|e| io(format!("creating {}", new_path.display()), e))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| io(format!("writing {}", new_path.display()), e))?;
    // Closed HERE, explicitly, before any caller can exec the file: a write
    // handle still open at the spawn is `ETXTBSY` on Linux, and the only
    // handle this crate ever holds on a candidate is this one. (A child
    // forked by another thread inside this function's window can still
    // inherit it until its own exec; `spawn_version_probe` covers that.)
    drop(file);
    Ok(())
}

/// Copy an executable through a sibling and a rename, so a crash mid-copy
/// cannot leave a truncated file at the destination.
fn copy_executable(from: &Path, to: &Path) -> Result<(), UpdateError> {
    let io = |what: String, e: std::io::Error| UpdateError::Io {
        what,
        reason: e.to_string(),
    };
    let mut tmp_name = to.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp = to.with_file_name(tmp_name);
    let bytes = fs::read(from).map_err(|e| io(format!("reading {}", from.display()), e))?;
    stage_candidate(&tmp, &bytes)?;
    fs::rename(&tmp, to).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io(format!("renaming {} to {}", tmp.display(), to.display()), e)
    })
}

/// The live executable becomes `.prev` — the rollback anchor — before the
/// swap. Taken before, never after: an anchor copied after the rename would
/// be a copy of the binary being rolled back, and rollback a no-op.
fn preserve_previous(live: &Path, prev: &Path) -> Result<(), UpdateError> {
    copy_executable(live, prev)
}

async fn restart_and_verify(
    ctx: &mut Context<'_>,
    health: &reqwest::Client,
    expect: &Expect,
) -> Result<Option<String>, String> {
    let pid_before = ctx.service.main_pid();
    (ctx.report)(&format!("==> Restarting the {}", ctx.service.describe()));
    ctx.service
        .restart()
        .map_err(|e| format!("restarting the {} failed: {e}", ctx.service.describe()))?;
    match expect {
        Expect::Version(v) => (ctx.report)(&format!(
            "==> Verifying {} reports version {v} ...",
            ctx.serving.health_url()
        )),
        Expect::Online => (ctx.report)(&format!(
            "==> Verifying {} is back online ...",
            ctx.serving.health_url()
        )),
    }
    wait_for_health(
        health,
        &ctx.serving,
        expect,
        ctx.health_attempts,
        ctx.health_interval,
        ctx.report,
    )
    .await
    .map_err(|e| {
        // The manager's own view, so a crash loop (pid moving, or none),
        // "never restarted" (pid unchanged) and "listening elsewhere" (a
        // healthy pid answering the wrong thing) read differently.
        let pid_after = ctx.service.main_pid();
        let show = |p: Option<u32>| p.map_or("none".to_string(), |p| p.to_string());
        format!(
            "{e}; service main pid before restart {}, after {}",
            show(pid_before),
            show(pid_after)
        )
    })
}

/// Put `.prev` back at the live path (staged and renamed, `.prev` itself left
/// as the anchor), restart, and require `expect`. `Err` is the recovery's
/// own failure, distinct from the update's.
/// The `Err` carries the recovery's failure and a description of what is at
/// the live path when it failed.
async fn restore_previous(
    ctx: &mut Context<'_>,
    health: &reqwest::Client,
    prev_path: &Path,
    expect: &Expect,
    candidate: &str,
    previous: &str,
) -> Result<(), (String, String)> {
    let new_path = ctx.install.sibling(".new");
    let still_candidate = |e: String| {
        (
            e,
            format!(
                "the failed candidate ({candidate}); {} still holds {previous}",
                prev_path.display()
            ),
        )
    };
    copy_executable(prev_path, &new_path).map_err(|e| still_candidate(e.to_string()))?;
    fs::rename(&new_path, &ctx.install.binary).map_err(|e| {
        still_candidate(format!(
            "renaming {} over {}: {e}",
            new_path.display(),
            ctx.install.binary.display()
        ))
    })?;
    (ctx.report)(&format!(
        "==> Restored {} from {}",
        ctx.install.binary.display(),
        prev_path.display()
    ));
    let served = restart_and_verify(ctx, health, expect).await.map_err(|e| {
        (
            e,
            format!("the previous binary ({previous}), restored but not verified serving"),
        )
    })?;
    (ctx.report)(&format!(
        "==> Health OK (recovery): {} reports {}",
        ctx.serving.health_url(),
        served
            .as_deref()
            .map(|v| format!("version {v}"))
            .unwrap_or_else(|| "no version, as the restored binary carries none".to_string())
    ));
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests: the pure parts. The transaction is exercised end to end in
// tests/update_flow.rs against a loopback release and a temporary install.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/agent")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
    }

    fn shared_text(name: &str) -> String {
        String::from_utf8(shared_fixture(name)).expect("text fixture")
    }

    // --- The compiled-in trust set ----------------------------------------

    /// The build compiled in the committed key file(s), each decodes, and no
    /// key is listed twice. Two keys is the shipped state once
    /// `release-signing-key-next.pub` is committed; one is the state until
    /// then. Zero is a broken build and cannot reach here (build.rs panics).
    #[test]
    fn the_compiled_in_trust_set_is_the_committed_key_files_and_they_are_distinct() {
        let next_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("release-signing-key-next.pub");
        assert_eq!(
            TRUSTED_PUBLIC_KEYS.len(),
            1 + usize::from(next_path.exists()),
            "the trust set is the current key plus the standby exactly when \
             agent/release-signing-key-next.pub exists; a present file that is not compiled \
             in is a stale build (run `cargo clean -p solador-agent`)"
        );
        let current = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("release-signing-key.pub"),
        )
        .expect("agent/release-signing-key.pub");
        assert_eq!(
            TRUSTED_PUBLIC_KEYS[0], current,
            "the first trusted key must be agent/release-signing-key.pub verbatim"
        );
        let trust = Trust::compiled_in().expect("the compiled-in keys decode and are distinct");
        let ids = trust.key_ids();
        assert_eq!(ids.len(), TRUSTED_PUBLIC_KEYS.len());
        assert_eq!(
            ids[0], "B2E5C62B763FD2C4",
            "the current key's id, read out of its bytes"
        );
        if let Some(next) = TRUSTED_PUBLIC_KEYS.get(1) {
            let next_file = fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("release-signing-key-next.pub"),
            )
            .expect("agent/release-signing-key-next.pub");
            assert_eq!(*next, next_file);
            assert_ne!(ids[0], ids[1], "the standby must be a distinct key");
        }
    }

    #[test]
    fn a_key_listed_twice_is_refused_as_a_trust_set() {
        let k = shared_text("test-agent-key.pub");
        let err = Trust::from_texts(&[&k, &k]).expect_err("refused");
        assert!(matches!(err, UpdateError::Trust(_)), "{err}");
        assert!(err.to_string().contains("twice"), "{err}");
        assert!(Trust::from_texts(&[]).is_err());
    }

    // --- The shared feed fixture: the contract's executable form ----------

    fn fixture_trust() -> Trust {
        Trust::from_texts(&[&shared_text("test-agent-key.pub")]).expect("fixture key")
    }

    #[test]
    fn the_committed_feed_fixture_verifies_and_parses_under_its_key() {
        let bytes = shared_fixture(FEED_ASSET);
        let sig = shared_text("agent-latest.json.minisig");
        let (feed, key) = verify_feed(&fixture_trust(), &bytes, &sig).expect("verifies");
        // The id read out of the fixture key's own bytes, so regenerating
        // the shared pair (tests/fixtures/README.md) moves this suite too.
        assert_eq!(key, key_id(&shared_text("test-agent-key.pub")).unwrap());
        assert_eq!(feed.version, "2026.9.9");
        assert_eq!(feed.targets.len(), 4);
        check_feed_tag(&feed, "v2026.9.9").expect("the tag names the version");
        for t in TARGETS {
            let entry = select_target(&feed, t, RELEASE_BASE, "v2026.9.9").expect(t);
            let asset = asset_name("2026.9.9", t);
            verify_binary(&fixture_trust(), &asset, entry, &shared_fixture(&asset)).expect(t);
        }
    }

    #[test]
    fn a_feed_whose_bytes_moved_by_one_character_is_refused_before_decoding() {
        let mut bytes = shared_fixture(FEED_ASSET);
        let sig = shared_text("agent-latest.json.minisig");
        let i = bytes.len() / 2;
        bytes[i] = if bytes[i] == b'a' { b'b' } else { b'a' };
        let err = verify_feed(&fixture_trust(), &bytes, &sig).expect_err("refused");
        assert!(matches!(err, UpdateError::Rejected { .. }), "{err}");

        // The final newline is a byte the signature covers.
        let mut bytes = shared_fixture(FEED_ASSET);
        assert_eq!(bytes.pop(), Some(b'\n'));
        let err = verify_feed(&fixture_trust(), &bytes, &sig).expect_err("refused");
        assert!(matches!(err, UpdateError::Rejected { .. }), "{err}");
    }

    #[test]
    fn a_feed_signed_by_a_key_this_build_does_not_trust_is_refused() {
        let bytes = shared_fixture(FEED_ASSET);
        let sig = shared_text("agent-latest.json.minisig");
        // The production key does not trust the fixture key.
        let err = verify_feed(&Trust::compiled_in().unwrap(), &bytes, &sig).expect_err("refused");
        match err {
            UpdateError::Rejected { reason, .. } => {
                assert!(reason.contains("does not trust"), "{reason}")
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn a_signature_made_for_another_file_is_refused_even_though_it_verifies() {
        // The feed's own signature, offered as the signature of a binary.
        let bytes = shared_fixture(FEED_ASSET);
        let sig = shared_text("agent-latest.json.minisig");
        let err = fixture_trust()
            .verify("solador-agent-2026.9.9-x86_64-apple-darwin", &sig, &bytes)
            .expect_err("refused");
        match err {
            UpdateError::Rejected { reason, .. } => {
                assert!(reason.contains("made for 'agent-latest.json'"), "{reason}")
            }
            other => panic!("{other}"),
        }
    }

    #[test]
    fn a_verified_binary_whose_hash_is_not_the_entrys_is_refused() {
        let bytes = shared_fixture(FEED_ASSET);
        let sig = shared_text("agent-latest.json.minisig");
        let (feed, _) = verify_feed(&fixture_trust(), &bytes, &sig).unwrap();
        let t = "x86_64-unknown-linux-musl";
        let asset = asset_name("2026.9.9", t);
        let mut entry = feed.targets[t].clone();
        entry.sha256 = "0".repeat(64);
        let err = verify_binary(&fixture_trust(), &asset, &entry, &shared_fixture(&asset))
            .expect_err("refused");
        assert!(matches!(err, UpdateError::HashMismatch { .. }), "{err}");

        // And a binary whose bytes moved fails the signature, never the hash.
        let mut tampered = shared_fixture(&asset);
        tampered[0] ^= 0x01;
        let err = verify_binary(&fixture_trust(), &asset, &feed.targets[t], &tampered)
            .expect_err("refused");
        assert!(matches!(err, UpdateError::Rejected { .. }), "{err}");
    }

    #[test]
    fn the_feed_must_name_the_release_it_was_fetched_from() {
        let bytes = shared_fixture(FEED_ASSET);
        let feed = Feed::parse(&bytes).unwrap();
        let err = check_feed_tag(&feed, "v2026.9.10").expect_err("refused");
        assert!(matches!(err, UpdateError::FeedTag { .. }), "{err}");
    }

    #[test]
    fn a_feed_entry_may_not_point_anywhere_but_the_asset_on_the_release() {
        let bytes = shared_fixture(FEED_ASSET);
        let mut feed = Feed::parse(&bytes).unwrap();
        let t = "aarch64-apple-darwin";
        feed.targets.get_mut(t).unwrap().url =
            "https://example.invalid/solador-agent-2026.9.9-aarch64-apple-darwin".to_string();
        let err = select_target(&feed, t, RELEASE_BASE, "v2026.9.9").expect_err("refused");
        assert!(matches!(err, UpdateError::FeedUrl { .. }), "{err}");
        let err = select_target(
            &feed,
            "riscv64gc-unknown-linux-musl",
            RELEASE_BASE,
            "v2026.9.9",
        )
        .expect_err("refused");
        assert!(matches!(err, UpdateError::TargetMissing(_)), "{err}");
    }

    #[test]
    fn unknown_json_keys_are_tolerated_and_malformed_fields_are_not() {
        let mut v: serde_json::Value = serde_json::from_slice(&shared_fixture(FEED_ASSET)).unwrap();
        v["keyId"] = serde_json::json!("B2E5C62B763FD2C4");
        v["targets"]["x86_64-apple-darwin"]["note"] = serde_json::json!("added later");
        let feed = Feed::parse(&serde_json::to_vec(&v).unwrap()).expect("additions tolerated");
        assert_eq!(feed.version, "2026.9.9");

        let mut bad = v.clone();
        bad["targets"]["x86_64-apple-darwin"]["sha256"] = serde_json::json!("ABC");
        assert!(matches!(
            Feed::parse(&serde_json::to_vec(&bad).unwrap()),
            Err(UpdateError::FeedMalformed(_))
        ));
        let mut bad = v.clone();
        bad["version"] = serde_json::json!("v2026.9.9");
        assert!(matches!(
            Feed::parse(&serde_json::to_vec(&bad).unwrap()),
            Err(UpdateError::FeedMalformed(_))
        ));
        let mut bad = v.clone();
        bad["targets"]["x86_64-apple-darwin"]["signature"] = serde_json::json!("not a sig");
        assert!(matches!(
            Feed::parse(&serde_json::to_vec(&bad).unwrap()),
            Err(UpdateError::FeedMalformed(_))
        ));
        assert!(matches!(
            Feed::parse(b"{not json"),
            Err(UpdateError::FeedMalformed(_))
        ));
    }

    // --- Versions, targets, URLs ----------------------------------------------

    #[test]
    fn calver_parses_strictly_and_orders_numerically() {
        assert_eq!(CalVer::parse("2026.9.12"), Some(CalVer(2026, 9, 12)));
        assert_eq!(CalVer::parse("2026.10.1"), Some(CalVer(2026, 10, 1)));
        for bad in [
            "v2026.9.12",
            "2026.09.1",
            "26.9.1",
            "2026.9",
            "0.5.0",
            "",
            "2026.9.1.0",
        ] {
            assert_eq!(CalVer::parse(bad), None, "{bad}");
        }
        assert!(CalVer(2026, 10, 1) > CalVer(2026, 9, 30));
        assert!(CalVer(2027, 1, 1) > CalVer(2026, 12, 99));
        assert!(is_release_tag("v2026.9.12"));
        assert!(!is_release_tag("2026.9.12"));
        assert!(!is_release_tag("vmain"));
    }

    #[test]
    fn host_targets_map_onto_the_four_published_triples_and_nothing_else() {
        assert_eq!(
            target_for("linux", "x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_for("linux", "aarch64").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert_eq!(
            target_for("macos", "aarch64").unwrap(),
            "aarch64-apple-darwin"
        );
        assert_eq!(
            target_for("macos", "x86_64").unwrap(),
            "x86_64-apple-darwin"
        );
        for (os, arch) in [
            ("windows", "x86_64"),
            ("linux", "riscv64"),
            ("freebsd", "x86_64"),
        ] {
            assert!(matches!(
                target_for(os, arch),
                Err(UpdateError::UnsupportedPlatform { .. })
            ));
        }
        for t in TARGETS {
            assert!(target_for(
                if t.contains("linux") {
                    "linux"
                } else {
                    "macos"
                },
                t.split('-').next().unwrap()
            )
            .is_ok());
        }
    }

    #[test]
    fn health_url_mirrors_lib_sh() {
        assert_eq!(health_url("", 7878), "http://127.0.0.1:7878/v1/health");
        assert_eq!(
            health_url("0.0.0.0", 7878),
            "http://127.0.0.1:7878/v1/health"
        );
        assert_eq!(health_url("::", 7878), "http://[::1]:7878/v1/health");
        assert_eq!(health_url("[::]", 9000), "http://[::1]:9000/v1/health");
        assert_eq!(
            health_url("fd7a::1", 9000),
            "http://[fd7a::1]:9000/v1/health"
        );
        assert_eq!(
            health_url("100.64.0.9", 7878),
            "http://100.64.0.9:7878/v1/health"
        );
    }

    #[test]
    fn loopback_bases_are_recognised_and_nothing_else_is() {
        assert!(is_loopback_base("http://127.0.0.1:8080"));
        assert!(is_loopback_base("http://localhost:1"));
        assert!(!is_loopback_base("http://example.com"));
        assert!(!is_loopback_base("http://127.0.0.1.evil.example"));
        assert!(!is_loopback_base("https://github.com/Sassy-Dog/solador"));
        // Userinfo that LOOKS like a loopback host is not a loopback host.
        assert!(!is_loopback_base("http://127.0.0.1:1@evil.example/"));
        assert!(!is_loopback_base("http://localhost@evil.example:80/x"));
        assert!(!is_loopback_base("not a url"));
    }

    // --- The env file ---------------------------------------------------------

    #[test]
    fn the_env_file_is_read_with_environmentfile_semantics_and_never_evaluated() {
        let text =
            "SOLADOR_AGENT_TOKEN=first\r\n# comment\n\nSOLADOR_AGENT_BIND=  \"127.0.0.1\"  \n\
                    SOLADOR_AGENT_PORT='17878'\nSOLADOR_AGENT_TOKEN=abc$(touch /tmp/x)`id`;x\n\
                    RUST_LOG= debug \r\nnot a key\n";
        let map = parse_env_file(text);
        assert_eq!(map["SOLADOR_AGENT_TOKEN"], "abc$(touch /tmp/x)`id`;x");
        assert_eq!(map["SOLADOR_AGENT_BIND"], "127.0.0.1");
        assert_eq!(map["SOLADOR_AGENT_PORT"], "17878");
        assert_eq!(map["RUST_LOG"], "debug");
        assert!(!map.contains_key("not a key"));
        // Inside one pair of quotes the value is taken as-is, exactly as
        // systemd's EnvironmentFile= and lib.sh's env_value take it.
        assert_eq!(parse_env_file("K=\" a \"\n")["K"], " a ");
    }

    #[test]
    fn serving_redacts_the_token_and_reads_bind_and_port() {
        let dir = tempfile::tempdir().unwrap();
        let env = dir.path().join("agent.env");
        fs::write(
            &env,
            "SOLADOR_AGENT_TOKEN=very-secret\nSOLADOR_AGENT_BIND=::\nSOLADOR_AGENT_PORT=9000\n",
        )
        .unwrap();
        let serving = read_serving(&env).unwrap();
        assert_eq!(serving.token(), "very-secret");
        assert_eq!(serving.health_url(), "http://[::1]:9000/v1/health");
        let dbg = format!("{serving:?}");
        assert!(!dbg.contains("very-secret"), "{dbg}");
        assert!(dbg.contains("<redacted>"), "{dbg}");

        fs::write(&env, "SOLADOR_AGENT_BIND=127.0.0.1\n").unwrap();
        let err = read_serving(&env).expect_err("no token is refused");
        assert!(err.to_string().contains("SOLADOR_AGENT_TOKEN"), "{err}");

        // The agent reads an unparseable port as 7878, so the probe does too.
        fs::write(
            &env,
            "SOLADOR_AGENT_TOKEN=t\nSOLADOR_AGENT_BIND=127.0.0.1\nSOLADOR_AGENT_PORT=abc\n",
        )
        .unwrap();
        assert_eq!(read_serving(&env).unwrap().port, 7878);

        // No bind, or an empty one, is refused: the agent would have
        // detected a tailnet address this command cannot dial.
        for text in [
            "SOLADOR_AGENT_TOKEN=t\n",
            "SOLADOR_AGENT_TOKEN=t\nSOLADOR_AGENT_BIND=\n",
        ] {
            fs::write(&env, text).unwrap();
            let err = read_serving(&env).expect_err("no bind is refused");
            assert!(
                err.to_string().contains("names no SOLADOR_AGENT_BIND"),
                "{err}"
            );
        }
    }

    // --- The install contract -------------------------------------------------

    #[test]
    fn the_systemd_unit_is_read_the_way_install_sh_renders_it() {
        let home = Path::new("/home/op");
        let bare = "[Service]\nExecStart=/home/op/.local/bin/solador-agent\n\
                    EnvironmentFile=%h/.config/solador-agent.env\n";
        assert_eq!(
            systemd_exec_start(bare).unwrap(),
            PathBuf::from("/home/op/.local/bin/solador-agent")
        );
        assert_eq!(
            systemd_environment_file(bare, home).unwrap(),
            PathBuf::from("/home/op/.config/solador-agent.env")
        );
        let quoted =
            "ExecStart=\"/home/some one/.local/bin/solador-agent\"\nEnvironmentFile=-%h/x.env\n";
        assert_eq!(
            systemd_exec_start(quoted).unwrap(),
            PathBuf::from("/home/some one/.local/bin/solador-agent")
        );
        assert_eq!(
            systemd_environment_file(quoted, home).unwrap(),
            PathBuf::from("/home/op/x.env")
        );
        let opt = "ExecStart=/opt/solador-agent/solador-agent\n";
        assert_eq!(
            systemd_exec_start(opt).unwrap(),
            PathBuf::from("/opt/solador-agent/solador-agent")
        );
        assert!(systemd_exec_start("Description=x\n").is_none());
        assert!(systemd_exec_start("ExecStart=relative/path\n").is_none());
        assert!(systemd_environment_file("ExecStart=/x\n", home).is_none());
    }

    #[test]
    fn the_launchagent_plist_is_read_as_the_installer_renders_it() {
        let json = r#"{"Label":"app.solador.agent","ProgramArguments":["/Users/op/.local/bin/solador-agent-launchd","/Users/op/.local/bin/solador-agent","/Users/op/.config/solador-agent.env","/Users/op/Library/Logs/solador-agent.log"],"RunAtLoad":true}"#;
        let (bin, env, log) = launchd_program_arguments(json).unwrap();
        assert_eq!(bin, PathBuf::from("/Users/op/.local/bin/solador-agent"));
        assert_eq!(env, PathBuf::from("/Users/op/.config/solador-agent.env"));
        assert_eq!(
            log,
            Some(PathBuf::from("/Users/op/Library/Logs/solador-agent.log"))
        );
        // The log is where to look, not a requirement: a trimmed plist still
        // names a service.
        let (_, _, log) =
            launchd_program_arguments(r#"{"ProgramArguments":["/l","/b","/e"]}"#).unwrap();
        assert_eq!(log, None);
        assert!(launchd_program_arguments(r#"{"Label":"x"}"#).is_err());
        assert!(launchd_program_arguments(r#"{"ProgramArguments":["/l","/b"]}"#).is_err());
        assert!(launchd_program_arguments(r#"{"ProgramArguments":["/l","b","/e"]}"#).is_err());
        assert!(valid_launchd_label("app.solador.agent"));
        assert!(valid_launchd_label("app.solador.agent.deploytest.123"));
        assert!(!valid_launchd_label(".hidden"));
        assert!(!valid_launchd_label("a/b"));
        assert!(!valid_launchd_label(""));
        assert_eq!(
            launchctl_pid("\tstate = running\n\tpid = 4321\n"),
            Some(4321)
        );
        assert_eq!(launchctl_pid("\tstate = not running\n"), None);
    }

    #[test]
    fn siblings_sit_beside_the_live_path() {
        let install = Install {
            binary: PathBuf::from("/home/op/.local/bin/solador-agent"),
            env_file: PathBuf::from("/home/op/.config/solador-agent.env"),
            service: Service::Systemd {
                unit: SYSTEMD_UNIT.to_string(),
            },
            log: None,
        };
        assert_eq!(
            install.sibling(".new"),
            PathBuf::from("/home/op/.local/bin/solador-agent.new")
        );
        assert_eq!(
            install.sibling(".update.lock"),
            PathBuf::from("/home/op/.local/bin/solador-agent.update.lock")
        );
    }

    #[test]
    fn a_missing_install_is_refused_with_the_installer_named() {
        let home = tempfile::tempdir().unwrap();
        let err = resolve_install(home.path(), LAUNCHD_LABEL).expect_err("nothing installed");
        match std::env::consts::OS {
            "linux" | "macos" => {
                assert!(matches!(err, UpdateError::Install(_)), "{err}");
                assert!(err.to_string().contains("install.sh"), "{err}");
            }
            _ => assert!(matches!(err, UpdateError::UnsupportedPlatform { .. })),
        }
    }

    /// The Linux `ETXTBSY` race, made deterministic: a helper process holds
    /// the executable open for writing for a moment (what a child forked
    /// mid-write does until its exec), and the probe has to wait it out
    /// rather than report "could not execute". On Linux the first raw spawn
    /// is shown to fail with `ExecutableFileBusy` while the holder lives —
    /// that is what makes this a regression test rather than a pass by
    /// construction; macOS has no such check and only the retry-path result
    /// is asserted there.
    #[cfg(unix)]
    #[test]
    fn a_freshly_written_executable_held_open_by_another_process_is_retried_not_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("agent");
        fs::write(&exe, "#!/bin/sh\nprintf '2026.9.9\\n'\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        // `>>` keeps the contents; the holder keeps fd 3 open for 0.6s.
        let mut holder = Command::new("sh")
            .arg("-c")
            .arg("exec 3>>\"$0\"; sleep 0.6")
            .arg(&exe)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        if cfg!(target_os = "linux") {
            let raw = Command::new(&exe).arg("--version").output();
            assert!(
                matches!(&raw, Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy),
                "on Linux the raw spawn must hit ETXTBSY while the holder lives: {raw:?}"
            );
        }
        let got = binary_version(&exe, Duration::from_secs(5));
        let _ = holder.wait();
        assert_eq!(got.unwrap(), "2026.9.9");
    }

    /// And a file that stays busy past the budget is reported as busy, not
    /// waited on forever. Linux only: elsewhere the exec simply succeeds.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_executable_that_stays_busy_is_reported_after_the_retry_budget() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("agent");
        fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        let mut holder = Command::new("sh")
            .arg("-c")
            .arg("exec 3>>\"$0\"; sleep 4")
            .arg(&exe)
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        let err = binary_version(&exe, Duration::from_secs(5)).unwrap_err();
        let _ = holder.kill();
        let _ = holder.wait();
        assert!(err.contains("could not execute"), "{err}");
        assert!(
            err.contains("busy") || err.contains("Text file busy"),
            "{err}"
        );
        assert!(
            started.elapsed() >= ETXTBSY_RETRY_BUDGET,
            "the retry budget was spent before giving up"
        );
    }

    // --- The lock -------------------------------------------------------------

    #[test]
    fn the_transaction_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("solador-agent.update.lock");
        let first = TransactionLock::acquire(path.clone()).expect("first holder");
        // A second open handle in the SAME process: flock locks are per open
        // file description, so this models a second process closely enough
        // to prove the refusal is the lock's.
        let err = TransactionLock::acquire(path.clone()).expect_err("busy");
        assert!(matches!(err, UpdateError::Busy { .. }), "{err}");
        assert_eq!(err.exit_code(), 75);
        drop(first);
        TransactionLock::acquire(path).expect("free again");
    }

    /// The in-process race the macOS CI leg hit: a child forked while the
    /// lock's descriptor is open inherits a reference to the same open file
    /// description, and a `flock` lives as long as ANY such reference does —
    /// so the lock's `File` being dropped here does not release it until
    /// that child execs or exits. In the test binary the child is another
    /// test's `sh` helper caught between fork and exec; on a host it is
    /// anything that spawns while `update` holds the lock. Made
    /// deterministic: the descriptor is `dup`ed WITHOUT `CLOEXEC` (`dup`
    /// clears the flag) so a `sleep` child keeps it past its exec for a
    /// moment. The note in the file names this process, so `acquire` must
    /// recognise its own stale lock and wait it out — and a note naming
    /// someone else must stay an immediate busy.
    #[cfg(unix)]
    #[test]
    fn a_stale_reference_to_our_own_lock_held_by_a_child_is_waited_out_not_reported_busy() {
        use std::os::fd::AsRawFd as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("solador-agent.update.lock");
        let first = TransactionLock::acquire(path.clone()).expect("first holder");
        // SAFETY: dup of a valid descriptor; the result is closed below.
        let inherited = unsafe { libc::dup(first._file.as_raw_fd()) };
        assert!(inherited >= 0);
        let mut child = Command::new("sh")
            .args(["-c", "sleep 0.7"])
            .stdin(Stdio::null())
            .spawn()
            .expect("sleep child");
        // Our copies are gone; the child's inherited one is not.
        // SAFETY: closing the descriptor dup() returned, once.
        unsafe { libc::close(inherited) };
        drop(first);
        let started = Instant::now();
        let again = TransactionLock::acquire(path.clone());
        let waited = started.elapsed();
        let _ = child.wait();
        let again = again.expect("our own stale lock is waited out, not reported busy");
        assert!(
            waited >= Duration::from_millis(300),
            "the second acquire had to wait for the child ({waited:?})"
        );
        drop(again);

        // A note naming ANOTHER pid is somebody else's transaction: busy at
        // once, no waiting.
        let mut other = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        other.try_lock().unwrap();
        writeln!(
            other,
            "pid={} since=1",
            std::process::id().wrapping_add(7919)
        )
        .unwrap();
        other.sync_all().unwrap();
        let started = Instant::now();
        let err = TransactionLock::acquire(path).expect_err("someone else holds it");
        assert!(matches!(err, UpdateError::Busy { .. }), "{err}");
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "no waiting on another pid"
        );
    }

    // --- Exit codes are distinct where the caller needs them distinct -------

    #[test]
    fn exit_codes_tell_the_three_outcomes_apart() {
        let busy = UpdateError::Busy {
            lock: PathBuf::from("/x"),
            holder: Some("pid=1 since=2".into()),
        };
        assert!(busy.to_string().contains("pid=1 since=2"), "{busy}");
        let recovered = UpdateError::UpdateFailedRecovered {
            failure: "f".into(),
            restored: "2026.9.1".into(),
            inspect: "Inspect:  systemctl --user status solador-agent".into(),
        };
        let unrecovered = UpdateError::UpdateFailedRecoveryFailed {
            failure: "f".into(),
            recovery: "r".into(),
            live: "the candidate (2026.9.9)".into(),
            inspect: "Inspect:  launchctl print gui/501/app.solador.agent".into(),
        };
        assert_eq!(busy.exit_code(), 75);
        assert_eq!(recovered.exit_code(), 5);
        assert_eq!(unrecovered.exit_code(), 3);
        assert_eq!(
            UpdateError::NotNewer {
                installed: "2026.9.13".into(),
                feed: "2026.9.12".into()
            }
            .exit_code(),
            4
        );
        assert_eq!(
            UpdateError::Rejected {
                object: "x".into(),
                reason: "y".into()
            }
            .exit_code(),
            1
        );
        assert!(unrecovered
            .to_string()
            .contains("At the live path now: the candidate"));
        assert!(recovered.to_string().contains("not a success"));
        assert!(unrecovered.to_string().contains("RECOVERY ALSO FAILED"));
        // Every failure that leaves a service to look at says where to look.
        assert!(recovered
            .to_string()
            .contains("Inspect:  systemctl --user status"));
        assert!(unrecovered
            .to_string()
            .contains("Inspect:  launchctl print"));
        let unhealthy = UpdateError::RollbackUnhealthy {
            reason: "r".into(),
            inspect: "Inspect:  x".into(),
        };
        assert!(unhealthy.to_string().contains("Inspect:  x"));
    }

    /// The hint names the manager's own status command and, on macOS, the
    /// log file the plist actually names — not a guessed default when the
    /// real one is known.
    #[test]
    fn inspect_hints_name_the_status_command_and_the_log() {
        let systemd = Service::Systemd {
            unit: "solador-agent".into(),
        };
        let hint = systemd.inspect_hint(None);
        assert!(
            hint.contains("systemctl --user status solador-agent"),
            "{hint}"
        );
        assert!(
            hint.contains("journalctl --user -u solador-agent"),
            "{hint}"
        );
        let launchd = Service::Launchd {
            label: "app.solador.agent".into(),
            uid: 501,
        };
        let hint = launchd.inspect_hint(Some(Path::new("/Users/op/Library/Logs/x.log")));
        assert!(
            hint.contains("launchctl print gui/501/app.solador.agent"),
            "{hint}"
        );
        assert!(
            hint.contains("tail -n 50 \"/Users/op/Library/Logs/x.log\""),
            "{hint}"
        );
        let hint = launchd.inspect_hint(None);
        assert!(hint.contains("~/Library/Logs/solador-agent.log"), "{hint}");
    }

    /// Root is refused before anything is read: the rule is on the euid,
    /// not on a guess about what root would do.
    #[test]
    fn root_is_refused_and_a_user_is_not() {
        let err = refuse_privileged(0).unwrap_err();
        assert!(matches!(err, UpdateError::Privileged { euid: 0 }));
        assert!(err.to_string().contains("without sudo"), "{err}");
        assert_eq!(err.exit_code(), 1);
        refuse_privileged(501).unwrap();
        refuse_privileged(current_euid()).ok();
    }

    // --- The release base and its redirects ----------------------------------

    /// The gate that keeps a shipped binary on HTTPS: `https://` passes,
    /// loopback `http://` passes (tests), any other `http://` is refused,
    /// and the production constant is what the shipped binary passes.
    #[test]
    fn the_release_base_must_be_https_unless_it_is_loopback() {
        assert!(!check_release_base(RELEASE_BASE).unwrap());
        assert!(check_release_base("http://127.0.0.1:8080").unwrap());
        assert!(check_release_base("http://localhost:1").unwrap());
        let err = check_release_base("http://example.com/Sassy-Dog/solador").unwrap_err();
        assert!(matches!(err, UpdateError::Network { .. }), "{err}");
        assert!(err.to_string().contains("not https://"), "{err}");
        assert!(check_release_base("ftp://github.com/x").is_err());
        assert!(check_release_base("http://127.0.0.1.evil.example/x").is_err());
    }

    #[test]
    fn redirects_are_followed_onto_https_only_and_never_past_ten_hops() {
        let gh = Some("objects.githubusercontent.com");
        let lo = Some("127.0.0.1");
        assert!(redirect_allowed(false, "https", gh, 0).is_ok());
        assert!(redirect_allowed(false, "https", gh, 9).is_ok());
        assert!(redirect_allowed(false, "https", gh, 10).is_err());
        assert!(redirect_allowed(false, "http", gh, 0).is_err());
        assert!(redirect_allowed(false, "http", lo, 0).is_err());
        // A loopback base may follow http — onto loopback only.
        assert!(redirect_allowed(true, "http", lo, 0).is_ok());
        assert!(redirect_allowed(true, "http", Some("localhost"), 0).is_ok());
        assert!(redirect_allowed(true, "http", Some("example.invalid"), 0).is_err());
        assert!(redirect_allowed(true, "http", None, 0).is_err());
        assert!(redirect_allowed(true, "https", gh, 0).is_ok());
        assert!(redirect_allowed(true, "ftp", lo, 0).is_err());
        assert!(redirect_allowed(true, "http", lo, 10).is_err());
    }

    // --- Asking a binary its version ------------------------------------------

    /// Every way a `--version` can fail to answer names its reason: the
    /// message that follows ("re-run install.sh") is right for a shallow
    /// build and wrong for `Exec format error`, and the operator must be
    /// able to tell them apart.
    #[cfg(unix)]
    #[test]
    fn a_version_probe_names_why_there_is_no_version() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: &str| {
            let p = dir.path().join(name);
            fs::write(&p, body).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let ok = write("ok", "#!/bin/sh\nprintf '2026.9.9\\n'\n");
        assert_eq!(
            binary_version(&ok, Duration::from_secs(5)).unwrap(),
            "2026.9.9"
        );
        let refuses = write("refuses", "#!/bin/sh\necho 'no version' >&2\nexit 1\n");
        let err = binary_version(&refuses, Duration::from_secs(5)).unwrap_err();
        assert!(
            err.contains("exited") && err.contains("no version"),
            "{err}"
        );
        let mute = write("mute", "#!/bin/sh\nexit 0\n");
        let err = binary_version(&mute, Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("printed nothing"), "{err}");
        let slow = write("slow", "#!/bin/sh\nsleep 5\n");
        let err = binary_version(&slow, Duration::from_millis(200)).unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        let err = binary_version(&dir.path().join("absent"), Duration::from_secs(5)).unwrap_err();
        assert!(err.contains("could not execute"), "{err}");
    }
}
