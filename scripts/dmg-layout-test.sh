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

# dmg_under <path> <dir>: is <path> inside <dir>? /private/var and /var are one
# place; hdiutil reports the spelling it was given, a caller may hold either.
dmg_under() {
    local a="${1#/private}" b="${2#/private}" canon
    canon="$(cd "$2" 2>/dev/null && pwd -P)" || canon="$2"
    canon="${canon#/private}"
    case "$a" in
        "$b"/*|"$canon"/*) return 0 ;;
        *) return 1 ;;
    esac
}

# dmg_is_leaked_fixture <image path> <temp root>: THE reaper rule (#629). True
# only for an image that is <temp root>/tmp.*/<one of this suite's fixture
# names>.dmg AND whose backing file is gone. A live run's fixtures (another
# worktree's suite, this one) still have their file, so they never match;
# neither do ~/Downloads installers or the simulator runtime images (a fixture
# replaced in place must therefore never be absent from its path). The temp
# root is compared with /private dropped from either side, since the leaked
# image's directory is gone and cannot be resolved.
dmg_is_leaked_fixture() {
    local path="$1" root="$2" canon_root parent grand
    parent="$(dirname "$path")"
    grand="$(dirname "$parent")"
    canon_root="$(cd "$root" 2>/dev/null && pwd -P)" || canon_root="$root"
    grand="${grand#/private}"
    case "$grand" in
        "${root#/private}"|"${canon_root#/private}") ;;
        *) return 1 ;;
    esac
    case "$(basename "$parent")" in
        tmp.*) ;;
        *) return 1 ;;
    esac
    case "$(basename "$path")" in
        good.dmg|none.dmg|wrong.dmg|dir.dmg|extra.dmg|full.dmg|bad.dmg) ;;
        *) return 1 ;;
    esac
    [[ ! -e "$path" ]]
}

# reap_leaked_fixtures <temp root>: detach, and say so, every (device, image)
# pair the rule above matches. Nothing else is ever touched.
reap_leaked_fixtures() {
    local listing dev path
    listing="$(dmg_attached_images)" || {
        echo "reap: could not read 'hdiutil info'; leaked fixtures, if any, were not reaped" >&2
        return 0
    }
    while IFS=$'\t' read -r dev path; do
        [[ -n "$dev" ]] || continue
        if dmg_is_leaked_fixture "$path" "$1"; then
            echo "reaping leaked fixture device $dev (image $path is gone)"
            detach_dmg_device "$dev" "$path" || true
        fi
    done <<< "$listing"
}

