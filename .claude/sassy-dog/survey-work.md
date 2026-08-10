---
scan_paths: crates app tests/frontend Solador DevCanopyTests Packages agent/src Scripts
exclude_pathspecs: ":!agent/target :!*.xcodeproj"
ci_workflow: ci.yml
priority_labels: [sev:high, sev:medium, sev:low]
write_policy: read-only
board:
  number: TODO  # set when the board is created (runbook R8)
  owner: cpmadrid
  project_id: TODO
  status_field_id: TODO
  ready_option_id: TODO
  backlog_option_id: TODO
  in_progress_option_id: TODO
---

## extra-guardrails

**Shipping paths from the plate:** `take #N #M` ships specific issues ad-hoc; for the board flow, `fill it` grooms Backlog → Ready, then `drain it` (or `/loop 5m /drain-it`) ships from Ready until empty.
