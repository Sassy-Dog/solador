//! Build `latest.json` for a published release — and refuse to, if the
//! signature does not cover the artifact.
//!
//! Run by `.github/workflows/publish-feed.yml` on `release: published`. It is a
//! binary rather than a workflow step full of `jq` because the check it exists
//! to perform is a cryptographic one, and because the manifest's fields each
//! have a way of being wrong that breaks *every* installed app at once rather
//! than degrading one line (see `updatefeed::manifest`).
//!
//! ```text
//! solador-update-feed \
//!   --version 2026.8.114 \
//!   --tarball  Solador-2026.8.114.app.tar.gz \
//!   --signature Solador-2026.8.114.app.tar.gz.sig \
//!   --url https://github.com/Sassy-Dog/solador/releases/download/v2026.8.114/Solador-2026.8.114.app.tar.gz \
//!   --tauri-config app/src-tauri/tauri.conf.json \
//!   --pub-date 2026-08-15T12:00:00Z \
//!   --notes-file notes.md \
//!   --out latest.json
//! ```
//!
//! The public key is read out of `tauri.conf.json` rather than passed in, so
//! the key this gate verifies under is, by construction, the key the app
//! compiles in. A flag would be a second place for it to be wrong.
//!
//! **Nothing is written unless every check passes.** An `--out` file that
//! exists is a manifest that verified.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use updatefeed::manifest::{Artifact, Feed};
use updatefeed::signature;

fn main() -> ExitCode {
    match run() {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // `::error::` so the reason lands on the workflow run's summary
            // rather than only in a collapsed log.
            eprintln!("::error::update feed refused: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let args = Args::parse(std::env::args().skip(1))?;

    let pubkey = pubkey_from_tauri_config(&args.tauri_config)?;
    let key_id = signature::key_id(&pubkey).ok_or_else(|| {
        format!(
            "{} carries no readable minisign key id",
            args.tauri_config.display()
        )
    })?;

    let payload = read_bytes(&args.tarball)?;
    let sig = read_text(&args.signature)?;

    // Before anything is built, let alone written. This is the acceptance
    // item: a feed entry whose signature does not verify must never be
    // published, and "the toolchain that produced it would not have got this
    // wrong" is not a check.
    signature::verify(&pubkey, &sig, &payload).map_err(|e| e.user_message())?;

    let notes = match &args.notes_file {
        Some(path) => {
            let text = read_text(path)?;
            // An empty notes file is *no notes*, not notes that say nothing.
            Some(text).filter(|t| !t.trim().is_empty())
        }
        None => None,
    };

    let feed = Feed::macos_universal(
        &args.version,
        notes,
        args.pub_date.clone(),
        Artifact {
            url: args.url.clone(),
            signature: sig.trim().to_string(),
        },
    )
    .map_err(|e| e.user_message())?;

    let mut json = serde_json::to_string_pretty(&feed.to_json())
        .map_err(|e| format!("could not serialize the manifest: {e}"))?;
    json.push('\n');
    std::fs::write(&args.out, json)
        .map_err(|e| format!("could not write {}: {e}", args.out.display()))?;

    Ok(format!(
        "Wrote {} for {} ({} bytes verified under minisign key {key_id}), targets: {}",
        args.out.display(),
        args.version,
        payload.len(),
        feed.platforms
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Read `plugins.updater.pubkey` out of the app's own configuration.
fn pubkey_from_tauri_config(path: &Path) -> Result<String, String> {
    let conf: serde_json::Value = serde_json::from_slice(&read_bytes(path)?)
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    conf["plugins"]["updater"]["pubkey"]
        .as_str()
        .map(str::to_string)
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "{} declares no plugins.updater.pubkey — the app would ship with no key to verify \
                 updates against",
                path.display()
            )
        })
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    String::from_utf8(read_bytes(path)?).map_err(|_| format!("{} is not UTF-8", path.display()))
}

/// The arguments, all required except the two that describe a release rather
/// than identify it.
///
/// Hand-parsed rather than through `clap`: this workspace takes no
/// argument-parsing dependency anywhere (`main.rs`'s `--dump-*` flags are
/// hand-read too), and six flags do not justify becoming the first.
struct Args {
    version: String,
    tarball: PathBuf,
    signature: PathBuf,
    url: String,
    tauri_config: PathBuf,
    notes_file: Option<PathBuf>,
    pub_date: Option<String>,
    out: PathBuf,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut version = None;
        let mut tarball = None;
        let mut signature = None;
        let mut url = None;
        let mut tauri_config = None;
        let mut notes_file = None;
        let mut pub_date = None;
        let mut out = None;

        let mut args = args;
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value"))
                    // An empty string is how an unset workflow variable arrives.
                    // Taking it would produce a manifest built from nothing.
                    .and_then(|v| {
                        if v.trim().is_empty() {
                            Err(format!("{flag} was given an empty value"))
                        } else {
                            Ok(v)
                        }
                    })
            };
            match flag.as_str() {
                "--version" => version = Some(value()?),
                "--tarball" => tarball = Some(PathBuf::from(value()?)),
                "--signature" => signature = Some(PathBuf::from(value()?)),
                "--url" => url = Some(value()?),
                "--tauri-config" => tauri_config = Some(PathBuf::from(value()?)),
                "--notes-file" => notes_file = Some(PathBuf::from(value()?)),
                "--pub-date" => pub_date = Some(value()?),
                "--out" => out = Some(PathBuf::from(value()?)),
                other => return Err(format!("unknown argument {other}")),
            }
        }

        Ok(Args {
            version: required(version, "--version")?,
            tarball: required(tarball, "--tarball")?,
            signature: required(signature, "--signature")?,
            url: required(url, "--url")?,
            tauri_config: required(tauri_config, "--tauri-config")?,
            notes_file,
            pub_date,
            out: required(out, "--out")?,
        })
    }
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T, String> {
    value.ok_or_else(|| format!("{flag} is required"))
}
