---
name: clean-it
description: >
  Clean up local + remote git state in the DevCanopy repo after a productive day —
  fast-forward main and prune, inventory stale branches/worktrees, triage orphan
  stashes, sweep untracked-file noise, remove stale worktrees, delete merged-PR branches local +
  remote. Use when the user says "clean it", "clean up", "tidy the repo", "clean branches", or asks
  to remove stale worktrees/branches/stashes. DevCanopy-specific.
---

<!-- generated-by: ai-agent-skills:refresh-sassydog-skills | template: clean-it | template-version: 1 -->

# DevCanopy Clean-It

Post-shipping git reconciliation for this repo. This skill is **thin**: it holds DevCanopy's
facts and delegates the actual mechanics (the `[gone]` grep trap, squash-merge `-D`, stash triage by
`closedByPullRequestsReferences`, agent-worktree teardown) to `ai-agent-skills:repo-cleanup`, so the
plumbing lives in one place across all repos.

**Acting principle:** assess first, act on what's confident, escalate only on mixed signal —
substantive WIP, dep-file touches, abandoned-but-non-trivial work. Never auto-drop a stash on an OPEN
issue; never auto-discard outside the allowlist.

## Project facts

| Fact | Value |
|---|---|
| Repo | `Sassy-Dog/devcanopy` |
| Default branch | `main` |
| `delete_branch_on_merge` | `true` |
| Dep/version-file globs (block stash auto-drop) | `Package.swift` `Package.resolved` `Cargo.toml` `Cargo.lock` `project.yml` |
| Extra noise allowlist (auto-discard) | `**/.build/` `DerivedData/` `build/` `agent/target/` |
| Never-discard (precious, gitignored) | `.env*` `*.pem` `*.p8` `*.key` `*.mobileprovision` — credential/signing material that cannot be regenerated |

## Run

Delegate the full reconciliation to the capability skill, passing the facts above:

```
Skill: ai-agent-skills:repo-cleanup
Args: "Reconcile Sassy-Dog/devcanopy post-shipping. default-branch=main;
       delete-branch-on-merge=true;
       dep-version-globs=Package.swift Package.resolved Cargo.toml Cargo.lock project.yml;
       noise-allowlist=**/.build/ DerivedData/ build/ agent/target/;
       never-discard=.env* *.pem *.p8 *.key *.mobileprovision (credential/signing material — cannot be regenerated).
       Inventory read-only first, then sync+prune, triage stashes, sweep untracked noise, tear down
       stale/detached worktrees, delete [gone] + squash-merged branches, mop up stale remote
       branches. Apply the assess-first / ask-on-mixed-signal principle."
```

If `ai-agent-skills:repo-cleanup` is not in your available skills, STOP and tell the user to install
the plugin (`claude plugin install ai-agent-skills`) — do not improvise the reconciliation from
memory (the `[gone]` grep trap and squash-merge `-D` are easy to get wrong and lose work).

<!-- BEGIN PROJECT-SPECIFIC: extra-cleanup -->
<!-- Repo-unique cleanup steps that repo-cleanup doesn't cover (extra label hygiene, cache dirs,
     vendored-artifact pruning, etc.) go here and survive template updates. -->
<!-- END PROJECT-SPECIFIC -->

## Guardrails

- Never delete a branch with an OPEN PR; never delete `main` (local or remote).
- Never `--force` remove a worktree whose SHA isn't in `main` without confirming.
- Never auto-drop a stash on an OPEN issue; `closedByPullRequestsReferences`, not bare issue state, is the "shipped" signal.
- Never auto-discard untracked files outside the allowlist or anything in the never-discard list.
- Don't disable `delete_branch_on_merge`; don't run inside a worktree you're about to remove.

<!-- BEGIN PROJECT-SPECIFIC: extra-guardrails -->
<!-- END PROJECT-SPECIFIC -->
