//! The `solador-agent-feed` binary, exercised as the workflow runs it — over a
//! directory of release-shaped files — rather than through the library.
//!
//! The library tests prove the verifier; these prove the part only the binary
//! does: discovering inputs on disk, refusing a directory that is not exactly
//! the four signed binaries, and writing nothing on any refusal. `build` is
//! held to reproducing the committed `agent-latest.json` byte for byte, so
//! the CLI, the library and the signed fixture are one document.

use std::path::{Path, PathBuf};
use std::process::Command;

use updatefeed::agent::{self, TARGETS};

const BIN: &str = env!("CARGO_BIN_EXE_solador-agent-feed");
const VERSION: &str = "2026.9.9";
const TAG: &str = "v2026.9.9";
const BASE: &str = "https://github.com/Sassy-Dog/solador/releases/download";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/agent")
}

fn shipped_pubkey() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agent/release-signing-key.pub")
}

/// A fresh directory under the target dir, unique per test, holding a copy of
/// the fixture assets that a test may then damage.
struct Scratch(PathBuf);

impl Scratch {
    fn with_assets(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("agent-feed-cli-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).expect("scratch dir");
        for t in TARGETS {
            for suffix in ["", ".minisig"] {
                let file = format!("solador-agent-{VERSION}-{t}{suffix}");
                std::fs::copy(fixtures().join(&file), dir.join("assets").join(&file))
                    .expect("copy fixture");
            }
        }
        Scratch(dir)
    }
    fn assets(&self) -> PathBuf {
        self.0.join("assets")
    }
    fn out(&self) -> PathBuf {
        self.0.join("agent-latest.json")
    }
}

fn build(scratch: &Scratch, pubkey: &Path, version: &str, tag: &str, base: &str) -> (bool, String) {
    let output = Command::new(BIN)
        .args([
            "build",
            "--version",
            version,
            "--tag",
            tag,
            "--asset-dir",
            scratch.assets().to_str().unwrap(),
            "--download-base",
            base,
            "--pubkey",
            pubkey.to_str().unwrap(),
            "--out",
            scratch.out().to_str().unwrap(),
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

fn verify(args: &[&str]) -> (bool, String) {
    let output = Command::new(BIN)
        .arg("verify")
        .args(args)
        .output()
        .expect("run the binary");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), text)
}

fn refused(scratch: &Scratch, (ok, text): (bool, String), needle: &str) {
    assert!(!ok, "expected a refusal, got success:\n{text}");
    assert!(
        text.contains("::error::agent feed refused: "),
        "refusals are workflow errors:\n{text}"
    );
    assert!(text.contains(needle), "expected '{needle}' in:\n{text}");
    assert!(
        !scratch.out().exists(),
        "a refusal must write nothing, but {} exists",
        scratch.out().display()
    );
}

#[test]
fn build_reproduces_the_committed_feed_byte_for_byte() {
    let scratch = Scratch::with_assets("reproduce");
    let (ok, text) = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    assert!(ok, "{text}");
    // The fixture key's id, read from the fixture rather than pinned, so a
    // regenerated fixture set (tests/fixtures/README.md) fails only on the
    // byte comparison below — with its message about regeneration — and not
    // here, on a literal that says nothing about why.
    let key_id =
        agent::key_id(&std::fs::read_to_string(fixtures().join("test-agent-key.pub")).unwrap())
            .expect("the fixture key decodes");
    assert!(
        text.contains(&format!("4 binaries verified under agent key {key_id}")),
        "{text}"
    );
    let written = std::fs::read(scratch.out()).expect("the feed was written");
    let committed = std::fs::read(fixtures().join("agent-latest.json")).expect("fixture");
    assert_eq!(written, committed, "the CLI's output is the signed fixture");
}

#[test]
fn a_trailing_slash_on_the_download_base_is_tolerated() {
    let scratch = Scratch::with_assets("trailing-slash");
    let (ok, text) = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        &format!("{BASE}/"),
    );
    assert!(ok, "{text}");
    let written = std::fs::read(scratch.out()).expect("written");
    let committed = std::fs::read(fixtures().join("agent-latest.json")).expect("fixture");
    assert_eq!(written, committed);
}

#[test]
fn verify_accepts_the_committed_pair_and_checks_every_binary() {
    let scratch = Scratch::with_assets("verify-ok");
    let (ok, text) = verify(&[
        "--feed",
        fixtures().join("agent-latest.json").to_str().unwrap(),
        "--signature",
        fixtures()
            .join("agent-latest.json.minisig")
            .to_str()
            .unwrap(),
        "--pubkey",
        fixtures().join("test-agent-key.pub").to_str().unwrap(),
        "--version",
        VERSION,
        "--asset-dir",
        scratch.assets().to_str().unwrap(),
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("4 binaries checked against their entries"));
}

#[test]
fn a_binary_without_its_minisig_is_refused() {
    let scratch = Scratch::with_assets("no-minisig");
    std::fs::remove_file(scratch.assets().join(format!(
        "solador-agent-{VERSION}-aarch64-apple-darwin.minisig"
    )))
    .unwrap();
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    refused(&scratch, r, "has no .minisig beside it");
}

#[test]
fn a_missing_target_is_refused_and_a_stale_out_file_does_not_survive_it() {
    let scratch = Scratch::with_assets("missing-target");
    for suffix in ["", ".minisig"] {
        std::fs::remove_file(scratch.assets().join(format!(
            "solador-agent-{VERSION}-x86_64-unknown-linux-musl{suffix}"
        )))
        .unwrap();
    }
    // An earlier run's output at --out must not outlive this run's refusal
    // and then read as verified output; `refused` asserts it is gone.
    std::fs::write(scratch.out(), b"{\"stale\": true}\n").unwrap();
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    refused(
        &scratch,
        r,
        "1 target(s) missing: x86_64-unknown-linux-musl",
    );
}

#[test]
fn a_fifth_triple_in_the_directory_is_refused_rather_than_skipped() {
    let scratch = Scratch::with_assets("fifth-triple");
    for suffix in ["", ".minisig"] {
        std::fs::copy(
            scratch.assets().join(format!(
                "solador-agent-{VERSION}-x86_64-apple-darwin{suffix}"
            )),
            scratch.assets().join(format!(
                "solador-agent-{VERSION}-x86_64-pc-windows-msvc{suffix}"
            )),
        )
        .unwrap();
    }
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    refused(
        &scratch,
        r,
        "'x86_64-pc-windows-msvc' is not a published agent target",
    );
}

#[test]
fn a_tampered_binary_is_refused() {
    let scratch = Scratch::with_assets("tampered");
    let path = scratch.assets().join(format!(
        "solador-agent-{VERSION}-aarch64-unknown-linux-musl"
    ));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 0x01;
    std::fs::write(&path, bytes).unwrap();
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    refused(&scratch, r, "signature does not cover its bytes");
}

#[test]
fn the_shipped_key_refuses_fixtures_it_did_not_sign() {
    let scratch = Scratch::with_assets("wrong-key");
    let r = build(&scratch, &shipped_pubkey(), VERSION, TAG, BASE);
    refused(&scratch, r, "signature does not cover its bytes");
}

#[test]
fn an_empty_directory_is_refused_as_a_release_without_agent_assets() {
    let scratch = Scratch::with_assets("empty");
    std::fs::remove_dir_all(scratch.assets()).unwrap();
    std::fs::create_dir_all(scratch.assets()).unwrap();
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    refused(
        &scratch,
        r,
        "a release without agent assets has no agent feed",
    );
}

#[test]
fn a_version_the_contract_refuses_is_refused_as_a_version() {
    let scratch = Scratch::with_assets("bad-version");
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        "2026.09.9",
        "v2026.09.9",
        BASE,
    );
    refused(&scratch, r, "is not the CalVer");
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        "v2026.9.8",
        BASE,
    );
    refused(&scratch, r, "does not name version");
}

