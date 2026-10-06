---
gotcha_summary: >
  Business logic lives in `crates/`, never in the Tauri shell or the frontend, which only paints
  what `viewmodel` hands it. Never fabricate a value to fill a gap: an unmeasured or unknown reading
  renders `—` and says why, and a defaulted state is as much a fabrication as a defaulted number.
  Gates are `./dev test` and `./dev lint`, which mirror CI and cover `agent/` as a workspace member.
  The required checks are listed in `.github/required-checks.yml` and must match job names in
  `ci.yml` exactly. Rust in `crates/*` and `app/src-tauri` must stay Windows-portable, because a
  Windows job gates every merge. Renaming `SERVICE` (the OS credential-store service name) orphans
  every stored credential, and renaming `LEGACY_APP_DIR_NAME` breaks the store migration that reads
  from it. Versions are derived from git by `scripts/get-version-info.sh` and are never
  hand-set anywhere. `agent/` must never depend on `crates/updatefeed` or `crates/crashreport`.
  The Tauri IPC boundary is deliberately not automated, so changes there need the manual smoke
  checklist in `app/README.md`.
board:
  number: 5
  owner: Sassy-Dog
  project_id: PVT_kwDODSBhws4BaqCG
  status_field_id: PVTSSF_lADODSBhws4BaqCGzhVgAc8
  ready_option_id: 8dcb24a9
  backlog_option_id: 906f24bb
  in_progress_option_id: d04a8f33
---

## extra-rubric
