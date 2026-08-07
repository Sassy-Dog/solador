---
stack_summary: Swift 5.9+/SwiftUI macOS app, SwiftData, XcodeGen-generated project — after adding/removing/renaming .swift files you MUST run `./Scripts/generate-project.sh` before building; local Swift package in `Packages/HostMetricsKit`; Rust remote agent in `agent/`
preflight_commands: |
  ./dev build && ./dev test && ./dev lint
pr_template_sections: [Summary, Changes, Testing]
merge_queue: true
board:
  number: 5
  owner: Sassy-Dog
  project_id: PVT_kwDODSBhws4BaqCG
  status_field_id: PVTSSF_lADODSBhws4BaqCGzhVgAc8
  ready_option_id: 8dcb24a9
  backlog_option_id: 906f24bb
  in_progress_option_id: d04a8f33
---

## subagent-rules

> **Language-specific pre-flight (extends step 4 below — `./dev build && ./dev test` alone is NOT enough):**
> - For `agent/` (Rust) changes, pre-flight MUST include `cargo fmt --check && cargo clippy -- -D warnings` — CI has a Format-check + Clippy gate, so a clean `cargo build`/`cargo test` still fails CI if formatting/lints are off.
> - For CI / build-config changes (`.github/workflows/`, toolchain pins): pin tool versions to what a recent **green** run actually used (verify in the run log — e.g. Xcode is `latest-stable` = a 26.x today, NOT a guessed 16.x), and never rename the `Swift app tests` / `Rust agent` job names — the `main` branch-protection ruleset requires those exact check contexts.