# #629: the trap detaches before it deletes, so a mid-suite death, a `set -e`
# exit or a Ctrl-C leaks no attached image. INT/TERM exit so this still runs.
# A listing it cannot read is said, never taken for "nothing attached".
# shellcheck disable=SC2329  # invoked through the trap
cleanup() {
    local listing dev path
    if declare -F dmg_attached_images >/dev/null; then
        if listing="$(dmg_attached_images)"; then
            while IFS=$'\t' read -r dev path; do
                [[ -n "$dev" ]] || continue
                if dmg_under "$path" "$work"; then
                    detach_dmg_device "$dev" "$path" || true
                fi
            done <<< "$listing"
        else
            echo "cleanup: could not read 'hdiutil info'; devices under $work may remain attached" >&2
        fi
    fi
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# BUILD_SH points at another copy of build.sh (the negative controls). The
# helpers come from this checkout first; anything BUILD_SH defines under the
# same name replaces them, which is how a control swaps in an earlier or
# mutated revision of one function.
build_sh="${BUILD_SH:-$SCRIPT_DIR/build.sh}"
lift() { awk -v fn="$2" '$0 ~ "^" fn "\\(\\) \\{" { on = 1 } on { print } on && /^\}/ { exit }' "$1"; }
helpers="dmg_attached_images dmg_norm_path dmg_same_image dmg_in_list dmg_devices_for_image dmg_device_state detach_dmg_device release_dmg_image warn_kept_dmg_devices dmg_attach_diag"
{
    for fn in $helpers; do
        lift "$SCRIPT_DIR/build.sh" "$fn"
    done
    for fn in stage_dmg_source $helpers attach_dmg_readonly assert_dmg_layout make_dmg; do
        lift "$build_sh" "$fn"
    done
} > "$work/under-test.sh"
for fn in stage_dmg_source attach_dmg_readonly assert_dmg_layout make_dmg; do
    grep -q "^$fn() {" "$work/under-test.sh" || { echo "could not lift $fn out of build.sh" >&2; exit 1; }
done
# shellcheck source=/dev/null
source "$work/under-test.sh"

# Earlier runs' leaked fixture devices (#629) make attaches fail; reap them.
reap_leaked_fixtures "$(dirname "$work")"

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

# --probe-die is how the suite tests its own EXIT trap: attach an image, print
# where, and die the way a mid-suite failure would. The parent then requires
# the device to be gone. Not an operator flag.
if [[ "${1:-}" == "--probe-die" ]]; then
    probe="$work/stage-probe"; mkdir -p "$probe"
    stage_dmg_source "$app" "$probe"
    make_image good "$probe"
    attach_dmg_readonly "$(mktemp -d "$work/mnt.XXXXXX")" "$work/good.dmg"
    echo "PROBE $work $ATTACHED_DEV"
    exit 3
fi

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

# #586: a transient EAGAIN from `hdiutil attach` is retried; nothing else is.
# The stub fails the first <n> attaches with <message>, records each attach, and
# hands every other call (and later attaches) to the real hdiutil. `sleep` is a
# no-op so the backoff costs nothing here. Sourced, like the stub below.
cat > "$work/flaky-hdiutil.sh" <<'EOF'
hdiutil() {
    if [[ "$1" == attach ]]; then
        echo attach >> "$FLAKY_LOG"
        if (( $(wc -l < "$FLAKY_LOG") <= FLAKY_FAILS )); then
            echo "hdiutil: attach failed - $FLAKY_MESSAGE" >&2
            # #660: an optional second error line, to prove the `error:` field
            # of a `dmg attach:` line carries only the first.
            if [[ -n "${FLAKY_SECOND:-}" ]]; then echo "$FLAKY_SECOND" >&2; fi
            return 1
        fi
    fi
    command hdiutil "$@"
}
sleep() { :; }
EOF
# attach_under_flake <fails> <message>: assert_dmg_layout on the good image;
# prints "<status> <attach count>".
attach_under_flake() {
    local status=0
    : > "$work/attaches"
    (
        export FLAKY_LOG="$work/attaches" FLAKY_FAILS="$1" FLAKY_MESSAGE="$2"
        # shellcheck source=/dev/null
        source "$work/flaky-hdiutil.sh"
        assert_dmg_layout "$work/good.dmg" Solador.app
    ) 2>"$work/err" || status=$?
    echo "$status $(wc -l < "$work/attaches" | tr -d ' ')"
}
busy="Resource temporarily unavailable"
result="$(attach_under_flake 2 "$busy")"
if [[ "$result" == "0 3" ]]; then
    pass "a busy attach is retried until it mounts"
else
    fail "a busy attach is retried until it mounts (status, attaches: $result; $(cat "$work/err"))"
fi
result="$(attach_under_flake 99 "$busy")"
# #660: anchored to the stub's own line, which only the give-up's final print of
# hdiutil's error can produce; a `dmg attach:` line ends in it but never starts
# with it, so it cannot satisfy the check.
if [[ "$result" == "1 5" ]] && grep -q "^hdiutil: attach failed - $busy\$" "$work/err" && grep -q "could not mount" "$work/err"; then
    pass "a busy attach gives up after 5 tries and names the error"
else
    fail "a busy attach gives up after 5 tries and names the error (status, attaches: $result; $(cat "$work/err"))"
fi
result="$(attach_under_flake 99 "no mountable file systems")"
if [[ "$result" == "1 1" ]] && grep -q "^hdiutil: attach failed - no mountable file systems\$" "$work/err"; then
    pass "any other attach failure is not retried"
else
    fail "any other attach failure is not retried (status, attaches: $result; $(cat "$work/err"))"
fi

# #642: every failed attempt leaves one `dmg attach:` line on stderr saying what
# stood on THIS image and at the mountpoint, so a real EAGAIN carries its own
# evidence. A first-try success prints none.
diag_count() { { grep -c '^dmg attach: attempt ' "$work/err" || true; } | tr -d ' '; }
tail_fields='devices on .*: none; .* mounted: no; images attached in total: [0-9][0-9]*'
result="$(attach_under_flake 2 "$busy")"
if [[ "$result" == "0 3" && "$(diag_count)" == 2 ]] \
    && grep -q "^dmg attach: attempt 1/5 failed after [0-9][0-9]*s, hdiutil exit 1: $tail_fields; error: hdiutil: attach failed - $busy\$" "$work/err" \
    && grep -q "^dmg attach: attempt 2/5 failed after [0-9][0-9]*s, hdiutil exit 1: $tail_fields; error: hdiutil: attach failed - $busy\$" "$work/err" \
    && ! grep -q 'final sweep' "$work/err"; then
    pass "two busy attaches leave two dmg attach lines with attempt, exit, devices, mountpoint and error"
else
    fail "two busy attaches leave two dmg attach lines (status, attaches: $result; lines: $(diag_count); $(cat "$work/err"))"
fi
# #660: an error of two lines. Each `error:` field carries the first line only
# (`head -n 1`); the give-up still prints both lines, the second on its own.
second="second line of the same error"
result="$(FLAKY_SECOND="$second" attach_under_flake 99 "$busy")"
if [[ "$result" == "1 5" ]] \
    && [[ "$(grep -c "^dmg attach: attempt [1-5]/5 .*; error: hdiutil: attach failed - $busy\$" "$work/err")" == 5 ]] \
    && ! grep '^dmg attach: ' "$work/err" | grep -q "$second" \
    && [[ "$(grep -c "^$second\$" "$work/err")" == 1 ]]; then
    pass "a two-line attach error shows only its first line in each error field"
else
    fail "a two-line attach error shows only its first line in each error field (status, attaches: $result; $(cat "$work/err"))"
fi
result="$(attach_under_flake 0 "$busy")"
if [[ "$result" == "0 1" ]] && ! grep -q '^dmg attach:' "$work/err"; then
    pass "a successful first attach prints no dmg attach line"
else
    fail "a successful first attach prints no dmg attach line (status, attaches: $result; $(cat "$work/err"))"
fi
result="$(attach_under_flake 99 "$busy")"
if [[ "$result" == "1 5" && "$(diag_count)" == 5 ]] \
    && [[ "$(grep -c "^dmg attach: after the 2 s settle, before the final sweep: $tail_fields\$" "$work/err")" == 1 ]] \
    && [[ "$(grep -c "^dmg attach: after the final sweep (what remains): $tail_fields\$" "$work/err")" == 1 ]]; then
    pass "a give-up has one line per attempt plus a pre-sweep and a post-sweep line"
else
    fail "a give-up has one line per attempt plus a pre-sweep and a post-sweep line (status, attaches: $result; lines: $(diag_count); $(cat "$work/err"))"
fi
# A listing that cannot be read during a diagnostic prints `unknown` and
# changes no outcome: only `hdiutil info` reads made from inside
# dmg_attach_diag fail; the reads the attach and the sweep make are the real ones.
cat > "$work/blind-hdiutil.sh" <<'EOF'
source "$work/flaky-hdiutil.sh"
flaky_hdiutil=$(declare -f hdiutil)
eval "flaky_${flaky_hdiutil#hdiutil}"
hdiutil() {
    if [[ "$1" == info && " ${FUNCNAME[*]} " == *" dmg_attach_diag "* ]]; then return 1; fi
    flaky_ "$@"
}
EOF
blind_run() {
    local status=0
    : > "$work/attaches"
    (
        export FLAKY_LOG="$work/attaches" FLAKY_FAILS="$1" FLAKY_MESSAGE="$busy"
        # shellcheck source=/dev/null
        source "$work/blind-hdiutil.sh"
        assert_dmg_layout "$work/good.dmg" Solador.app
    ) 2>"$work/err" || status=$?
    echo "$status $(wc -l < "$work/attaches" | tr -d ' ')"
}
result="$(blind_run 1)"
if [[ "$result" == "0 2" && "$(diag_count)" == 1 ]] \
    && grep -q '^dmg attach: attempt 1/5 .*: devices on .*: unknown; .* mounted: no; images attached in total: unknown; error: ' "$work/err"; then
    pass "an unreadable listing during a diagnostic prints unknown and the attach still succeeds"
else
    fail "an unreadable listing during a diagnostic prints unknown (status, attaches: $result; $(cat "$work/err"))"
fi
result="$(blind_run 99)"
if [[ "$result" == "1 5" && "$(diag_count)" == 5 ]] \
    && [[ "$(grep -c 'devices on .*: unknown; .* images attached in total: unknown' "$work/err")" == 7 ]]; then
    pass "a give-up with unreadable diagnostics has the same outcome as with readable ones"
else
    fail "a give-up with unreadable diagnostics has the same outcome (status, attaches: $result; $(cat "$work/err"))"
fi

# A fake listing holds two images: ours, already attached before the attach
# (kept, never detached, marked pre-existing) and an operator's, whose name
# must never appear. Every attach fails EAGAIN; nothing real is attached.
cat > "$work/other-hdiutil.sh" <<'EOF'
hdiutil() {
    case "$1" in
        attach) echo "hdiutil: attach failed - Resource temporarily unavailable" >&2; return 1 ;;
        info)
            printf '<?xml version="1.0"?>\n<plist version="1.0"><dict><key>images</key><array>'
            printf '<dict><key>image-path</key><string>/elsewhere/mine.dmg</string><key>system-entities</key><array><dict><key>dev-entry</key><string>/dev/disk77</string></dict></array></dict>'
            printf '<dict><key>image-path</key><string>/elsewhere/OPERATORS-SECRET-IMAGE.sparseimage</string><key>system-entities</key><array><dict><key>dev-entry</key><string>/dev/disk90</string></dict></array></dict>'
            printf '</array></dict></plist>\n' ;;
        detach) echo "$*" >> "$FAKE_DETACHES" ;;
    esac
}
sleep() { :; }
EOF
other_status=0
: > "$work/fake-detaches"
(
    export FAKE_DETACHES="$work/fake-detaches"
    # shellcheck source=/dev/null
    source "$work/other-hdiutil.sh"
    attach_dmg_readonly "$work" /elsewhere/mine.dmg
) 2>"$work/err" || other_status=$?
pre="devices on /elsewhere/mine.dmg: /dev/disk77 (pre-existing); .* mounted: no; images attached in total: 2"
if [[ "$other_status" == 1 && "$(diag_count)" == 5 && ! -s "$work/fake-detaches" ]] \
    && [[ "$(grep -c "^dmg attach: attempt [1-5]/5 .*: $pre" "$work/err")" == 5 ]] \
    && grep -q "^dmg attach: after the 2 s settle, before the final sweep: $pre\$" "$work/err" \
    && grep -q "^dmg attach: after the final sweep (what remains): $pre\$" "$work/err" \
    && ! grep -qE 'SECRET|disk90' "$work/err"; then
    pass "a pre-existing device is marked as such and another image is only counted, never named"
