//! Does this minisign `.sig` cover this file, under the key the app ships?
//!
//! Run by `release.yml`'s `release-windows` job straight after it minisigns the
//! Authenticode-signed installer, and **before** it uploads either file
//! (#705). It is the release-time twin of the check `solador-update-feed`
//! repeats at publish time, and it is the same verifier — the plugin's own
//! algorithm, reproduced step for step by [`updatefeed::signature::verify`] —
//! because the Tauri CLI has `signer sign` and no `signer verify`, and "the
//! signer exited 0" is not a check: the claim that matters is that the bytes
//! about to ship verify under the public key compiled into the app.
//!
//! ```text
//! solador-verify-signature \
//!   --file Solador_2026.8.114_x64-setup.exe \
//!   --signature Solador_2026.8.114_x64-setup.exe.sig \
//!   --tauri-config app/src-tauri/tauri.conf.json
//! ```
//!
//! The public key is read out of `tauri.conf.json`, never passed in, for the
//! reason `solador-update-feed` gives. Exit 0 means it verified; anything else,
//! including a missing or empty `.sig`, is a refusal.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use updatefeed::signature;

fn main() -> ExitCode {
    match run() {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("::error::signature refused: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<String, String> {
    let args = Args::parse(std::env::args().skip(1))?;

    let conf: serde_json::Value = serde_json::from_slice(&read_bytes(&args.tauri_config)?)
        .map_err(|e| format!("{} is not valid JSON: {e}", args.tauri_config.display()))?;
    let pubkey = conf["plugins"]["updater"]["pubkey"]
        .as_str()
        .map(str::to_string)
        .filter(|k| !k.trim().is_empty())
        .ok_or_else(|| {
            format!(
                "{} declares no plugins.updater.pubkey",
                args.tauri_config.display()
            )
        })?;
    let key_id = signature::key_id(&pubkey).ok_or_else(|| {
        format!(
            "{} carries no readable minisign key id",
            args.tauri_config.display()
        )
    })?;

    let payload = read_bytes(&args.file)?;
    let sig = String::from_utf8(read_bytes(&args.signature)?)
        .map_err(|_| format!("{} is not UTF-8", args.signature.display()))?;

    signature::verify(&pubkey, &sig, &payload).map_err(|e| e.user_message())?;

    Ok(format!(
        "{} ({} bytes) verified against {} under minisign key {key_id}",
        args.file.display(),
        payload.len(),
        args.signature.display()
    ))
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// Hand-parsed for the reason `solador-update-feed` gives.
struct Args {
    file: PathBuf,
    signature: PathBuf,
    tauri_config: PathBuf,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut file = None;
        let mut signature = None;
        let mut tauri_config = None;

        let mut args = args;
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{flag} needs a value"))
                    // An empty string is how an unset workflow variable arrives.
                    .and_then(|v| {
                        if v.trim().is_empty() {
                            Err(format!("{flag} was given an empty value"))
                        } else {
                            Ok(PathBuf::from(v))
                        }
                    })
            };
            match flag.as_str() {
                "--file" => file = Some(value()?),
                "--signature" => signature = Some(value()?),
                "--tauri-config" => tauri_config = Some(value()?),
                other => return Err(format!("unknown argument {other}")),
            }
        }

        let required =
            |v: Option<PathBuf>, flag: &str| v.ok_or_else(|| format!("{flag} is required"));
        Ok(Args {
            file: required(file, "--file")?,
            signature: required(signature, "--signature")?,
            tauri_config: required(tauri_config, "--tauri-config")?,
        })
    }
}
