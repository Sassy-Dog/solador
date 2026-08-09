---
scan_paths: crates app tests/frontend DevCanopy DevCanopyTests Packages agent/src Scripts
exclude_pathspecs: ":!agent/target :!*.xcodeproj"
ci_workflow: ci.yml
priority_labels: [sev:high, sev:medium, sev:low]
write_policy: read-only
board:
  number: 5
  owner: Sassy-Dog
  project_id: PVT_kwDODSBhws4BaqCG
  status_field_id: PVTSSF_lADODSBhws4BaqCGzhVgAc8
  ready_option_id: 8dcb24a9
  backlog_option_id: 906f24bb
  in_progress_option_id: d04a8f33
---

## extra-guardrails

**Shipping paths from the plate:** `take #N #M` ships specific issues ad-hoc; for the board flow, `fill it` grooms Backlog → Ready, then `drain it` (or `/loop 5m /drain-it`) ships from Ready until empty.
