#!/usr/bin/env bash
#
# Redeploy (or roll back) the DevCanopy metrics agent on an existing Linux host.
#
# Unlike install.sh, this script is **non-interactive**: it never prompts for a
# token. It assumes the host is already installed (env file + systemd user unit
# present) and only swaps the binary, restarts, and verifies. Use install.sh for
# the first-time setup on a fresh host.
#
# Usage:
#   ./deploy/redeploy.sh            # build, atomic-swap, restart, verify
#   ./deploy/redeploy.sh rollback   # restore the previous binary and restart
#   ./deploy/redeploy.sh --help
#
# Why a dedicated script (the tribal knowledge this captures):
#   * The running binary can't be overwritten in place — Linux returns ETXTBSY
#     ("Text file busy"). We copy the new build to "<bin>.new" and atomically
#     rename it over the live path, which the kernel allows even while it runs.
#   * The displaced binary is kept as "<bin>.prev" so a bad build is one
#     `rollback` away instead of a from-source rebuild on the VM while
#     monitoring is down.
#   * After restart we poll /v1/health and assert it reports the version from
#     Cargo.toml, so a half-applied swap or a stale process can't pass silently.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# crate_version / verify_health are shared with install.sh, which runs the same
# served-version-vs-source assertion after its own restart.
# shellcheck source=agent/deploy/lib.sh
source "$SCRIPT_DIR/lib.sh"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="devcanopy-agent"
ENV_FILE="$HOME/.config/devcanopy-agent.env"
UNIT_NAME="$BIN_NAME"

# ---- locate the installed binary -------------------------------------------
# Mirror install.sh's layout: prefer /opt, fall back to ~/.local/bin. We resolve
# the live path from the systemd unit's ExecStart when possible so this keeps
# working even if a host used the fallback location.
resolve_install_path() {
    local unit_file="$HOME/.config/systemd/user/${UNIT_NAME}.service"
    if [ -f "$unit_file" ]; then
        local exec_path
        exec_path="$(grep -E '^ExecStart=' "$unit_file" | head -n1 | cut -d= -f2- | awk '{print $1}')"
        if [ -n "$exec_path" ]; then
            printf '%s\n' "$exec_path"
            return 0
        fi
    fi
    if [ -e "/opt/devcanopy-agent/$BIN_NAME" ]; then
        printf '%s\n' "/opt/devcanopy-agent/$BIN_NAME"
    else
        printf '%s\n' "$HOME/.local/bin/$BIN_NAME"
    fi
}

INSTALL_PATH="$(resolve_install_path)"
INSTALL_DIR="$(dirname "$INSTALL_PATH")"
PREV_BIN="${INSTALL_PATH}.prev"
NEW_BIN="${INSTALL_PATH}.new"

# `install`/`mv` into INSTALL_DIR may need sudo (e.g. /opt). Compute a prefix once.
SUDO=""
if [ ! -w "$INSTALL_DIR" ]; then
    if command -v sudo >/dev/null 2>&1; then
        SUDO="sudo"
    else
        echo "ERROR: $INSTALL_DIR is not writable and sudo is unavailable." >&2
        exit 1
    fi
fi

restart_unit() {
    systemctl --user restart "$UNIT_NAME"
}

# ---- rollback subcommand ---------------------------------------------------
do_rollback() {
    if [ ! -e "$PREV_BIN" ]; then
        echo "ERROR: no previous binary at $PREV_BIN — nothing to roll back to." >&2
        exit 1
    fi
    command -v systemctl >/dev/null 2>&1 || { echo "ERROR: systemctl not found (Linux + systemd required)." >&2; exit 1; }
    command -v curl >/dev/null 2>&1 || { echo "ERROR: curl not found (needed for health verification)." >&2; exit 1; }
    echo "==> Rolling back: restoring $PREV_BIN -> $INSTALL_PATH"

    # Swap the live binary and .prev so rollback is itself reversible: the
    # currently-live (bad) binary is preserved as the new .prev, and the next
    # rollback would roll forward to it. Stage to .new first for an ETXTBSY-safe
    # atomic rename, exactly like deploy.
    local displaced="${INSTALL_PATH}.rollback-displaced"
    if [ -e "$INSTALL_PATH" ]; then
        $SUDO cp -p "$INSTALL_PATH" "$displaced"
    fi
    $SUDO cp -p "$PREV_BIN" "$NEW_BIN"
    $SUDO mv -f "$NEW_BIN" "$INSTALL_PATH"
    if [ -e "$displaced" ]; then
        $SUDO mv -f "$displaced" "$PREV_BIN"
    fi

    restart_unit

    # The agent binary has no --version flag, so we can't statically know which
    # version .prev embeds. Verify it simply comes back online (status ok).
    if verify_health "$ENV_FILE" ""; then
        echo "==> Rollback complete. The agent is back online on the previous binary."
        echo "    The binary you rolled back over is now $PREV_BIN (re-run this to roll forward)."
    else
        echo >&2
        echo "ERROR: rollback restarted the previous binary but it did not come back online." >&2
        echo "       Inspect:  systemctl --user status $UNIT_NAME" >&2
        echo "                 journalctl --user -u $UNIT_NAME -n 50 --no-pager" >&2
        exit 1
    fi
}

