use std::path::Path;

fn main() {
    emit_trusted_public_keys();
}

/// Compile the trust set in: the text of `agent/release-signing-key.pub` — the
/// key every agent release is signed under — and, **when the file exists**,
/// `agent/release-signing-key-next.pub`, the standby that makes a rotation a
/// release rather than a recall.
///
/// These are the AGENT's keys, never the app updater's (`tauri.conf.json`'s
/// `pubkey`): `agent-latest.json` is signed under the agent's keypair, which is
/// deliberately a different one. Modelled on `agent/build.rs`'s
/// `emit_trusted_public_keys`, which compiles the identical set into the agent
/// itself, so the cockpit and a host's updater accept exactly the same
/// signatures.
///
/// Written to `$OUT_DIR/trusted_keys.rs` as a `&[&str]` and `include!`d,
/// because `include_str!` has no "if present" form and the standby's *absence*
/// is a legitimate state of the tree. The current key being absent is not: a
/// build without it would trust nothing and report every feed as forged, so
/// that is a hard build failure with the path named.
///
/// Key text is embedded verbatim and decoded at run time by `minisign-verify`;
/// a unit test decodes the compiled-in set so a malformed file fails in CI.
fn emit_trusted_public_keys() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let agent_dir = Path::new(&manifest_dir).join("../../agent");
    let current = agent_dir.join("release-signing-key.pub");
    let next = agent_dir.join("release-signing-key-next.pub");

    // Re-run when either file changes, and when the optional one appears or
    // disappears. NOT `rerun-if-changed` on the absent path: cargo treats a
    // watched path that does not exist as always-dirty and re-runs this script
    // on every build (measured for `agent/build.rs`, which this follows).
    // Watching the directory is what catches the file being added or removed;
    // cargo scans it recursively, so an edit under `agent/` re-runs this
    // script. That is cheap, and it does NOT recompile this crate or anything
    // above it: the output file is only rewritten when its text changed, and
    // rustc's dep-info tracks that file's mtime.
    println!("cargo:rerun-if-changed={}", current.display());
    println!("cargo:rerun-if-changed={}", agent_dir.display());
    if next.exists() {
        println!("cargo:rerun-if-changed={}", next.display());
    }

    let current_text = std::fs::read_to_string(&current).unwrap_or_else(|e| {
        panic!(
            "agent/release-signing-key.pub is missing or unreadable ({e}). It is the \
             public half of the key every agent release is signed under, and the \
             cockpit's trust set is built from it — a build without it would trust \
             nothing. Restore the committed file; never substitute another key."
        )
    });
    let mut keys = vec![current_text];
    if next.exists() {
        let next_text = std::fs::read_to_string(&next)
            .unwrap_or_else(|e| panic!("{} exists but is unreadable: {e}", next.display()));
        keys.push(next_text);
    }

    let mut source = String::from(
        "/// The trust set, embedded by `build.rs`: the committed AGENT public key\n\
         /// file(s) verbatim. One entry until `agent/release-signing-key-next.pub`\n\
         /// is committed, two afterwards; never fetched, never read at run time.\n\
         pub const TRUSTED_PUBLIC_KEYS: &[&str] = &[\n",
    );
    for key in &keys {
        source.push_str("    ");
        source.push_str(&rust_string_literal(key));
        source.push_str(",\n");
    }
    source.push_str("];\n");

    let target = Path::new(&out_dir).join("trusted_keys.rs");
    if std::fs::read_to_string(&target).ok().as_deref() != Some(source.as_str()) {
        std::fs::write(&target, source).expect("writing trusted_keys.rs into OUT_DIR");
    }
}

/// A Rust string literal for arbitrary text, escaped rather than raw so a key
/// file containing `"#` (or anything else) can never break out of it.
fn rust_string_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
