---
name: drain-it
description: >
  Loop-driven dispatcher for DevCanopy: each invocation is one idempotent tick that
  reconciles in-flight PRs, then tops the pipeline back up to 5 concurrent
  issues pulled ONLY from the board's Ready column, respecting dependencies and
  migration/codegen sequencing, until Ready is empty. Designed to run under
  "/loop 5m /drain-it" but a single manual invocation is also valid. Use when the user says
  "drain it", "drain the backlog", "work through Ready", "keep shipping until Ready is empty",
  or invokes it via /loop. DevCanopy-specific
---

<!-- generated-by: ai-agent-skills:create-dev-workflows | template: drain-it | template-version: 1 -->

# DevCanopy Drain-It

One invocation = **one tick** of a drain loop. All state lives in GitHub (board status, assignees, PRs, branches) — never in conversation memory, because under `/loop` each tick may run with no recollection of the previous one. The contract with fill-it: **drain-it pulls exclusively from Ready** and trusts that Ready means dispatchable; anything that smells undispatchable gets bounced back, never patched up inline.

## 1. Reconcile in-flight (always first)

Find work this loop already started. **The board snapshot is the source of truth**: cards in **In progress** / **In review** with assignee @me are in-flight — whether or not a PR exists yet (a sub-agent mid-implementation has only a `*/issue-N-*` branch; PR-based queries undercount and overshoot the cap).

- Open PRs from those branches → delegate to `ai-agent-skills:pr-shepherd`: mergeable check, merge greens (direct: `--squash --delete-branch`), tear down worktrees for merged PRs, reconcile local main.
- **Failed/red PRs**: surface in the tick report with the failing check named. Comment `drain-it: attempt 1 failed — <check>: <one-line cause>` on the issue. ONE redispatch with the failure context added to the prompt is allowed on a later tick; a second failure moves the card back to **Backlog** with a `blocked` label and a comment — a human (or fill-it, after the human weighs in) decides next. Never park failures in Ready: Ready must stay synonymous with dispatchable.
- **`CONFLICTING` PRs**: never auto-rebase; surface and hold.

## 2. Compute capacity

`in-flight` = cards in In progress/In review claimed by this loop (claim = assignee @me + board status). **Capacity = 5 − in-flight.** A green PR sitting in the merge queue still counts as in-flight until it is actually MERGED — compute capacity from post-reconcile live state and accept that a queue-pending slot frees up next tick, not this one. Capacity ≤ 0 → emit the tick report and stop; the next tick tops up.

## 3. Select from Ready — and only Ready

Snapshot board 5 via `ai-agent-skills:github-issues`; take the **Ready** column in board order (board order = priority). Filter, in order:

| Filter | Rule |
|--------|------|
| Claimed | Skip if assignee set or status ≠ Ready (another session got it) |
| Blocked | Skip `blocked` label |
| Dependencies | Skip while any literal `Depends on #N` references an issue that is not CLOSED — re-eligible automatically once the dep merges |
| Smell test | Run take-it's pre-flight smell test (research-shaped titles, open-question sections, stub bodies). Failures: comment why + move the card back to Backlog for fill-it. Never "fix it up" inline — that hides the grooming gap. |

Take the first `capacity` survivors.

## 4. Dispatch

Use take-it's mechanics verbatim (claim → fast-forward local main → one sub-agent per issue, `isolation: "worktree"`, single message, batch manifest in `.git/drain-it-batch.json`, take-it's self-contained sub-agent prompt).

**Model policy: pass `model: "opus"` on every dispatched Agent call.** The alias resolves to the latest Opus (4.8 today). Implementation work runs on Opus because it is the cheaper tier relative to the coordinator's session model — only this coordinator tick stays on the session model. Do not silently change the sub-agent model in either direction.

Sub-agents NEVER merge (single-writer: merges happen in §1 of a tick).

## 5. Tick report (terse — this prints every few minutes under /loop)

```
DRAIN TICK — in-flight 3/5 | merged this tick: #1712 | dispatched: #1707 #1711 | Ready remaining: 4
holds: #1713 (Depends on #1717, still open) · #1708 (migration slot busy)
```

Plus one line per failure with its next action.

## 6. Drain complete

Ready empty AND in-flight zero → announce loudly:

```
DRAIN COMPLETE — Ready is empty and nothing is in flight. Cancel the loop (or run fill-it to refill).
```

Under a self-paced loop, do not schedule another wake-up after this. Under `/loop 5m`, tell the user to cancel — ticks after completion are no-ops, not errors.

## Guardrails

- **Ready only.** Backlog items are fill-it's job — drain-it never promotes, never grooms, never files issues.
- **Hard cap 5 in flight**, counting carry-over from previous ticks, not just this tick's dispatches.
- **Idempotent ticks**: every action re-checks live GitHub state first; a crashed tick must be safely re-runnable (worktrees reclaimable via the batch manifest).
- **Single-writer**: only the coordinator merges/enqueues; max one redispatch per issue without a human.
- If `ai-agent-skills:pr-shepherd` or take-it is missing, STOP and say so — do not improvise dispatch or merge mechanics.

<!-- BEGIN PROJECT-SPECIFIC: extra-sequencing -->
<!-- END PROJECT-SPECIFIC -->