# ---- deploy (default) ------------------------------------------------------
do_deploy() {
    command -v cargo >/dev/null 2>&1 || { echo "ERROR: cargo not found in PATH." >&2; exit 1; }
    command -v systemctl >/dev/null 2>&1 || { echo "ERROR: systemctl not found (Linux + systemd required)." >&2; exit 1; }
    command -v curl >/dev/null 2>&1 || { echo "ERROR: curl not found (needed for health verification)." >&2; exit 1; }

    if [ ! -f "$ENV_FILE" ]; then
        echo "ERROR: $ENV_FILE not found." >&2
        echo "       This host has not been installed yet. Run ./deploy/install.sh first" >&2
        echo "       (it provisions the token + systemd unit). redeploy.sh only upgrades" >&2
        echo "       an already-installed host and never prompts for a token." >&2
        exit 1
    fi

    local target_version
    target_version="$(crate_version "$CRATE_DIR/Cargo.toml")"
    [ -n "$target_version" ] || { echo "ERROR: could not read the [package] version from Cargo.toml." >&2; exit 1; }

    echo "==> Building release binary (target version $target_version)..."
    ( cd "$CRATE_DIR" && cargo build --release )
    local built_bin="$CRATE_DIR/target/release/$BIN_NAME"
    [ -x "$built_bin" ] || { echo "ERROR: build did not produce $built_bin" >&2; exit 1; }

    # Preserve the currently-installed binary as .prev (one-command rollback).
    if [ -e "$INSTALL_PATH" ]; then
        echo "==> Preserving current binary as $PREV_BIN"
        $SUDO cp -p "$INSTALL_PATH" "$PREV_BIN"
    else
        echo "==> No existing binary at $INSTALL_PATH (treating as fresh slot)."
    fi

    # Atomic, ETXTBSY-safe swap: stage to .new, then rename over the live path.
    # `mv` within the same directory is an atomic rename; the kernel permits
    # renaming over a running executable even when an in-place write would fail.
    echo "==> Staging new binary -> $NEW_BIN"
    $SUDO install -m 0755 "$built_bin" "$NEW_BIN"
    echo "==> Atomically swapping into place -> $INSTALL_PATH"
    $SUDO mv -f "$NEW_BIN" "$INSTALL_PATH"

    echo "==> Restarting service..."
    restart_unit

    if verify_health "$ENV_FILE" "$target_version"; then
        echo "==> Redeploy complete: $BIN_NAME is on $target_version."
        echo "    Previous binary kept at $PREV_BIN (run './deploy/redeploy.sh rollback' to revert)."
    else
        echo >&2
        echo "ERROR: redeploy verification FAILED. The new binary is live but unhealthy." >&2
        echo "       Roll back now with:  $0 rollback" >&2
        exit 1
    fi
}

# ---- arg parsing -----------------------------------------------------------
case "${1:-deploy}" in
    deploy|"")
        do_deploy
        ;;
    rollback)
        do_rollback
        ;;
    -h|--help|help)
        # Print the header comment block (lines 2–23) with the leading "#"/"# "
        # stripped. Two passes keep it portable across BSD (macOS) and GNU sed.
        sed -n '2,23p' "$0" | sed -e 's/^# //' -e 's/^#$//'
        ;;
    *)
        echo "ERROR: unknown command '$1'. Use: deploy (default) | rollback | --help" >&2
        exit 1
        ;;
esac
