#!/usr/bin/env bash

# Single Source of Truth for the Build Number
# ===========================================
# Emits the CFBundleVersion (CURRENT_PROJECT_VERSION) — the org Versioning
# spec's (v1.0, §1) machine-facing number. This script is the §3 build-number
# capability owner for this repo: every consumer reads it from here and the
# count is never inlined anywhere else.
#
# CONSUMER (#303): `scripts/build.sh`'s bundle path stamps this value over the
# .app's `CFBundleVersion` and then asserts it back out of the Info.plist. The
# stamp is needed because Tauri's config carries exactly ONE version field, so
# the bundler writes the marketing version into both plist keys; these are two
# decoupled numbers and CFBundleVersion is this one's. The xcodebuild
# CURRENT_PROJECT_VERSION path this was originally written for went away with
# the macOS app.
#
# DESIGN: the build number is DECOUPLED from the marketing version
# (scripts/get-version-info.sh) and is a single, globally-monotonic integer
# that NEVER resets and is never date-gated. It is the total git commit count
# on the current branch — monotonic because main is merge-only.
#
# Priority order:
#   1. BUILD_NUMBER env (org-canonical replay pin, §3) — consumed verbatim,
#      wins even over an explicit --at ref.
#   2. Total git commit count at the requested ref (default HEAD).
#   3. There is no priority 3 — FAIL CLOSED. A fallback that emits a clock
#      value is forbidden (§10): a wall-clock number silently poisons the
#      monotonic sequence the moment git works again.
#
# Usage:
#   BUILD_NUM=$(bash scripts/get-build-number.sh)
#   BUILD_NUM=$(bash scripts/get-build-number.sh --at v2026.7.3)  # count at a ref
#
# --at <ref> (§3 capability): historical rebuilds read the commit count AT a
# tag/ref from this SSOT instead of inlining `git rev-list --count $TAG`.
# rev-list peels annotated tags natively, so `--at vX.Y.Z` counts from the
# tagged COMMIT.

set -euo pipefail

REF="HEAD"
case "${1:-}" in
    "")
        ;;
    --at)
        REF="${2:-}"
        if [[ -z "$REF" ]]; then
            echo "usage: $0 [--at <ref>]" >&2
            exit 2
        fi
        ;;
    *)
        echo "usage: $0 [--at <ref>]" >&2
        exit 2
        ;;
esac

# Priority 1: replay pin — consumed verbatim.
if [[ -n "${BUILD_NUMBER:-}" ]]; then
    echo "$BUILD_NUMBER"
    exit 0
fi

# Priority 2: total git commit count at the ref.
if command -v git >/dev/null 2>&1 && git rev-parse --git-dir >/dev/null 2>&1; then
    if COMMIT_COUNT=$(git rev-list --count "$REF" 2>/dev/null) && [[ -n "$COMMIT_COUNT" ]]; then
        echo "$COMMIT_COUNT"
        exit 0
    fi
fi

# Fail closed (§10): never emit a clock value or a made-up floor.
echo "error: could not derive a build number for ref '$REF' (git missing, not a repo, or unresolvable ref)" >&2
exit 1
