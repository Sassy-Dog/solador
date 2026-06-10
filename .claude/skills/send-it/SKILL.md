---
name: send-it
description: >
  Ship a PR end-to-end in the DevCanopy repo — worktree audit, pre-flight CI guardrails,
  template-compliant PR body, commit, push,
  watch CI, merge, clean up. Use when the user says "send it", "ship it", "open the PR",
  "create a PR", or asks to merge a branch. DevCanopy-specific.
---

<!-- generated-by: ai-agent-skills:create-dev-workflows | template: send-it | template-version: 1 -->

# DevCanopy Send-It

End-to-end PR flow for this repo, in order: worktree audit → pre-flight guardrails → PR body → commit/push → watch + merge (delegated to `ai-agent-skills:pr-shepherd`).

**Merge policy: DIRECT squash merge.** This repo has no merge queue and auto-merge is disabled — merge green PRs with `gh pr merge --squash --delete-branch`. Never use `gh pr merge --auto`; with auto-merge disabled it errors or silently never merges.

## 1. Worktree audit

**Non-negotiable, even on a "trivial" one-file PR.** Run first:

```bash
git status --short
git stash list
```

For **every** entry (modified, added, deleted, untracked — including pre-existing dirt), pick exactly one action and announce it before proceeding:

| Action | When | How |
|---|---|---|
| **Ship with this PR** | Part of the same logical change | `git add <file>` — explicit paths, never `git add -A` |
| **Ship as a separate PR** | Real work, unrelated scope | Branch + commit it FIRST on its own branch, push, open PR; then return |
| **Stash for later** | Mid-flight WIP | `git stash push -m "<descriptive name>" -- <files>` |
| **Discard** | Truly unwanted | `git restore <file>` / `rm <file>` — only after confirming |

Untracked files (`??`) are the highest-risk class: invisible to `git diff`, easy to lose. Do not proceed until `git status --short` is empty OR every entry has a confirmed disposition. "I'll just stage the file I changed" is the failure mode this step exists to prevent.

## 2. Pre-flight CI guardrails

Mirror CI locally, scoped to changed paths — seconds locally beats a CI round-trip:

```bash
./dev build && ./dev test
```

Any check fails → fix before commit. Never push and rely on CI to surface it.

<!-- BEGIN PROJECT-SPECIFIC: extra-gates -->
**XcodeGen regen gate** — if `.swift` files were added, removed, or renamed, run `./Scripts/generate-project.sh` before the pre-flight; `./dev build`/`./dev test` won't pick up new source files without regenerating the XcodeGen project.
<!-- END PROJECT-SPECIFIC -->

## 3. Template-compliant PR body

**MANDATORY CHECKPOINT.** This repo has no `.github/PULL_REQUEST_TEMPLATE.md`; use this standard body, every section present:

- [ ] `## Summary` — what changed and why, 1–3 sentences
- [ ] `## Changes` — bullet list of the concrete edits
- [ ] `## Testing` — what was run (`./dev test`, manual verification) and the result

Never pass a one-liner `--body "fix bug"` that bypasses the structure.

### Issue + tracker references (close-on-merge rules)

- Closing an issue requires a literal `Closes #<N>` (or `Fixes`/`Resolves`) **on its own line** in the body — one line per issue; comma lists don't reliably parse.
- **A title parenthetical like `fix(metrics): foo (#240)` is a hyperlink, NOT a close trigger** — the classic shipped-but-still-open cause.
- Partial/follow-up work → omit the keyword, leave the issue open.

## 4. Commit, push, watch, merge

```bash
git commit -m "$(cat <<'EOF'
<type>(<scope>): short imperative

Why, briefly.

Closes #<N>

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
git push -u origin "$(git branch --show-current)"
gh pr create --title "..." --body "..."   # template-compliant body from §3
```

**Watch + merge (delegated).** Do NOT reimplement polling/merging inline:

Skill: `ai-agent-skills:pr-shepherd`
Args: "Shepherd PR #<N> in Sassy-Dog/devcanopy: mergeable check first, watch checks, then squash-merge with `--delete-branch`. After merge, reconcile local main and delete the feature branch."

If `ai-agent-skills:pr-shepherd` is not in your available skills, STOP and tell the user to install the plugin (`claude plugin install ai-agent-skills`) — do not improvise the merge flow from memory.

## Guardrails

- Never silently scope to "the file we just edited" — §1 in full, every time.
- Never push past a failing pre-flight check; never merge past a red CI.
- Never force-push main.
- Draft PRs: stop after `gh pr create` — the author flips to ready.

<!-- BEGIN PROJECT-SPECIFIC: extra-guardrails -->
<!-- END PROJECT-SPECIFIC -->