#[test]
fn an_empty_flag_value_is_refused_like_a_missing_one() {
    let scratch = Scratch::with_assets("empty-flag");
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        "",
        TAG,
        BASE,
    );
    refused(&scratch, r, "--version was given an empty value");
}

#[test]
fn verify_refuses_edited_bytes_a_foreign_key_and_a_wrong_version() {
    let scratch = Scratch::with_assets("verify-neg");
    let feed = fixtures().join("agent-latest.json");
    let sig = fixtures().join("agent-latest.json.minisig");
    let key = fixtures().join("test-agent-key.pub");

    // One character moved after signing.
    let edited = scratch.0.join("edited.json");
    let mut bytes = std::fs::read(&feed).unwrap();
    let idx = bytes.iter().position(|b| *b == b'{').unwrap();
    bytes.insert(idx + 1, b' ');
    std::fs::write(&edited, bytes).unwrap();
    let (ok, text) = verify(&[
        "--feed",
        edited.to_str().unwrap(),
        "--signature",
        sig.to_str().unwrap(),
        "--pubkey",
        key.to_str().unwrap(),
    ]);
    assert!(
        !ok && text.contains("signature does not cover its bytes"),
        "{text}"
    );

    let (ok, text) = verify(&[
        "--feed",
        feed.to_str().unwrap(),
        "--signature",
        sig.to_str().unwrap(),
        "--pubkey",
        shipped_pubkey().to_str().unwrap(),
    ]);
    assert!(
        !ok && text.contains("signature does not cover its bytes"),
        "{text}"
    );

    let (ok, text) = verify(&[
        "--feed",
        feed.to_str().unwrap(),
        "--signature",
        sig.to_str().unwrap(),
        "--pubkey",
        key.to_str().unwrap(),
        "--version",
        "2026.9.8",
    ]);
    assert!(
        !ok && text.contains("describes a different release"),
        "{text}"
    );

    // A binary in --asset-dir that is not the one the entry describes.
    let path = scratch
        .assets()
        .join(format!("solador-agent-{VERSION}-x86_64-apple-darwin"));
    let mut b = std::fs::read(&path).unwrap();
    b.push(b'x');
    std::fs::write(&path, b).unwrap();
    let (ok, text) = verify(&[
        "--feed",
        feed.to_str().unwrap(),
        "--signature",
        sig.to_str().unwrap(),
        "--pubkey",
        key.to_str().unwrap(),
        "--asset-dir",
        scratch.assets().to_str().unwrap(),
    ]);
    assert!(
        !ok && text.contains("signature does not cover its bytes"),
        "{text}"
    );
}

