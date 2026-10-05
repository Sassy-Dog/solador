# Change-based CI

`.github/workflows/ci.yml` starts on every PR to `main`, main push, and merge
queue group. A manual dispatch runs the full suite. `scripts/ci_changes.py`
selects jobs and steps from the complete Git diff; it never uses GitHub's
changed-files API or workflow-level path filters.

| Changed area | Selected work |
| --- | --- |
| Recognized documentation only | Selection tests and security/dependency guards; no compilation |
| `app/ui/`, `tests/frontend/` | Rust view-model parity tests, frontend server tests and Playwright on macOS, including the Rust fixture generator |
| `agent/` outside `deploy/`, agent build script | Agent Rust on Linux, macOS and Windows; Linux static musl build; build script changes also select helpers |
| `agent/deploy/`, explicit agent signing/feed helpers | Shell lint, deploy/versioning/feed/signing helper suites on Linux/macOS; versioning on Windows |
| `app/src-tauri/`, app build/run/icon helpers | Workspace Rust on macOS/Windows, frontend tests, unsigned universal macOS bundle; helper script changes also select helpers |
| `crates/` | Workspace Rust, frontend and macOS bundle; also Linux agent/musl when the crate is in the agent's local dependency graph |
| Shared inputs, agent signing public keys, workflows, selection policy, or an unrecognized path | Full suite |

Mixed changes take the union. Shared inputs include `Cargo.toml`, `Cargo.lock`,
Rust toolchain/configuration, `dev`, `prd`, and scripts not explicitly assigned
to a narrower area. The classifier is the authoritative path list. It follows
local normal, build, dev and target-specific Cargo dependencies transitively,
including inherited workspace dependencies. New or unreadable paths fail toward
more testing, never less.

For PRs the diff starts at the merge base; queue groups compare their base and
head, covering every queued PR together; main pushes compare `before` and
`after`. Deleted paths and both sides of renames count. Missing commits,
incomplete history or an unreadable needed dependency graph select everything.
The selection job fetches full history with a blob filter and writes its choices
to the Actions run summary.

All five existing required check names remain unchanged. An unneeded job skips
through a job condition, which GitHub accepts for required checks. The required
**Secrets guard** uses `always()` and rejects selector failure or missing/invalid
outputs, then runs the selection regression suite and the existing guards.
Thus a broken selector cannot merge through skipped dependent jobs. No branch
rules or merge protections are relaxed. Selected tests retain their existing
commands and required-tool checks; agent-only Mac/Windows runs narrow Cargo to
`-p solador-agent`.

GitHub-managed default CodeQL is separate and remains enabled. Release workflows
are unchanged. Local `./dev test` and `./dev lint` still run their full suites,
including the selector tests (Python 3.11+ and Git, no third-party packages).
Run the selector tests alone with:

```sh
python3 -B -m unittest -v scripts.ci_changes_test
```