else
    fail "a pre-existing device is marked and another image is only counted (status $other_status; detaches: $(cat "$work/fake-detaches"); $(cat "$work/err"))"
fi

# #629: a failed attach or detach must not leave a device attached. The stub
# acts on the REAL hdiutil, so a leak is a real attached device, observed with
# the same `hdiutil info` the operator would use.
#   STUB_ATTACH_FAILS  the first <n> attaches really attach, then report EAGAIN
#   STUB_DETACH_MODE   pass | uneject (first detach unmounts, then fails) | always
cat > "$work/leaky-hdiutil.sh" <<'EOF'
hdiutil() {
    local n
    case "$1" in
        attach)
            n=$(( $(cat "$STUB_ATTACHES" 2>/dev/null || echo 0) + 1 ))
            echo "$n" > "$STUB_ATTACHES"
            # How many devices already stand on this image when an attempt starts.
            dmg_devices_for_image "$STUB_DMG" | wc -l | tr -d ' ' >> "$STUB_PRE"
            if (( n <= STUB_ATTACH_FAILS )); then
                command hdiutil "$@" >/dev/null 2>&1
                echo "hdiutil: attach failed - Resource temporarily unavailable" >&2
                return 1
            fi
            ;;
        detach)
            echo "$*" >> "$STUB_DETACHES"
            case "$STUB_DETACH_MODE" in
                always) echo "hdiutil: couldn't eject - busy" >&2; return 1 ;;
                uneject)
                    if [[ "$(wc -l < "$STUB_DETACHES" | tr -d ' ')" == 1 ]]; then
                        command diskutil unmountDisk "$2" >/dev/null 2>&1
                        return 1
                    fi
                    ;;
            esac
            ;;
    esac
    command hdiutil "$@"
}
sleep() { :; }
EOF
# leak_run <attach fails> <detach mode> [github]: assert_dmg_layout on the good
# image under the stub; prints the status. Files: $work/{err,detaches,pre}.
# GITHUB_ACTIONS is cleared (CI runs this suite with it set) unless asked for.
leak_run() {
    local status=0
    rm -f "$work/attaches" "$work/detaches" "$work/pre"
    : > "$work/detaches"; : > "$work/pre"
    (
        export STUB_ATTACHES="$work/attaches" STUB_DETACHES="$work/detaches" STUB_PRE="$work/pre"
        export STUB_DMG="$work/good.dmg" STUB_ATTACH_FAILS="$1" STUB_DETACH_MODE="$2"
        unset GITHUB_ACTIONS
        if [[ "${3:-}" == github ]]; then export GITHUB_ACTIONS=true; fi
        # shellcheck source=/dev/null
        source "$work/leaky-hdiutil.sh"
        assert_dmg_layout "$work/good.dmg" Solador.app
    ) 2>"$work/err" || status=$?
    echo "$status"
}
# What stands on the good image; "UNREADABLE" if the listing cannot be read, so
# a failed read never passes for "nothing attached".
leftover() {
    local d
    d="$(dmg_devices_for_image "$work/good.dmg")" || { echo UNREADABLE; return 0; }
    printf '%s' "$d" | tr '\n' ' '
}

