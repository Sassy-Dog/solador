---
name: take-it
description: >
  Parallel issue-shipping for DevCanopy. The user names one or more GitHub issue numbers;
  dispatch one sub-agent per issue (each in its own git worktree), implement the fix, open a PR
  with Closes #N, and a coordinator loop polls to auto-merge greens and surface failures. Use when
  the user says "take #341, #432", "take #N", "take it #N", "go take #N and #M", "pick up #N",
  "knock out #N", or any variant handing over a list of GitHub issue numbers to ship in parallel.
  DevCanopy-specific
---

<!-- generated-by: ai-agent-skills:refresh-sassydog-skills | template: take-it | template-version: 3 -->

# DevCanopy Take-It

Parallel issue-shipping: the user hands you GitHub issue numbers; this skill ships them concurrently. It's the *executor* — it assumes the user already knows what they want shipped and does not re-prioritize.

## 1. Parse the issue list

**The numbers are ALWAYS GitHub issue numbers, NEVER list positions.** "take 218 219" means `gh issue view 218` and `gh issue view 219`. Echo the resolved list before dispatching: "Taking #218, #219 — 2 sub-agents." Empty or ambiguous input ("do the easy ones") → STOP and ask. Cap at **5 sub-agents per dispatch**; queue the rest for the next round.

### Pre-flight smell test

| Pattern in title or body | Action |
|---|---|
| Title starts with `Assess`, `Investigate`, `Evaluate`, `Spike:`, `Decide:` | Flag — research doc, not implementation; confirm before dispatching |
| Body is a batch checklist of many independent sub-items | Flag — dispatch as ONE PR or a coherent subset; confirm intent |
| Body contains `## Open questions` / `## Decision criteria` | Flag — decision not yet made |

## 2. Pre-flight per issue

```bash
gh issue view N --repo Sassy-Dog/devcanopy --json number,title,state,labels,body,assignees
```

Skip + announce if: not OPEN; `blocked` label; assignee already set or board card already In progress/In review; or stub body (< 80 chars) — but **check `gh issue view N --comments` before calling it a stub**; scope may live in a follow-up comment. For survivors capture title, body, labels (label → conventional-commit prefix: `bug`→`fix`, `enhancement`→`feat`, `documentation`→`docs`, else `chore`).

## 3. Claim each issue

Best-effort, so parallel sessions don't double-pick: `gh issue edit N --repo Sassy-Dog/devcanopy --add-assignee @me`, and move the board card to In progress per `ai-agent-skills:github-issues` (`references/board-graphql.md`; board 5, IDs: project `PVT_kwDODSBhws4BaqCG`, status field `PVTSSF_lADODSBhws4BaqCGzhVgAc8`, In progress `d04a8f33`). Claim failures are logged, never fatal — the PR's `Closes #N` lands the card on Done regardless.

## 4. Dispatch sub-agents in parallel

**First, fast-forward local main** — worktrees branch from local HEAD, not origin; a stale base lands the PR `CONFLICTING`:

```bash
git fetch origin main --quiet
git switch main >/dev/null 2>&1 && git pull --ff-only origin main
```

**Issue ALL Agent calls in a single message** with `isolation: "worktree"`. **Record the batch manifest** as results return — `{issue, pr, worktreePath, worktreeBranch}`, written somewhere durable (e.g. `.git/take-it-batch.json`) so a crashed coordinator's worktrees stay reclaimable.

**Sub-agent prompt template** (self-contained — the agent has zero conversation context):

