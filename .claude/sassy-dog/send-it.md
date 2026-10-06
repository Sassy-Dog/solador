---
pr_template_path: ""
pr_template_sections: [Why, What, Verification]
preflight_commands: |
  ./dev test && ./dev lint
merge_queue: true
---

## extra-gates

**The gate pair.** `./dev test` runs the root workspace tests (`agent/` included), the deploy,
versioning and run-helper shell suites, the e2e server's bind test, and the Playwright suite.
`./dev lint` runs fmt, clippy, shellcheck, the secrets guard with its mutation corpus, the
agent-deps guard, the Tauri CLI pin guard, and the `.claude` tracking guard (the last two with
their negative controls). Together they mirror CI's lint and test gates.

**CI-only legs.** `Windows workspace tests` and `macOS bundle (unsigned)` are required checks that
a local run does not cover, so a green `./dev test` does not clear them.

## extra-guardrails
