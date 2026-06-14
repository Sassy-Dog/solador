---
name: fill-it
description: >
  Backlog grooming for DevCanopy: refine open issues until they are fully dispatchable
  by a cold worktree sub-agent, then move them to Ready on the board. The counterpart that
  feeds drain-it. Use when the user says "fill it", "groom the backlog", "refine the backlog",
  "scope these issues", "make these dispatchable", "get the backlog ready", or asks to move
  issues to Ready. Writes: issue-body edits, board moves, and
  epic-split sub-issues only — never deletes, never closes, never dispatches work —
  DevCanopy-specific
---

<!-- generated-by: ai-agent-skills:create-dev-workflows | template: fill-it | template-version: 1 -->

# DevCanopy Fill-It

Groom the backlog until every issue is either **Ready** (a cold sub-agent could ship it) or **explicitly parked with a named reason**. Fill-it owns *content quality*; sequencing and dispatch belong to drain-it. The two share one contract: **Ready means dispatchable.** Nothing reaches Ready that you would not hand to a worktree agent with zero conversation context.

## 1. Collect candidates

Board 5 is authoritative (GH Project board #5 status columns — the board is the source of truth for backlog state):

- Snapshot via `ai-agent-skills:github-issues` (`board-snapshot.sh`, `PROJECT_NUMBER=5 OWNER=Sassy-Dog`).
- Candidates: every open issue in **Backlog** or with **no status** (open issues missing from the board get added to it). Re-validate existing Ready items every run (the snapshot is already in hand) — drift happens; a decision marker or new blocker added after promotion demotes the card back to Backlog with a comment. Ready is a promise; stale promises break drain-it.
- Read each candidate IN FULL: `gh issue view N --repo Sassy-Dog/devcanopy --comments` — scope often lives in follow-up comments.

## 2. The dispatchability rubric

An issue is **Ready** only if ALL of these hold:

| # | Test | Failure action |
|---|------|----------------|
| 1 | Problem + desired outcome stated in the body | Refine (§3) |
| 2 | Scope names real touchpoints (files/components/services) or enough pointers that a cold agent finds them in one recon pass | Refine (§3) — recon the codebase yourself, write the map in |
| 3 | Acceptance criteria checklist present | Refine (§3) |
| 4 | Self-contained: screenshots/attachments transcribed into prose (GitHub `user-attachments` URLs are cookie-walled — unreadable from a worktree agent), referenced docs committed on the default branch | Refine (§3); ask the user to paste any image you cannot read — until they do, the verdict is **parked: awaiting-user** |
| 5 | No open product decisions: no `(decision)` markers, no `## Open questions`, no "TBD" | Surface the decision to the user with a recommended default; issue stays Backlog until resolved |
| 6 | Right-sized: one coherent PR per issue | Epic → split (§4) |
| 7 | Dependencies recorded as literal `Depends on #N` lines (one per line) | Add them — drain-it enforces ordering from these lines |

A dependency being open does NOT block Ready (drain-it sequences at dispatch time) — only *unrecorded* dependencies block, because invisible ordering is how parallel agents collide.

## 3. Refine

Per failing candidate:

1. Ground the scope in the codebase — dispatch `Explore` agent(s) for recon when touchpoints are unknown; never write a scope you haven't verified against real files.
2. Rewrite the body: preserve the original ask as a `> quote`, then problem/scope/touchpoints/acceptance/dispatch-notes sections.
3. Record repo gotchas the sub-agent needs (run `./Scripts/generate-project.sh` after adding/removing/renaming `.swift` files (XcodeGen); `Packages/HostMetricsKit` is a local SwiftPM package; the Rust agent in `agent/` must be redeployed to `ubu-3xdv` after changes; Debug signs with team 52YMXC3348).
4. `gh issue edit N --repo Sassy-Dog/devcanopy --body-file ...` — edit, don't comment-and-hope.

Decisions are NEVER guessed: present each to the user as a recommendation with trade-offs; fold the answer into the body marked **Decision (date)** so it supersedes any `(decision)` marker.

## 4. Epic split

A multi-workstream issue gets child issues (one per dispatchable unit) via the gated write path — `ai-agent-skills:github-issues` `file-or-link-issue.sh`, marker `epic-split: #<parent>/<slug>`, body containing `Part of #<parent>` (NOT `Closes`). Child issues then pass the §2 rubric individually; the parent stays out of Ready (it tracks, it doesn't dispatch). **Run splits FIRST in a grooming pass**: `Depends on #N` lines must point at dispatchable issues — children, never a tracking parent — so an issue depending on "the schema part of epic #E" cannot finalize its dependency line until #E's split has produced the child number.

## 5. Promote + report

Move qualifying cards to **Ready** per `ai-agent-skills:github-issues` (`references/board-graphql.md`; project `PVT_kwDODSBhws4BaqCG`, status field `PVTSSF_lADODSBhws4BaqCGzhVgAc8`, Ready `8dcb24a9`).

Final table: issue · verdict (**Ready** / needs-decision / split → children / parked: awaiting-user / parked: reason) · what changed. End with the decisions awaiting the user, if any.

## Guardrails

- Never file new issues outside the §4 epic-split gate; synthesis of brand-new work is plate-it's job.
- Never close issues, never delete content — the original ask survives as a quote.
- Never promote with an unresolved decision "because the default is obvious" — the default goes to the user first.
- Ready is a promise to drain-it. When in doubt, park with a reason instead.

<!-- BEGIN PROJECT-SPECIFIC: extra-rubric -->
<!-- END PROJECT-SPECIFIC -->
