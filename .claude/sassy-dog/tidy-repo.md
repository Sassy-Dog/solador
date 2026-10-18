---
dep_version_globs: ["Cargo.toml", "**/Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "tests/frontend/package.json", "tests/frontend/package-lock.json"]
noise_allowlist: ["tests/frontend/test-results/", "tests/frontend/node_modules/", "app/src-tauri/gen/", ".DS_Store"]
never_discard: [".env*", ".envrc.local", "*.pem", "*.p8", "*.p12", "*.key", "*.minisig"]
---

## extra-cleanup

Run `./dev clean --over-budget` from the repo root and report its line. It
clears `target/` only when it is over `TARGET_BUDGET_GB` (`scripts/config.sh`),
which is above one debug plus one release build: cargo never prunes old
dependency versions, so without this the directory only grows. Under the budget
it leaves `target/` alone, so do not discard `target/` through the noise
allowlist either; this step owns it. Skip it while a build, test run or
`./dev publish` is running in this checkout.

## extra-guardrails

- `target/` is not untracked-file noise: leave it out of the untracked sweep and
  its Ask prompt. The extra-cleanup step above is the only thing that removes it.
