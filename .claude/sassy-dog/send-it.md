---
pr_template_sections: [Summary, Changes, Testing]
preflight_commands: |
  ./dev build && ./dev test && ./dev lint
merge_queue: true
---

## extra-gates

**Lint gate** — `./dev lint` mirrors CI's Lint job (`swiftlint --strict` against `lint-baseline.json` + `swiftformat --lint`). The pre-push hook (`./Scripts/install-hooks.sh`) also runs it automatically, but running it in pre-flight fails fast with a clearer message. When `.swift` files were renamed, re-point their `lint-baseline.json` entries — the baseline is path-keyed, so a rename un-baselines its violations.

**XcodeGen regen gate** — if `.swift` files were added, removed, or renamed, run `./Scripts/generate-project.sh` before the pre-flight; `./dev build`/`./dev test` won't pick up new source files without regenerating the XcodeGen project.