/// Refused as a usage error — the `::error::agent feed refused:` sentence,
/// exit 1 — and not by a panic, which is also a non-zero exit.
fn refused_usage(args: &[&str], needle: &str) {
    let output = Command::new(BIN).args(args).output().unwrap();
    let text = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{args:?}: {text}");
    assert!(
        text.contains("::error::agent feed refused: ") && text.contains(needle),
        "{args:?}: expected '{needle}' in:\n{text}"
    );
}

#[test]
fn an_unknown_command_or_flag_is_refused() {
    refused_usage(&["frobnicate"], "unknown command 'frobnicate'");
    refused_usage(&["build", "--bogus", "x"], "unknown argument --bogus");
    refused_usage(&[], "expected a command");
    refused_usage(&["build", "--version"], "--version needs a value");
    refused_usage(&["verify", "--feed", "x"], "--signature is required");
}

#[test]
fn a_missing_asset_directory_is_an_io_error_not_a_missing_target() {
    let scratch = Scratch::with_assets("missing-dir");
    std::fs::remove_dir_all(scratch.assets()).unwrap();
    let r = build(
        &scratch,
        &fixtures().join("test-agent-key.pub"),
        VERSION,
        TAG,
        BASE,
    );
    // An I/O error must read as one — never as "target missing", which sends
    // whoever reads it to the release rather than to the disk.
    assert!(
        !r.1.contains("target(s) missing"),
        "an unreadable directory was reported as a release problem:\n{}",
        r.1
    );
    refused(&scratch, r, "could not read");
}

#[test]
fn verify_refuses_when_a_binary_named_by_the_feed_is_absent_from_the_asset_dir() {
    let scratch = Scratch::with_assets("verify-missing-binary");
    std::fs::remove_file(
        scratch
            .assets()
            .join(format!("solador-agent-{VERSION}-aarch64-apple-darwin")),
    )
    .unwrap();
    let (ok, text) = verify(&[
        "--feed",
        fixtures().join("agent-latest.json").to_str().unwrap(),
        "--signature",
        fixtures()
            .join("agent-latest.json.minisig")
            .to_str()
            .unwrap(),
        "--pubkey",
        fixtures().join("test-agent-key.pub").to_str().unwrap(),
        "--asset-dir",
        scratch.assets().to_str().unwrap(),
    ]);
    assert!(!ok, "{text}");
    assert!(
        text.contains("::error::agent feed refused: could not read"),
        "{text}"
    );
}
