//! `solador-verify-signature`, as `release.yml`'s Windows leg runs it between
//! signing the installer and uploading it (#705).
//!
//! The release-time gate has one job: exit non-zero unless the `.sig` covers
//! the file's exact bytes under the key in `tauri.conf.json`. Each refusal below
//! is a way the Windows leg could otherwise upload an installer the app would
//! reject, and a missing or empty `.sig` is one of them.

use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_solador-verify-signature");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/updater")
        .join(name)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir =
            Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("verify-signature-cli-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::copy(fixture("payload.tar.gz"), dir.join("setup.exe")).expect("copy payload");
        std::fs::copy(fixture("payload.tar.gz.sig"), dir.join("setup.exe.sig")).expect("copy sig");
        let pubkey = std::fs::read_to_string(fixture("test-key.pub")).expect("pubkey fixture");
        let conf = serde_json::json!({ "plugins": { "updater": { "pubkey": pubkey.trim() } } });
        std::fs::write(dir.join("tauri.conf.json"), conf.to_string()).expect("write config");
        Scratch(dir)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_string()
    }
    fn run(&self) -> (bool, String) {
        let output = Command::new(BIN)
            .args([
                "--file",
                &self.path("setup.exe"),
                "--signature",
                &self.path("setup.exe.sig"),
                "--tauri-config",
                &self.path("tauri.conf.json"),
            ])
            .output()
            .expect("run the binary");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        (output.status.success(), text)
    }
}

#[test]
fn a_signature_over_the_exact_bytes_passes_and_names_the_key() {
    let (ok, text) = Scratch::new("good").run();
    assert!(ok, "{text}");
    assert!(text.contains("verified"), "{text}");
}

/// The Authenticode-appends-to-the-PE failure: a `.sig` made before signing
/// covers bytes that never ship. One flipped byte stands in for the appended
/// certificate.
#[test]
fn a_file_that_changed_after_it_was_minisigned_is_refused() {
    let s = Scratch::new("tampered");
    let mut bytes = std::fs::read(s.0.join("setup.exe")).expect("read");
    bytes[0] ^= 0x01;
    std::fs::write(s.0.join("setup.exe"), bytes).expect("write");
    let (ok, text) = s.run();
    assert!(!ok, "{text}");
    assert!(text.contains("does not cover"), "{text}");
}

#[test]
fn a_missing_signature_file_is_refused() {
    let s = Scratch::new("no-sig");
    std::fs::remove_file(s.0.join("setup.exe.sig")).expect("remove");
    let (ok, text) = s.run();
    assert!(!ok, "{text}");
    assert!(text.contains("setup.exe.sig"), "{text}");
}

#[test]
fn an_empty_signature_file_is_refused() {
    let s = Scratch::new("empty-sig");
    std::fs::write(s.0.join("setup.exe.sig"), "").expect("write");
    let (ok, text) = s.run();
    assert!(!ok, "{text}");
}

/// The key comes from the file the app compiles in. The real one did not sign
/// the test fixture, so verifying under it must fail: this is what a release
/// signed with the wrong `TAURI_SIGNING_PRIVATE_KEY` looks like.
#[test]
fn a_signature_from_a_different_key_than_the_shipped_one_is_refused() {
    let s = Scratch::new("foreign");
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/src-tauri/tauri.conf.json");
    std::fs::copy(shipped, s.0.join("tauri.conf.json")).expect("copy the shipped config");
    let (ok, text) = s.run();
    assert!(!ok, "{text}");
    assert!(text.contains("does not cover"), "{text}");
}

#[test]
fn every_flag_is_required() {
    let output = Command::new(BIN)
        .args(["--file", "x"])
        .output()
        .expect("run the binary");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("is required"));
}
