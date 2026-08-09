---
pr_template_sections: [Summary, Changes, Testing]
preflight_commands: |
  ./dev test && ./dev lint
merge_queue: true
---

## extra-gates

**The gate pair** — `./dev test` runs `cargo test --locked --workspace` (the root workspace:
`crates/*` + `app/src-tauri`) plus the `tests/frontend` Playwright e2e suite. `./dev lint` runs
`cargo fmt --all -- --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings`,
which is exactly what CI's `rust-workspace` job runs — there is no separate `Lint` job any more.
The pre-push hook (`./Scripts/install-hooks.sh`) also runs the lint half automatically, but running
it in pre-flight fails fast with a clearer message.

**`./dev build` is deliberately NOT in the pre-flight** — it builds `DevCanopy/`, the frozen
SwiftUI app, which CI has not compiled since 2026-08-04. A Swift compile break can therefore sit on
`main` unnoticed, so including it would fail pre-flight for reasons unrelated to the change under
test — and a failure nobody can attribute is worse than a gate that isn't run.

**Windows portability gate** — `Windows workspace tests` (`windows-latest`) is a required check on
`main`, so anything touching `crates/*` or `app/src-tauri` must avoid unix-only path separators,
file permissions, and process assumptions. `./dev test` covers this machine only; the Windows leg is
CI-only, so a green local run does not clear it.

**`agent/` is a separate Cargo workspace** — the root `./dev test`/`./dev lint` do not touch it. For
`agent/` changes, run its gates from inside `agent/`:
`cargo fmt --check && cargo clippy -- -D warnings && cargo test`. Its CI job is `Rust agent`, on a
self-hosted linux runner.

**Swift-only legacy**, should a change ever touch `DevCanopy/`: run
`./Scripts/generate-project.sh` after adding, removing, or renaming `.swift` files (XcodeGen won't
pick them up otherwise), and re-point renamed files' `lint-baseline.json` entries — the baseline is
path-keyed, so a rename un-baselines its violations. Note that nothing in CI runs SwiftLint or
SwiftFormat any more; `lint-baseline.json` and `.swiftlint.yml` remain in the tree unused.
