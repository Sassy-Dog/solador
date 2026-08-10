---
stack_summary: >
  Rust workspace (`crates/*` + `app/src-tauri`) behind a Tauri v2 shell, with a plain HTML/CSS/JS
  frontend and no bundler (`app/ui`) — this is the app going forward and the only thing CI builds.
  `DevCanopy/` is a frozen SwiftUI + SwiftData macOS app, neither built, tested nor linted in CI.
  `agent/` is a separate Cargo workspace with its own `Cargo.lock`, `rust-toolchain.toml` and
  `Rust agent` CI job. Frontend e2e is Playwright in `tests/frontend`.
preflight_commands: |
  ./dev test && ./dev lint
pr_template_sections: [Summary, Changes, Testing]
merge_queue: true
board:
  number: TODO  # set when the board is created (runbook R8)
  owner: cpmadrid
  project_id: TODO
  status_field_id: TODO
  ready_option_id: TODO
  backlog_option_id: TODO
  in_progress_option_id: TODO
---

## subagent-rules

> **Pre-flight (extends step 4 below):** `./dev test && ./dev lint` is the gate pair. `./dev test`
> runs `cargo test --locked --workspace` plus the `tests/frontend` Playwright suite; `./dev lint`
> runs `cargo fmt --all -- --check` and `cargo clippy --locked --workspace --all-targets -- -D warnings`.
> A clean `cargo build`/`cargo test` still fails CI if formatting or lints are off.
> **`./dev build` is not a gate** — it builds the frozen SwiftUI app, which CI does not compile.
>
> - **The `main` protection ruleset requires exactly three check contexts:** `Rust workspace + frontend e2e`,
>   `Windows workspace tests`, and `Rust agent`. Never rename those jobs in `.github/workflows/ci.yml`.
>   **`Windows workspace tests` gates every merge**, so anything in `crates/*` or `app/src-tauri` must stay
>   Windows-portable — no unix-only path separators, file permissions, or process assumptions.
> - For `agent/` (Rust) changes: it is a **separate Cargo workspace**, so run its gates from inside `agent/`
>   (`cargo fmt --check && cargo clippy -- -D warnings && cargo test`). The root `./dev test`/`./dev lint`
>   do not touch it.
> - For CI / build-config changes (`.github/workflows/`, toolchain pins): pin tool versions to what a recent
>   **green** run actually used, verified in the run log rather than guessed.
> - `DevCanopy/` (SwiftUI) is frozen and has not been built by CI since 2026-08-04. Do not change it, and do
>   not treat a pre-existing Swift compile error there as your regression.