result="$(leak_run 2 pass)"
if [[ "$result" == "0" && -z "$(leftover)" && "$(tr '\n' ' ' < "$work/pre")" == "0 0 0 " ]]; then
    pass "an attach that fails after attaching is detached and the next attempt starts clean"
else
    fail "an attach that fails after attaching is detached (status $result; devices standing at each attempt: $(tr '\n' ' ' < "$work/pre"); left: $(leftover); $(cat "$work/err"))"
fi
release_dmg_image "$work/good.dmg" || true
# #642: the line of an attach that attached and then reported EAGAIN names that
# device, on this image's path, with the mountpoint mounted.
if [[ "$(diag_count)" == 2 ]] \
    && [[ "$(grep -c "^dmg attach: attempt [12]/5 failed after [0-9][0-9]*s, hdiutil exit 1: devices on $work/good.dmg: /dev/disk[0-9]*; .* mounted: yes; " "$work/err")" == 2 ]]; then
    pass "an attach that attached then failed is recorded with its device and a mounted mountpoint"
else
    fail "an attach that attached then failed is recorded with its device (lines: $(diag_count); $(cat "$work/err"))"
fi

result="$(leak_run 0 uneject)"
if [[ "$result" == "0" && -z "$(leftover)" ]] \
    && [[ "$(wc -l < "$work/detaches" | tr -d ' ')" -ge 2 ]] \
    && ! grep -qv '^detach /dev/disk' "$work/detaches"; then
    pass "a detach that unmounts but does not eject is retried by device node"
