fn main() {
    // Publishes the git-derived CalVer as `SOLADOR_MARKETING_VERSION`, or
    // publishes nothing when it cannot be derived honestly (a shallow clone, a
    // non-git tree). `settings::VERSION` reads it through `option_env!`, so the
    // absence is a state About renders as `—` rather than a stand-in number.
    //
    // The logic lives in `crates/buildversion` because `agent/build.rs` needs
    // exactly the same thing (#390) and a second copy would diverge at the
    // first fix. It computes nothing: `scripts/get-version-info.sh` owns the
    // CalVer algorithm (docs/VERSIONING.md).
    buildversion::emit_marketing_version();
    tauri_build::build()
}
