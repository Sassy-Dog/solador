//! Build `agent-latest.json` for a published release, and verify a signed
//! feed/signature pair the way a consumer will (#391).
//!
//! Run by `.github/workflows/publish-feed.yml`'s protected `agent-feed` job,
//! in two steps with the signer between them:
//!
//! ```text
//! solador-agent-feed build \
//!   --version 2026.9.9 \
//!   --tag v2026.9.9 \
//!   --asset-dir agent-dist \
//!   --download-base https://github.com/Sassy-Dog/solador/releases/download \
//!   --pubkey agent/release-signing-key.pub \
//!   --out feed/agent-latest.json
//!
//! scripts/agent-signing.sh sign feed/agent-latest.json      # → .minisig
//!
//! solador-agent-feed verify \
//!   --feed feed/agent-latest.json \
//!   --signature feed/agent-latest.json.minisig \
//!   --pubkey agent/release-signing-key.pub \
//!   --version 2026.9.9 \
//!   --asset-dir agent-dist
//! ```
//!
//! `build` reads every `solador-agent-<version>-*` binary in `--asset-dir`
//! with the `.minisig` beside it, verifies each under the committed public key,
//! hashes the verified bytes and writes the document — or writes nothing.
//! **An `--out` file that exists after `build` returns is a feed whose every
//! entry verified**: a previous run's file at that path is removed before
//! anything is checked, and the new one is written whole (temp file, then
//! rename) so a failure mid-write leaves nothing there either.
//!
//! `verify` is the consumer's check, run by the producer before upload: the
//! **exact bytes** of the feed against its detached signature first, the JSON
//! second, and (with `--asset-dir`) every binary against its entry — signature
//! and hash both. It never re-serialises: the bytes on disk are the bytes the
//! signature has to cover, and the bytes a consumer will fetch.
//!
//! The signer is deliberately not in here. `scripts/agent-signing.sh` is the
//! one implementation that signs agent artifacts — binaries and feed alike —
//! and a second signer in Rust would be a second version policy to keep in
//! step with it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use updatefeed::agent::{self, Feed, Input, BINARY_PREFIX, TARGETS};

fn main() -> ExitCode {
    match run() {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // `::error::` so the reason lands on the workflow run's summary
            // rather than only in a collapsed log.
            eprintln!("::error::agent feed refused: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("build") => build(BuildArgs::parse(args)?),
        Some("verify") => verify(VerifyArgs::parse(args)?),
        Some(other) => Err(format!(
            "unknown command '{other}' — expected build or verify"
        )),
        None => Err("expected a command: build or verify".to_string()),
    }
}

fn build(args: BuildArgs) -> Result<String, String> {
    // A stale --out from an earlier run must not survive a refusal below and
    // then read as this run's verified output.
    match std::fs::remove_file(&args.out) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!(
                "could not remove stale {}: {e}",
                args.out.display()
            ))
        }
    }
    // Before scanning for files named after it: a version the contract
    // refuses should be refused as a version, not as "no binaries found".
    agent::check_version(&args.version).map_err(|e| e.user_message())?;
    let pubkey = read_text(&args.pubkey)?;
    let key_id = agent::key_id(&pubkey).map_err(|e| e.user_message())?;

    // Every file named like an agent binary for this version, not only the
    // four expected ones, so nothing in the directory is silently left out of
    // a feed that then looks complete. A fifth triple with a `.minisig`
    // beside it reaches the library and is refused there as an unknown
    // target; any other stray (`.sha256`, a directory) is refused below for
    // lacking one. Either way the answer is a refusal, never an omission.
    let prefix = format!("{BINARY_PREFIX}-{}-", args.version);
    let mut names = Vec::new();
    let entries = std::fs::read_dir(&args.asset_dir)
        .map_err(|e| format!("could not read {}: {e}", args.asset_dir.display()))?;
    for entry in entries {
        // An unreadable entry is an I/O error to report, not a file to skip:
        // skipping it would surface as "target missing", which sends whoever
        // reads it to the release rather than to the disk.
        let entry =
            entry.map_err(|e| format!("could not read {}: {e}", args.asset_dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&prefix) && !name.ends_with(".minisig") {
            names.push(name);
        }
    }
    names.sort();
    if names.is_empty() {
        return Err(format!(
            "no {prefix}* binaries in {} — a release without agent assets has no agent feed \
             (v2026.9.3 and earlier predate #390; a tag whose release.yml agent leg failed \
             has none either)",
            args.asset_dir.display()
        ));
    }

    let mut inputs = Vec::with_capacity(names.len());
    for name in names {
        let target = name[prefix.len()..].to_string();
        let path = args.asset_dir.join(&name);
        let sig_path = args.asset_dir.join(format!("{name}.minisig"));
        if !sig_path.is_file() {
            return Err(format!(
                "{name} has no .minisig beside it — refusing to build an entry the feed could \
                 not vouch for"
            ));
        }
        inputs.push(Input {
            url: format!("{}/{}/{name}", args.download_base, args.tag),
            target,
            bytes: read_bytes(&path)?,
            signature: read_text(&sig_path)?,
        });
    }

    // Every refusal — unknown or missing target, foreign key, tampered bytes,
    // bad URL — happens inside build(); a Feed in hand is one that verified.
    let feed =
        Feed::build(&args.version, &args.tag, &pubkey, inputs).map_err(|e| e.user_message())?;
    let bytes = feed.to_bytes();
    // Whole or absent: written beside the destination and renamed over it,
    // so a failure mid-write cannot leave a truncated feed at --out.
    let tmp = args.out.with_extension("json.partial");
    std::fs::write(&tmp, &bytes).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &args.out).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("could not move {} into place: {e}", args.out.display())
    })?;

    Ok(format!(
        "Wrote {} for {} ({} bytes; {} binaries verified under agent key {key_id}):\n{}",
        args.out.display(),
        args.version,
        bytes.len(),
        feed.targets.len(),
        describe(&feed)
    ))
}