else
    fail "a detach that unmounts but does not eject is retried by device node (status $result; left: $(leftover); detaches: $(tr '\n' '|' < "$work/detaches"))"
fi
release_dmg_image "$work/good.dmg" || true

result="$(leak_run 0 always)"
stuck="$(leftover)"
if [[ "$result" == "1" && "$stuck" == /dev/disk* ]] \
    && grep -q "could not detach /dev/disk" "$work/err" \
    && grep -q "hdiutil detach -force /dev/disk" "$work/err"; then
    pass "a detach that always fails is reported with the device named"
else
    fail "a detach that always fails is reported with the device named (status $result; left: $stuck; $(cat "$work/err"))"
fi
# Our own device, left by the stub on purpose: remove it with the real hdiutil.
release_dmg_image "$work/good.dmg" || fail "could not clean up the stubbed-stuck device"

# Local vs hosted runner: the same unconfirmed detach fails locally (above) and
# only warns on GitHub Actions, where the runner is thrown away.
result="$(leak_run 0 always github)"
if [[ "$result" == "0" ]] && grep -q "continuing on a throwaway runner" "$work/err"; then
    pass "an unconfirmed detach warns instead of failing on GitHub Actions"
else
    fail "an unconfirmed detach warns instead of failing on GitHub Actions (status $result; $(cat "$work/err"))"
fi
release_dmg_image "$work/good.dmg" || fail "could not clean up the stubbed-stuck device"

# A freed /dev/diskN is handed to the next image at once. A device is the pair
# (device, image): once the listing shows the number under another image, no
# further detach, -force included, may be sent (that would hit its new owner).
# And an empty listing is not an unreadable one. A fake listing stands in for
# `hdiutil info`; nothing real is attached or detached.
cat > "$work/fake-hdiutil.sh" <<'EOF'
fake_plist() {
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>images</key><array><dict><key>image-path</key><string>%s</string><key>system-entities</key><array><dict><key>dev-entry</key><string>%s</string></dict></array></dict></array></dict></plist>\n' "$1" "$2"
}
fake_plist2() {
    printf '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>images</key><array>'
    for d in "$2" "$3"; do
        printf '<dict><key>image-path</key><string>%s</string><key>system-entities</key><array><dict><key>dev-entry</key><string>%s</string></dict></array></dict>' "$1" "$d"
    done
    printf '</array></dict></plist>\n'
}
hdiutil() {
    case "$1" in
        attach)
            printf '<?xml version="1.0"?>\n<plist version="1.0"><dict><key>system-entities</key><array><dict><key>dev-entry</key><string>/dev/disk77s1</string></dict></array></dict></plist>\n'
            ;;
        info)
            case "$FAKE_LISTING" in
                reuse)
                    if [[ -s "$FAKE_DETACHES" ]]; then fake_plist /elsewhere/other.dmg /dev/disk77; else fake_plist /elsewhere/mine.dmg /dev/disk77; fi ;;
                reuse-from-start) fake_plist /elsewhere/other.dmg /dev/disk77 ;;
                adopt) fake_plist /elsewhere/mine.dmg /dev/disk77 ;;
                two)
                    # disk77 stays; disk78 goes once a detach names it.
                    if grep -q disk78 "$FAKE_DETACHES"; then fake_plist /elsewhere/mine.dmg /dev/disk77; else fake_plist2 /elsewhere/mine.dmg /dev/disk77 /dev/disk78; fi ;;
                empty-array) printf '<?xml version="1.0"?>\n<plist version="1.0"><dict><key>images</key><array/></dict></plist>\n' ;;
                no-key) printf '<?xml version="1.0"?>\n<plist version="1.0"><dict><key>framework</key><string>704</string></dict></plist>\n' ;;
                junk) echo "not a plist" ;;
            esac ;;
        detach) echo "$*" >> "$FAKE_DETACHES" ;;
    esac
}
sleep() { :; }
EOF
# fake_run <listing> <function and arguments...>: prints "<status> <detaches sent>".
fake_run() {
    local status=0 listing="$1"
    shift
    : > "$work/fake-detaches"
    (
        export FAKE_LISTING="$listing" FAKE_DETACHES="$work/fake-detaches"
        # shellcheck source=/dev/null
        source "$work/fake-hdiutil.sh"
        "$@"
    ) >"$work/fake-out" 2>"$work/err" || status=$?
    echo "$status $(wc -l < "$work/fake-detaches" | tr -d ' ')"
}
result="$(fake_run reuse detach_dmg_device /dev/disk77 /elsewhere/mine.dmg)"
if [[ "$result" == "0 1" ]] && ! grep -q force "$work/fake-detaches"; then
    pass "a device number reused by another image after a detach is not detached again"