> You are shipping GitHub issue **#{N}** in the DevCanopy repo (Swift 5.9+/SwiftUI macOS app, SwiftData, XcodeGen-generated project — after adding/removing/renaming .swift files you MUST run `./Scripts/generate-project.sh` before building; local Swift package in `Packages/HostMetricsKit`; Rust remote agent in `agent/`).
>
> **Issue title:** {title} · **Labels:** {labels}
> **Issue body:**
>
> ```
> {body}
> ```
>
> **Your job:**
>
> 1. **Stay inside your assigned worktree.** cwd resets between Bash calls — prefix every call with `cd <your worktree path> &&`, and verify `pwd && git rev-parse --show-toplevel && git branch --show-current` before your first edit. **Never `git stash`** (worktrees share one `.git`; a stash collides with the other parallel agents). Commit WIP to your branch or discard explicitly. **Never run an editable/dev install into a shared interpreter or global store** — under parallel worktree agents, whoever installs last repoints imports for everyone, so a green test run may silently be testing another agent's source (Python: `pip install -e` writes the editable link into the shared interpreter's `site-packages`; Node: `npm link`/global installs are the same trap). Work isolated: create a throwaway venv/env *inside your worktree* (and never commit it), or run against your tree without installing (`PYTHONPATH=src …` for pure-Python). Verify the import resolves inside YOUR worktree before trusting a green run.
> 2. Read the issue carefully. If scope is genuinely unclear after the body and linked issues/PRs, STOP and report back — do not guess.
> 3. Implement the change following the repo's `CLAUDE.md`.
<!-- BEGIN PROJECT-SPECIFIC: subagent-rules -->
> **Language-specific pre-flight (extends step 4 below — `./dev build && ./dev test` alone is NOT enough):**
> - For `agent/` (Rust) changes, pre-flight MUST include `cargo fmt --check && cargo clippy -- -D warnings` — CI has a Format-check + Clippy gate, so a clean `cargo build`/`cargo test` still fails CI if formatting/lints are off.
> - For CI / build-config changes (`.github/workflows/`, toolchain pins): pin tool versions to what a recent **green** run actually used (verify in the run log — e.g. Xcode is `latest-stable` = a 26.x today, NOT a guessed 16.x), and never rename the `Swift app tests` / `Rust agent` job names — the `main` branch-protection ruleset requires those exact check contexts.
<!-- END PROJECT-SPECIFIC -->
> 5. Run the send-it pre-flight locally and fix anything red: `./dev build && ./dev test`
> 6. Commit on branch `{prefix}/issue-{N}-{slug}` with a conventional-commit message containing a literal `Closes #{N}` line.
> 7. Push and open a PR per the send-it template — the body MUST contain `Closes #{N}` on its own line (Summary / Changes / Testing sections).
> 8. **Do NOT merge.** Report back: `RESULT: pr=<N> branch=<name> status=<opened|skipped|failed> note=<one-line>`

## 5. Coordinator: watch + merge (delegated)

Use the plugin capability skill for ALL polling/merge/teardown mechanics — do NOT reimplement them inline:

Skill: `ai-agent-skills:pr-shepherd`
Args: "Watch PRs <numbers from the RESULT lines> in Sassy-Dog/devcanopy. Merge policy: MERGE QUEUE — enqueue greens with `gh pr merge --auto` (no method flag, no --delete-branch), confirm isInMergeQueue, handle ejects. After all PRs are terminal, tear down these worktrees: <paths from the batch manifest>, then reconcile local main."

If `ai-agent-skills:pr-shepherd` is not in your available skills, STOP and tell the user to install the plugin (`claude plugin install ai-agent-skills`) — do not improvise the merge loop from memory.

Run the coordinator synchronously; backgrounding it orphans PRs at "checks pending".

## 6. Final report

| Issue | PR | Status | Notes |
|-------|----|--------|-------|
| #218 | #260 | ✅ MERGED | one-line summary |
| #240 | #261 | ⚠️ FAILED | named failing check + log excerpt |
| #216 | — | ⏭ SKIPPED | reason |

Always end with: claims to unwind by hand (assignments, board cards for unshipped issues) and a next-action one-liner per failure.

## Guardrails

- **Single-writer**: sub-agents never merge/enqueue; only the coordinator does, and only for green PRs.
- **Never auto-rebase a CONFLICTING PR** — surface it.
- Cap parallelism at 5. Don't dispatch on stubs or `blocked` issues.

<!-- BEGIN PROJECT-SPECIFIC: extra-guardrails -->
<!-- END PROJECT-SPECIFIC -->
