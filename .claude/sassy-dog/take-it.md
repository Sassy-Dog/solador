---
stack_summary: >
  Rust workspace (`crates/*`, `app/src-tauri`, and the `agent/` metrics daemon as a member) behind
  a Tauri v2 shell, with a bundler-free HTML/CSS/JS frontend in `app/ui` and a Playwright e2e suite
  in `tests/frontend`. Shell tooling lives in `scripts/` behind the `./dev` entry point.
preflight_commands: |
  ./dev test && ./dev lint
pr_template_sections: [Why, What, Verification]
merge_queue: true
review_site: agent
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

<!--
Repo-specific implementation rules for a take-it sub-agent go here. By convention the first rule is
numbered `4.`, continuing the take-it prompt's own steps 1-3, which live in the skill rather than in
this file. A list here that starts at 4 is COMPLETE: there are no items 1-3 in this file and nothing
is withheld (Sassy-Dog/skills#373). Nothing parses these numbers; they are a reading aid only.
-->

> 4. `./dev test && ./dev lint` is the gate pair and mirrors CI, `agent/` included. A clean
>    `cargo test` still fails CI if fmt, clippy, shellcheck or the secrets guard are off.
> 5. Never rename a job whose name appears in `.github/required-checks.yml`; the ruleset waits
>    forever on a context no job reports.
> 6. The Windows job gates every merge, but `./dev test` runs on this machine only. Avoid unix-only
>    path separators, permissions and process assumptions in `crates/*` and `app/src-tauri`.
> 7. Reconcile `AGENTS.md`, `app/README.md`, `agent/README.md` and `docs/` in the same PR
>    whenever the change makes a claim in them untrue.

## extra-guardrails