else
    fail "a device number reused by another image after a detach is not detached again (status, detaches: $result: $(tr '\n' '|' < "$work/fake-detaches"))"
fi
empty_bad=""
for mode in empty-array no-key; do
    [[ "$(fake_run "$mode" dmg_attached_images)" == "0 0" && ! -s "$work/fake-out" ]] || empty_bad="$empty_bad $mode:list"
    [[ "$(fake_run "$mode" detach_dmg_device /dev/disk77 /elsewhere/mine.dmg)" == "0 0" ]] || empty_bad="$empty_bad $mode:detach"
    [[ "$(fake_run "$mode" release_dmg_image /elsewhere/mine.dmg)" == "0 0" ]] || empty_bad="$empty_bad $mode:release"
done
if [[ -z "$empty_bad" ]]; then pass "a readable listing with no images is empty, not unreadable"; else fail "a readable listing with no images is empty, not unreadable:$empty_bad"; fi
unreadable_bad=""
[[ "$(fake_run junk dmg_attached_images)" == "1 0" ]] || unreadable_bad="$unreadable_bad list"
[[ "$(fake_run junk release_dmg_image /elsewhere/mine.dmg)" == "1 0" ]] || unreadable_bad="$unreadable_bad release"
if [[ "$(fake_run junk detach_dmg_device /dev/disk77 /elsewhere/mine.dmg)" != "1 0" ]] || ! grep -q "could not read 'hdiutil info'" "$work/err"; then unreadable_bad="$unreadable_bad detach"; fi
if [[ -z "$unreadable_bad" ]]; then pass "an unreadable listing fails, and no detach is sent on it"; else fail "an unreadable listing fails, and no detach is sent on it:$unreadable_bad"; fi

# The number is under another image from the very first read: nothing is sent,
# so a pair check skipped on the first attempt cannot hide behind the later ones.
reuse_bad=""
[[ "$(fake_run reuse-from-start detach_dmg_device /dev/disk77 /elsewhere/mine.dmg)" == "0 0" ]] || reuse_bad="$reuse_bad detach"
[[ "$(fake_run reuse-from-start release_dmg_image /elsewhere/mine.dmg)" == "0 0" ]] || reuse_bad="$reuse_bad release"
if [[ -z "$reuse_bad" ]]; then pass "a device number that already belongs to another image gets no detach at all"; else fail "a device number that already belongs to another image gets no detach at all:$reuse_bad"; fi

# The keep-list: devices that stood on the image before this run attached it
# (an earlier build still mounted at the same path) are never detached. Fake
# listing: two devices on one image, one kept; only the other may be named.
result="$(fake_run two release_dmg_image /elsewhere/mine.dmg /dev/disk77)"
if [[ "$result" == "0 1" ]] && grep -q '^detach /dev/disk78$' "$work/fake-detaches" && ! grep -q disk77 "$work/fake-detaches"; then
    pass "release_dmg_image leaves the kept devices alone and detaches only the others"
else
    fail "release_dmg_image leaves the kept devices alone (status, detaches: $result: $(tr '\n' '|' < "$work/fake-detaches"))"
fi
# And an attach that lands on a device that was already there is not ours to
# detach: the layout is bad (nothing mounted), but no detach may be sent.
result="$(fake_run adopt assert_dmg_layout /elsewhere/mine.dmg Solador.app)"
if [[ "$result" == "1 0" ]]; then
    pass "an attach that lands on a pre-existing device does not detach it"
else
    fail "an attach that lands on a pre-existing device does not detach it (status, detaches: $result: $(tr '\n' '|' < "$work/fake-detaches"); $(cat "$work/err"))"
fi

# A device that finishes attaching AFTER a failed attach reported its error (and
# after the in-loop sweep found nothing) is caught by the pause-and-sweep at the
# end. The stub fails the attach with a non-retried error and queues a real
# attach; `sleep` performs it, and the harness performs it after the function
# returns if it is still queued, so without the final pause the device is left.
cat > "$work/late-hdiutil.sh" <<'EOF'
late_attach() {
    local n out
    if [[ -e "$STUB_PENDING" ]]; then
        rm -f "$STUB_PENDING"
        # Retried on EAGAIN, as the suite does: a concurrent suite must not turn
        # this into a case that passes because the late attach never happened.
        for n in 1 2 3 4 5; do
            if out="$(command hdiutil attach -plist -nobrowse -readonly -noverify -mountpoint "$STUB_MNT" "$STUB_DMG" 2>&1)"; then
                : > "$STUB_PENDING.done"
                return 0
            fi
            case "$out" in
                *"Resource temporarily unavailable"*) command sleep "$((n * 2))" ;;
                *) return 1 ;;
            esac
        done
        return 1
    fi
}
hdiutil() {
    if [[ "$1" == attach ]]; then
        : > "$STUB_PENDING"
        echo "hdiutil: attach failed - no mountable file systems" >&2
        return 1
    fi
    command hdiutil "$@"
}
sleep() { late_attach; }
EOF
late_rc=0
rm -f "$work/pending-attach.done"
(
    export STUB_PENDING="$work/pending-attach" STUB_DMG="$work/good.dmg"
    STUB_MNT="$(mktemp -d "$work/mnt.XXXXXX")"; export STUB_MNT
    # shellcheck source=/dev/null
    source "$work/late-hdiutil.sh"
    rc=0
    attach_dmg_readonly "$STUB_MNT" "$work/good.dmg" 2>"$work/late-err" || rc=$?
    late_attach
    exit "$rc"
) || late_rc=$?
late_left="$(leftover)"
if [[ ! -e "$work/pending-attach.done" ]]; then
    fail "a device that attaches after the last failed attempt is swept by the final pause (the late attach never happened)"
