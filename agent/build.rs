fn main() {
    // The agent ships as four published binaries (#390), so it needs the same
    // git-derived CalVer the cockpit carries — and from the same place, since
    // `docs/VERSIONING.md` allows the algorithm exactly one home. `--version`
    // and `/v1/health` read it back through `option_env!`, and its absence is
    // a state both of them render rather than paper over.
    buildversion::emit_marketing_version();
}
