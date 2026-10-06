#!/usr/bin/env bash
# Pins #539: the .dmg must mount with the app and an `Applications` symlink to
# /Applications (the drag-to-install target), and the build must fail when
# either is missing or the link points elsewhere.
#
# build.sh is a program, not a library, so the staging and assertion functions
# are lifted out of it with awk. A dummy `Solador.app` directory stands in for
# the bundle and the real `hdiutil` builds and mounts the image; no signing
# identity or credentials are needed. macOS only (hdiutil). Compatible with
# macOS /bin/bash 3.2.
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "dmg layout: skipped (needs macOS hdiutil)"
    exit 0
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

lift() { awk -v fn="$1" '$0 ~ "^" fn "\\(\\) \\{" { on = 1 } on { print } on && /^\}/ { exit }' "$SCRIPT_DIR/build.sh"; }
{ lift stage_dmg_source; lift assert_dmg_layout; lift make_dmg; } > "$work/under-test.sh"
for fn in stage_dmg_source assert_dmg_layout make_dmg; do
    grep -q "^$fn() {" "$work/under-test.sh" || { echo "could not lift $fn out of build.sh" >&2; exit 1; }
done
# shellcheck source=/dev/null
source "$work/under-test.sh"

failures=0
pass() { printf 'PASS  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }

app="$work/Solador.app"
mkdir -p "$app/Contents/MacOS"
echo stub > "$app/Contents/MacOS/Solador"

# make_image <name> <stage dir>: real hdiutil, same flags as build.sh.
make_image() {
    hdiutil create -volname Solador -srcfolder "$2" -ov -format UDZO "$work/$1.dmg" >/dev/null 2>&1
}

# expect_refused <label> <image name> <message fragment>
expect_refused() {
    if assert_dmg_layout "$work/$2.dmg" Solador.app 2>"$work/err"; then
        fail "$1"
    else
        case "$(cat "$work/err")" in
            *"$3"*) pass "$1" ;;
            *) fail "$1 (refused for the wrong reason: $(cat "$work/err"))" ;;
        esac
    fi
}

# Correct image, staged by the real function.
good="$work/stage-good"; mkdir -p "$good"
stage_dmg_source "$app" "$good"
make_image good "$good"
if assert_dmg_layout "$work/good.dmg" Solador.app 2>"$work/err"; then pass "correct image passes"; else fail "correct image passes: $(cat "$work/err")"; fi

# No Applications entry.
none="$work/stage-none"; mkdir -p "$none"
ditto "$app" "$none/Solador.app"
make_image none "$none"
expect_refused "missing Applications is refused" none "must hold exactly"

# Applications points elsewhere.
wrong="$work/stage-wrong"; mkdir -p "$wrong"
ditto "$app" "$wrong/Solador.app"
ln -s /tmp "$wrong/Applications"
make_image wrong "$wrong"
if assert_dmg_layout "$work/wrong.dmg" Solador.app 2>"$work/err"; then
    fail "wrong symlink target is refused"
else
    case "$(cat "$work/err")" in
        *"symlink to /Applications"*) pass "wrong symlink target is refused" ;;
        *) fail "wrong symlink target refused for the wrong reason: $(cat "$work/err")" ;;
    esac
fi

# Applications is a real directory, not a symlink.
dir="$work/stage-dir"; mkdir -p "$dir/Applications"
ditto "$app" "$dir/Solador.app"
make_image dir "$dir"
expect_refused "directory named Applications is refused" dir "must be a symlink"

# Extra entry.
extra="$work/stage-extra"; mkdir -p "$extra"
stage_dmg_source "$app" "$extra"
echo x > "$extra/README"
make_image extra "$extra"
expect_refused "extra root entry is refused" extra "must hold exactly"

# make_dmg itself (the wiring: stage -> hdiutil -> assert -> sign). codesign is
# stubbed; everything else is real. Run in a subshell because make_dmg traps
# EXIT and calls `exit`. A make_dmg that reverts to `-srcfolder "$app"` or
# drops the assertion fails here.
codesign() { :; }
export APP_NAME=Solador
( make_dmg "$app" "$work/full.dmg" "stub identity" ) >/dev/null 2>"$work/err" \
    && pass "make_dmg builds a layout-correct image" \
    || fail "make_dmg builds a layout-correct image: $(cat "$work/err")"
if assert_dmg_layout "$work/full.dmg" Solador.app 2>/dev/null; then pass "make_dmg output has the Applications link"; else fail "make_dmg output has the Applications link"; fi
rm -f "$work/full.dmg"
# Negative control: a make_dmg whose staging omits the link must fail the build.
# The stub is sourced like the real function, never defined inline: an inline
# definition would be the only one ShellCheck can see, and 0.9.0 (CI's) then
# reports the calls above as SC2218, "only defined later".
cat > "$work/stage-without-link.sh" <<'EOF'
stage_dmg_source() { ditto "$1" "$2/$(basename "$1")"; }
EOF
if (
    # shellcheck source=/dev/null
    source "$work/stage-without-link.sh"
    make_dmg "$app" "$work/bad.dmg" "stub identity"
) >/dev/null 2>"$work/err"; then
    fail "make_dmg without the link fails the build"
else
    case "$(cat "$work/err")" in
        *"must be a symlink"*|*"must hold exactly"*) pass "make_dmg without the link fails the build" ;;
        *) fail "make_dmg without the link failed for the wrong reason: $(cat "$work/err")" ;;
    esac
fi

if [[ "$failures" -ne 0 ]]; then
    echo "$failures failure(s)" >&2
    exit 1
fi
echo "dmg layout: all passed"
