---
gotcha_summary: >
  The Tauri/Rust app (`crates/*`, `app/`) is the app going forward and the only thing CI builds;
  `DevCanopy/` (SwiftUI) is frozen — neither built, tested nor linted in CI, so leave it alone and
  do not assume it compiles. Gates are `./dev test` (`cargo test --locked --workspace` plus the
  `tests/frontend` Playwright suite) and `./dev lint` (`cargo fmt --all -- --check` plus
  `cargo clippy --locked --workspace --all-targets -- -D warnings`); `./dev build` builds the frozen
  Swift app and is NOT a gate. Run the app with `./dev run --tauri` — a bare `cargo build` launched
  directly is unsigned and re-prompts the macOS Keychain for every stored credential. The `main`
  ruleset requires exactly three contexts — `Rust workspace + frontend e2e`, `Windows workspace
  tests`, `Rust agent` — so Rust changes must stay Windows-portable. `agent/` is a separate Cargo
  workspace (its own `Cargo.lock` and `rust-toolchain.toml`, its own CI job) that the root
  `./dev test`/`./dev lint` do not touch, and it must be redeployed to `ubu-3xdv` after changes.
  Swift-only legacy, should a change ever touch `DevCanopy/`: run `./Scripts/generate-project.sh`
  after adding/removing/renaming `.swift` files (XcodeGen), `Packages/HostMetricsKit` is a local
  SwiftPM package, and Debug signs with team 52YMXC3348.
board:
  number: 5
  owner: Sassy-Dog
  project_id: PVT_kwDODSBhws4BaqCG
  status_field_id: PVTSSF_lADODSBhws4BaqCGzhVgAc8
  ready_option_id: 8dcb24a9
  backlog_option_id: 906f24bb
  in_progress_option_id: d04a8f33
---