fn verify(args: VerifyArgs) -> Result<String, String> {
    let pubkey = read_text(&args.pubkey)?;
    let key_id = agent::key_id(&pubkey).map_err(|e| e.user_message())?;
    let feed_bytes = read_bytes(&args.feed)?;
    let signature = read_text(&args.signature)?;

    // Exact bytes first, JSON second — the consumer's order.
    let feed =
        agent::verify_pair(&pubkey, &feed_bytes, &signature).map_err(|e| e.user_message())?;

    if let Some(expected) = &args.version {
        if &feed.version != expected {
            return Err(format!(
                "{} advertises {}, expected {expected} — this feed describes a different release",
                args.feed.display(),
                feed.version
            ));
        }
    }

    let mut checked = 0;
    if let Some(dir) = &args.asset_dir {
        for target in TARGETS {
            let entry = feed
                .targets
                .get(target)
                .ok_or_else(|| format!("{target} is not in the feed"))?;
            let path = dir.join(agent::asset_name(&feed.version, target));
            let bytes = read_bytes(&path)?;
            agent::verify_binary(&pubkey, &feed.version, target, entry, &bytes)
                .map_err(|e| e.user_message())?;
            checked += 1;
        }
    }

    Ok(format!(
        "{} ({} bytes) verifies under agent key {key_id} and describes {}; {} binaries checked \
         against their entries:\n{}",
        args.feed.display(),
        feed_bytes.len(),
        feed.version,
        checked,
        describe(&feed)
    ))
}

fn describe(feed: &Feed) -> String {
    feed.targets
        .iter()
        .map(|(target, entry)| format!("  {target}  sha256 {}  {}", entry.sha256, entry.url))
        .collect::<Vec<_>>()
        .join("\n")
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    String::from_utf8(read_bytes(path)?).map_err(|_| format!("{} is not UTF-8", path.display()))
}

/// Hand-parsed for the reason `solador-update-feed` gives: this workspace
/// takes no argument-parsing dependency, and a handful of flags does not
/// justify becoming the first.
struct BuildArgs {
    version: String,
    tag: String,
    asset_dir: PathBuf,
    download_base: String,
    pubkey: PathBuf,
    out: PathBuf,
}

impl BuildArgs {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut version = None;
        let mut tag = None;
        let mut asset_dir = None;
        let mut download_base = None;
        let mut pubkey = None;
        let mut out = None;

        let mut args = args;
        while let Some(flag) = args.next() {
            let mut value = || value_for(&flag, args.next());
            match flag.as_str() {
                "--version" => version = Some(value()?),
                "--tag" => tag = Some(value()?),
                "--asset-dir" => asset_dir = Some(PathBuf::from(value()?)),
                "--download-base" => {
                    download_base = Some(value()?.trim_end_matches('/').to_string())
                }
                "--pubkey" => pubkey = Some(PathBuf::from(value()?)),
                "--out" => out = Some(PathBuf::from(value()?)),
                other => return Err(format!("unknown argument {other}")),
            }
        }

        Ok(BuildArgs {
            version: required(version, "--version")?,
            tag: required(tag, "--tag")?,
            asset_dir: required(asset_dir, "--asset-dir")?,
            download_base: required(download_base, "--download-base")?,
            pubkey: required(pubkey, "--pubkey")?,
            out: required(out, "--out")?,
        })
    }
}

struct VerifyArgs {
    feed: PathBuf,
    signature: PathBuf,
    pubkey: PathBuf,
    version: Option<String>,
    asset_dir: Option<PathBuf>,
}

impl VerifyArgs {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut feed = None;
        let mut signature = None;
        let mut pubkey = None;
        let mut version = None;
        let mut asset_dir = None;

        let mut args = args;
        while let Some(flag) = args.next() {
            let mut value = || value_for(&flag, args.next());
            match flag.as_str() {
                "--feed" => feed = Some(PathBuf::from(value()?)),
                "--signature" => signature = Some(PathBuf::from(value()?)),
                "--pubkey" => pubkey = Some(PathBuf::from(value()?)),
                "--version" => version = Some(value()?),
                "--asset-dir" => asset_dir = Some(PathBuf::from(value()?)),
                other => return Err(format!("unknown argument {other}")),
            }
        }

        Ok(VerifyArgs {
            feed: required(feed, "--feed")?,
            signature: required(signature, "--signature")?,
            pubkey: required(pubkey, "--pubkey")?,
            version,
            asset_dir,
        })
    }
}

/// An empty string is how an unset workflow variable arrives. Taking it would
/// build a feed from nothing, so it is refused like a missing value.
fn value_for(flag: &str, value: Option<String>) -> Result<String, String> {
    match value {
        Some(v) if !v.trim().is_empty() => Ok(v),
        Some(_) => Err(format!("{flag} was given an empty value")),
        None => Err(format!("{flag} needs a value")),
    }
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{flag} is required"))
}