elif [[ "$late_rc" == 1 && -z "$late_left" ]]; then
    pass "a device that attaches after the last failed attempt is swept by the final pause"
else
    fail "a device that attaches after the last failed attempt is swept by the final pause (status $late_rc; left: $late_left)"
fi
release_dmg_image "$work/good.dmg" || fail "could not clean up the late-attached device"
# #642: the sweep detaches silently, so the line read before it is the only
# trace of a device that appeared during the pause; the one after reads `none`.
if grep -q "^dmg attach: after the 2 s settle, before the final sweep: devices on $work/good.dmg: /dev/disk[0-9]*; .* mounted: yes; " "$work/late-err" \
    && grep -q "^dmg attach: after the final sweep (what remains): devices on $work/good.dmg: none; " "$work/late-err"; then
    pass "a device that appeared during the final pause is named before the sweep and gone after it"
else
    fail "a device that appeared during the final pause is named before the sweep ($(cat "$work/late-err"))"
fi

# make_dmg's shape, on real devices: the image is attached, then replaced at the
# same path (rm -f, then create -ov), then checked. The earlier device keeps its
# image path and must survive the check's sweeps.
keep_mnt="$(mktemp -d "$work/mnt.XXXXXX")"
attach_dmg_readonly "$keep_mnt" "$work/good.dmg"; keep_dev="$ATTACHED_DEV"
# The replacement is written under another name and moved over the path, so the
# old inode is unlinked as make_dmg's `rm -f` does but the path is never absent:
# a leaked-fixture reaper in a concurrent suite would take the gap for a leak.
hdiutil create -volname Solador -srcfolder "$good" -ov -format UDZO "$work/replacement.dmg" >/dev/null 2>&1
mv -f "$work/replacement.dmg" "$work/good.dmg"
keep_rc=0; assert_dmg_layout "$work/good.dmg" Solador.app 2>"$work/err" || keep_rc=$?
keep_st=0; dmg_device_state "$keep_dev" "$work/good.dmg" || keep_st=$?
if [[ "$keep_rc" == 0 && "$keep_st" == 0 ]] && grep -q "$keep_dev was already attached .* left alone" "$work/err"; then
    pass "a same-path image still mounted from before is not swept by the layout check"
else
    fail "a same-path image still mounted from before is not swept by the layout check (status $keep_rc; earlier device $keep_dev state $keep_st; $(cat "$work/err"))"
fi
detach_dmg_device "$keep_dev" "$work/good.dmg" || fail "could not detach the earlier device $keep_dev"

# The EXIT trap: a child that dies mid-suite with an image attached.
probe_status=0
probe="$("${BASH:-bash}" "${BASH_SOURCE[0]}" --probe-die 2>"$work/probe-err")" || probe_status=$?
probe_work=""; probe_dev=""
read -r _ probe_work probe_dev <<< "$(printf '%s\n' "$probe" | grep '^PROBE ' || true)"
# 1 = that device is no longer attached as the probe's image (2 = unreadable).
probe_state() { local st=0; dmg_device_state "$probe_dev" "$probe_work/good.dmg" || st=$?; echo "$st"; }
if [[ "$probe_status" == 3 && -n "$probe_dev" && ! -e "$probe_work" ]] && [[ "$(probe_state)" == 1 ]]; then
    pass "the EXIT trap detaches before it deletes (a mid-suite exit leaks nothing)"
else
    fail "the EXIT trap detaches before it deletes (status $probe_status; $probe; work gone: $([[ ! -e "$probe_work" ]] && echo yes || echo no); $(cat "$work/probe-err"))"
    [[ -n "$probe_dev" ]] && detach_dmg_device "$probe_dev" "$probe_work/good.dmg" || true
fi

