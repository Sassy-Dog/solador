---
max_in_flight: 3
merge_queue: true
board:
  number: 5
  owner: Sassy-Dog
  project_id: PVT_kwDODSBhws4BaqCG
  status_field_id: PVTSSF_lADODSBhws4BaqCGzhVgAc8
  ready_option_id: 8dcb24a9
  backlog_option_id: 906f24bb
  in_progress_option_id: d04a8f33
---

## extra-sequencing

**Dispatch cap is deliberately 3 in this repo, not the template default of 5.** Four places encode
it and must agree: the frontmatter description, §2 capacity, the §5 tick-report example's
`in-flight n/3` denominator, and the §Guardrails hard cap. The binding constraint is
self-hosted macOS runner capacity, not agent throughput: `ci.yml` runs **three** jobs on
`[self-hosted, macOS, sassy-dog]` — `swift-tests`, `lint`, `rust-workspace` (lines 26, 125, 218) —
against exactly **two** org macOS runners (`sassydog-ghr-mac-s1` / `-s2`). A 5-wide burst plus
merge-queue and push runs stacks 15+ macOS-bound jobs onto 2 slots, which is what turned CI's p90
into queue wait (Jul 29–30: jobs executed in 0.3–3.1 min while waiting 42–73 min). At 3 wide the
steady-state macOS demand fits the runners. See issue #145.

**If a template re-sync (`ai-agent-skills:refresh-sassydog-skills`) restores 5, re-apply 3** in all
four places above. Revisit the number only if the macOS runner count changes or ci.yml's macOS job
count changes — those are the two inputs.
