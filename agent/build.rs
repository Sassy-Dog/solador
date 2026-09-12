use std::path::Path;

fn main() {
    // The agent ships as four published binaries (#390), so it needs the same
    // git-derived CalVer the cockpit carries — and from the same place, since
    // `docs/VERSIONING.md` allows the algorithm exactly one home. `--version`
    // and `/v1/health` read it back through `option_env!`, and its absence is
    // a state both of them render rather than paper over.
    buildversion::emit_marketing_version();

    emit_trusted_public_keys();
}

/// Compile the updater's trust set in (#393): the text of
/// `agent/release-signing-key.pub` — the key every release is signed under
/// today — and, **when the file exists**, `agent/release-signing-key-next.pub`,
/// the standby that makes a rotation a release rather than a recall.
///
/// Written to `$OUT_DIR/trusted_keys.rs` as a `&[&str]` and `include!`d by
/// `src/update.rs`, because `include_str!` has no "if present" form and the
/// standby's *absence* is a legitimate state of the tree (it is the state this
/// PR ships in; `scripts/agent-standby-key.sh` is what adds the file). The
/// current key being absent is not: a build without it would produce an
/// updater that trusts nothing and reports every feed as forged, so that is a
/// hard build failure with the path named.
///
/// The key text is embedded verbatim and decoded at run time by
/// `minisign-verify`; a malformed file fails there, on every `update`, and a
/// unit test in `src/update.rs` decodes the compiled-in set so the failure
/// lands in CI first.
fn emit_trusted_public_keys() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let current = Path::new(&manifest_dir).join("release-signing-key.pub");
    let next = Path::new(&manifest_dir).join("release-signing-key-next.pub");

    // Re-run when either file changes, and when the optional one appears or
    // disappears. NOT `rerun-if-changed` on the absent path: cargo treats a
    // watched path that does not exist as always-dirty and re-runs this
    // script — and rustc — on every build (measured). Watching the crate
    // directory is what catches the file being added or removed; cargo
    // scans a watched directory recursively, so any edit under `agent/`
    // re-runs this script (cheap: two file reads and the version scripts)
    // and recompiles the crate — which an edit under `agent/` does anyway.
    // A leaf directory holding only the two `.pub` files would be tighter
    // and is a coordinated rename (`scripts/config.sh`, workflows, docs) for
    // another day.
    println!("cargo:rerun-if-changed={}", current.display());
    println!("cargo:rerun-if-changed={manifest_dir}");
    if next.exists() {
        println!("cargo:rerun-if-changed={}", next.display());
    }

    let current_text = std::fs::read_to_string(&current).unwrap_or_else(|e| {
        panic!(
            "agent/release-signing-key.pub is missing or unreadable ({e}). It is the \
             public half of the key every agent release is signed under, and the \
             updater's trust set is built from it — a build without it would trust \
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
        "/// The updater's trust set, embedded by `build.rs`: the committed public\n\
         /// key file(s) verbatim. One entry until `agent/release-signing-key-next.pub`\n\
         /// is committed, two afterwards; never fetched, never read at run time.\n\
         pub const TRUSTED_PUBLIC_KEYS: &[&str] = &[\n",
    );
    for key in &keys {
        source.push_str("    ");
        source.push_str(&rust_string_literal(key));
        source.push_str(",\n");
    }
    source.push_str("];\n");
    std::fs::write(Path::new(&out_dir).join("trusted_keys.rs"), source)
        .expect("writing trusted_keys.rs into OUT_DIR");
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
