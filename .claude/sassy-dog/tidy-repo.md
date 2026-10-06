---
dep_version_globs: ["Cargo.toml", "**/Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "tests/frontend/package.json", "tests/frontend/package-lock.json"]
noise_allowlist: ["target/", "tests/frontend/test-results/", "tests/frontend/node_modules/", "app/src-tauri/gen/", ".DS_Store"]
never_discard: [".env*", ".envrc.local", "*.pem", "*.p8", "*.p12", "*.key", "*.minisig"]
---

## extra-cleanup

## extra-guardrails
