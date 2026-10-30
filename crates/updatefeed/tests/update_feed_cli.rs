//! The `solador-update-feed` binary, exercised as `publish-feed.yml` runs it.
//!
//! The library tests prove the verifier and the manifest; these prove the part
//! only the binary does: that the Windows installer, its signature and its URL
//! are **required**, that a Windows artifact missing from disk is a refusal,
//! and that nothing is written on any refusal. A feed with no `windows-x86_64`
//! key fails every updater-enabled Windows install on every launch (#368's
//! symptom), so "missing" must be an error here and never a macOS-only feed.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_solador-update-feed");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/updater")
        .join(name)
}

struct Scratch(PathBuf);

impl Scratch {
    /// A directory holding the fixture payload and signature under both the
    /// macOS and the Windows names, and a `tauri.conf.json` carrying the
    /// fixture's public key.
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("update-feed-cli-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        for (src, dst) in [
            ("payload.tar.gz", "mac.app.tar.gz"),
            ("payload.tar.gz.sig", "mac.app.tar.gz.sig"),
            ("payload.tar.gz", "win-setup.exe"),
            ("payload.tar.gz.sig", "win-setup.exe.sig"),
        ] {
            std::fs::copy(fixture(src), dir.join(dst)).expect("copy fixture");
        }
        let pubkey = std::fs::read_to_string(fixture("test-key.pub")).expect("pubkey fixture");
        let conf = serde_json::json!({ "plugins": { "updater": { "pubkey": pubkey.trim() } } });
        std::fs::write(dir.join("tauri.conf.json"), conf.to_string()).expect("write config");
        Scratch(dir)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_string()
    }
    fn out(&self) -> PathBuf {
        self.0.join("latest.json")
    }
}

/// Run with the full flag set, minus any flag named in `omit`.
fn run(s: &Scratch, omit: &[&str], overrides: &[(&str, String)]) -> (bool, String) {
    let mut flags: Vec<(&str, String)> = vec![
        ("--version", "2026.8.114".to_string()),
        ("--tarball", s.path("mac.app.tar.gz")),
        ("--signature", s.path("mac.app.tar.gz.sig")),
        (
            "--url",
            "https://example.invalid/v2026.8.114/Solador.app.tar.gz".to_string(),
        ),
        ("--windows-installer", s.path("win-setup.exe")),
        ("--windows-signature", s.path("win-setup.exe.sig")),
        (
            "--windows-url",
            "https://example.invalid/v2026.8.114/Solador-setup.exe".to_string(),
        ),
        ("--tauri-config", s.path("tauri.conf.json")),
        ("--out", s.out().to_str().unwrap().to_string()),
    ];
    for (flag, value) in overrides {
        for f in flags.iter_mut().filter(|(name, _)| name == flag) {
            f.1 = value.clone();
        }
    }
    let mut cmd = Command::new(BIN);
    for (flag, value) in flags.iter().filter(|(name, _)| !omit.contains(name)) {
        cmd.arg(flag).arg(value);
    }
    let output = cmd.output().expect("run the binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

#[test]
fn a_complete_release_writes_all_three_targets() {
    let s = Scratch::new("complete");
    let (ok, text) = run(&s, &[], &[]);
    assert!(ok, "{text}");
    let doc: serde_json::Value =
        serde_json::from_slice(&std::fs::read(s.out()).expect("out written")).expect("json");
    for key in ["darwin-aarch64", "darwin-x86_64", "windows-x86_64"] {
        assert!(doc["platforms"][key].is_object(), "{key} missing: {doc}");
    }
    assert_eq!(
        doc["platforms"]["windows-x86_64"]["url"],
        "https://example.invalid/v2026.8.114/Solador-setup.exe"
    );
}

#[test]
fn each_windows_flag_is_required() {
    for flag in [
        "--windows-installer",
        "--windows-signature",
        "--windows-url",
    ] {
        let s = Scratch::new(&format!("omit{flag}"));
        let (ok, text) = run(&s, &[flag], &[]);
        assert!(!ok, "omitting {flag} must fail: {text}");
        assert!(text.contains(&format!("{flag} is required")), "{text}");
        assert!(!s.out().exists(), "a refusal must write nothing");
    }
}

#[test]
fn a_windows_installer_missing_from_disk_is_refused_not_skipped() {
    let s = Scratch::new("missing-installer");
    std::fs::remove_file(s.0.join("win-setup.exe")).expect("remove");
    let (ok, text) = run(&s, &[], &[]);
    assert!(!ok, "{text}");
    assert!(text.contains("win-setup.exe"), "{text}");
    assert!(!s.out().exists());
}

#[test]
fn a_windows_signature_missing_from_disk_is_refused_not_skipped() {
    let s = Scratch::new("missing-sig");
    std::fs::remove_file(s.0.join("win-setup.exe.sig")).expect("remove");
    let (ok, text) = run(&s, &[], &[]);
    assert!(!ok, "{text}");
    assert!(text.contains("win-setup.exe.sig"), "{text}");
    assert!(!s.out().exists());
}

#[test]
fn an_installer_that_changed_after_signing_is_refused() {
    let s = Scratch::new("tampered");
    let mut bytes = std::fs::read(s.0.join("win-setup.exe")).expect("read");
    bytes[0] ^= 0x01;
    std::fs::write(s.0.join("win-setup.exe"), bytes).expect("write");
    let (ok, text) = run(&s, &[], &[]);
    assert!(!ok, "{text}");
    assert!(text.contains("Windows"), "{text}");
    assert!(!s.out().exists());
}
