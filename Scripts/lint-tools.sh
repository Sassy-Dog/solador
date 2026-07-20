#!/usr/bin/env bash
# Pinned SwiftLint/SwiftFormat provisioning — the local mirror of CI's
# "Install (pinned)" steps (.github/workflows/ci.yml). ci.yml stays the single
# source of truth for the version pins; this helper downloads the same release
# binaries into build/lint-tools/<tool>-<version>/ and prints their absolute
# paths, so ./dev lint and ./dev format run EXACTLY what CI runs. There is
# deliberately no fallback to a PATH/Homebrew install: a floating install
# silently un-pinning the version is the failure mode this file exists to kill
# (see PR #111 — the same shadow bug in CI itself).
#
# Sourced by lint.sh / format.sh after lib.sh, with cwd at the repo root.
# Cached binaries live under build/ (gitignored); a pin bump in ci.yml lands in
# a fresh version-keyed directory, so stale caches can never be picked up.

LINT_TOOLS_DIR="build/lint-tools"
LINT_TOOLS_CI_FILE=".github/workflows/ci.yml"

# Reads a version pin (e.g. SWIFTLINT_VERSION) out of ci.yml.
lint_tools_pin() {
    grep -E "$1:" "$LINT_TOOLS_CI_FILE" 2>/dev/null | grep -oE "[0-9]+\.[0-9]+\.[0-9]+" | head -1
}

# Prints the absolute path of the pinned SwiftLint, downloading it on first
# use. Fails (returns 1) when the pin can't be read or satisfied. Logs go to
# stderr so callers can command-substitute the path.
ensure_pinned_swiftlint() {
    local version dir bin
    version=$(lint_tools_pin "SWIFTLINT_VERSION")
    if [[ -z "$version" ]]; then
        log_error "no SWIFTLINT_VERSION pin found in $LINT_TOOLS_CI_FILE" >&2
        return 1
    fi
    dir="$LINT_TOOLS_DIR/swiftlint-$version"
    bin="$dir/swiftlint"
    if [[ ! -x "$bin" ]]; then
        log_info "Downloading pinned SwiftLint $version (one-time, cached in $dir)…" >&2
        mkdir -p "$dir"
        if ! curl -fsSL -o "$dir/swiftlint.zip" \
            "https://github.com/realm/SwiftLint/releases/download/${version}/portable_swiftlint.zip"; then
            log_error "SwiftLint $version download failed and no cached copy exists at $bin" >&2
            return 1
        fi
        unzip -o -q "$dir/swiftlint.zip" -d "$dir" && rm -f "$dir/swiftlint.zip"
    fi
    if [[ "$("$bin" version)" != "$version" ]]; then
        log_error "cached swiftlint reports $("$bin" version), expected $version — delete $dir and retry" >&2
        return 1
    fi
    printf '%s/%s\n' "$PWD" "$bin"
}

# Prints the absolute path of the pinned SwiftFormat, downloading it on first
# use. Same contract as ensure_pinned_swiftlint.
ensure_pinned_swiftformat() {
    local version dir bin found
    version=$(lint_tools_pin "SWIFTFORMAT_VERSION")
    if [[ -z "$version" ]]; then
        log_error "no SWIFTFORMAT_VERSION pin found in $LINT_TOOLS_CI_FILE" >&2
        return 1
    fi
    dir="$LINT_TOOLS_DIR/swiftformat-$version"
    bin="$dir/swiftformat"
    if [[ ! -x "$bin" ]]; then
        log_info "Downloading pinned SwiftFormat $version (one-time, cached in $dir)…" >&2
        mkdir -p "$dir/extract"
        if ! curl -fsSL -o "$dir/swiftformat.zip" \
            "https://github.com/nicklockwood/SwiftFormat/releases/download/${version}/swiftformat.zip"; then
            log_error "SwiftFormat $version download failed and no cached copy exists at $bin" >&2
            return 1
        fi
        unzip -o -q "$dir/swiftformat.zip" -d "$dir/extract"
        # The release zip nests the binary (artifact-bundle layout varies by
        # release) — locate it the same way CI does.
        found=$(find "$dir/extract" -name swiftformat -type f -perm +111 | head -1)
        if [[ -z "$found" ]]; then
            log_error "no swiftformat binary inside the $version release zip" >&2
            return 1
        fi
        mv "$found" "$bin"
        rm -rf "$dir/extract" "$dir/swiftformat.zip"
    fi
    if [[ "$("$bin" --version)" != "$version" ]]; then
        log_error "cached swiftformat reports $("$bin" --version), expected $version — delete $dir and retry" >&2
        return 1
    fi
    printf '%s/%s\n' "$PWD" "$bin"
}
