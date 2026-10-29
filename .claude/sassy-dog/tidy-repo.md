---
dep_version_globs: ["Cargo.toml", "**/Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "tests/frontend/package.json", "tests/frontend/package-lock.json"]
noise_allowlist: [".DS_Store"]
never_discard: [".env*", ".envrc.local", "*.pem", "*.p8", "*.p12", "*.key", "*.minisig"]
---

## extra-guardrails

- This tidy is for worktrees, branches and confirming every piece of work is
  committed and pushed. Caches and build output are not untracked-file noise:
  leave `target/`, every `node_modules/`, `__pycache__/`, Playwright's
  `test-results/` and `app/src-tauri/gen/` out of the untracked sweep and its
  Ask prompt, including where the plugin's universal allowlist matches them.
  `./dev clean` is the operator's to run, never the tidy's.