# The reaper rule: leaked fixtures match, nothing else does.
root="$work/root"; mkdir -p "$root/tmp.AbC123" "$root/tmp.live" "$root/plain" "$root/tmp.AbC123/deeper"
: > "$root/tmp.live/good.dmg"
reaper_cases=(
    "yes|$root/tmp.AbC123/wrong.dmg"
    "yes|$root/tmp.AbC123/full.dmg"
    "no|$root/tmp.live/good.dmg"
    "no|$root/tmp.AbC123/installer.dmg"
    "no|$root/plain/good.dmg"
    "no|$root/tmp.AbC123/deeper/good.dmg"
    "no|$HOME/Downloads/jetbrains-toolbox-3.8.1.88030-arm64.dmg"
    "no|$HOME/Downloads/good.dmg"
    "no|/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/x.asset/AssetData/Restore/090-12345-001.dmg"
    "no|/System/Library/AssetsV2/Restore/good.dmg"
    "no|/elsewhere/tmp.AbC123/good.dmg"
)
reaper_bad=""
for c in "${reaper_cases[@]}"; do
    want="${c%%|*}"; img="${c#*|}"
    got=no; dmg_is_leaked_fixture "$img" "$root" && got=yes
    [[ "$got" == "$want" ]] || reaper_bad="$reaper_bad [$want: $img]"
done
# The leaked image's directory is gone, so the spelling of the root decides:
# /var and /private/var are one place, whichever side carries the prefix.
case "$root" in
    /var/*)
        got=no; dmg_is_leaked_fixture "/private$root/tmp.AbC123/wrong.dmg" "$root" && got=yes
        [[ "$got" == yes ]] || reaper_bad="$reaper_bad [yes: /private path against a /var root]"
        got=no; dmg_is_leaked_fixture "$root/tmp.AbC123/wrong.dmg" "/private$root" && got=yes
        [[ "$got" == yes ]] || reaper_bad="$reaper_bad [yes: /var path against a /private root]"
        got=no; dmg_is_leaked_fixture "/elsewhere/tmp.AbC123/wrong.dmg" "/private$root" && got=yes
        [[ "$got" == no ]] || reaper_bad="$reaper_bad [no: foreign path against a /private root]"
        got=no; dmg_is_leaked_fixture "/private$root/tmp.live/good.dmg" "$root" && got=yes
        [[ "$got" == no ]] || reaper_bad="$reaper_bad [no: live /private path]"
        ;;
esac
if [[ -z "$reaper_bad" ]]; then pass "the reaper rule matches leaked fixtures and nothing else"; else fail "the reaper rule matches leaked fixtures and nothing else:$reaper_bad"; fi

# End to end, on a device this run attaches itself: attach, delete the backing
# file (the leak's shape), reap, and the device must be gone. A live image in
# the same root must survive the reap.
mkdir -p "$root/tmp.Leak" "$root/tmp.Live"
cp "$work/good.dmg" "$root/tmp.Leak/wrong.dmg"
cp "$work/good.dmg" "$root/tmp.Live/wrong.dmg"
leak_img="$root/tmp.Leak/wrong.dmg"; live_img="$root/tmp.Live/wrong.dmg"
attach_dmg_readonly "$(mktemp -d "$work/mnt.XXXXXX")" "$leak_img"; leak_dev="$ATTACHED_DEV"
attach_dmg_readonly "$(mktemp -d "$work/mnt.XXXXXX")" "$live_img"; live_dev="$ATTACHED_DEV"
rm -f "$leak_img"
reap_leaked_fixtures "$root" >"$work/reap-out"
leak_st=0; dmg_device_state "$leak_dev" "$leak_img" || leak_st=$?
live_st=0; dmg_device_state "$live_dev" "$live_img" || live_st=$?
if [[ "$leak_st" == 1 && "$live_st" == 0 ]] && grep -q "reaping leaked fixture device $leak_dev" "$work/reap-out"; then
    pass "the reaper detaches a leaked fixture device and leaves a live one"
else
    fail "the reaper detaches a leaked fixture device and leaves a live one ($(cat "$work/reap-out"))"
fi
detach_dmg_device "$leak_dev" "$leak_img" || true
detach_dmg_device "$live_dev" "$live_img" || true

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
# definition would be the only one ShellCheck can see, and an older ShellCheck
# (0.9.0, CI's before #548's pin) then reports the calls above as SC2218,
# "only defined later".
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

# Last: nothing this run attached is still attached (#629).
left_attached=""
if final_listing="$(dmg_attached_images)"; then
    while IFS=$'\t' read -r dev path; do
        if [[ -n "$dev" ]] && dmg_under "$path" "$work"; then left_attached="$left_attached $dev($path)"; fi
    done <<< "$final_listing"
else
    left_attached=" (could not read 'hdiutil info')"
fi
if [[ -z "$left_attached" ]]; then pass "nothing under the suite's temp dir is still attached"; else fail "devices still attached under the suite's temp dir:$left_attached"; fi

if [[ "$failures" -ne 0 ]]; then
    echo "$failures failure(s)" >&2
    exit 1
fi
echo "dmg layout: all passed"
