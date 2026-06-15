#!/usr/bin/env bash
#
# Install the DevCanopy metrics agent as a user systemd service (Linux).
#
# Usage:
#   ./deploy/install.sh
#
# What it does:
#   1. Builds the release binary (cargo build --release).
#   2. Installs it to /opt/devcanopy-agent/ (falls back to ~/.local/bin if /opt
#      is not writable and sudo is unavailable).
#   3. Writes the env file ~/.config/devcanopy-agent.env with the bearer token
#      (prompts for it, or reuses an existing one).
#   4. Installs + enables the user systemd unit and starts it.
#
# Re-running is safe (idempotent): it rebuilds, replaces the binary, and restarts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_NAME="devcanopy-agent"
ENV_FILE="$HOME/.config/devcanopy-agent.env"
UNIT_SRC="$SCRIPT_DIR/${BIN_NAME}.service"
UNIT_DST="$HOME/.config/systemd/user/${BIN_NAME}.service"

# ---- preflight -------------------------------------------------------------
command -v cargo >/dev/null 2>&1 || { echo "ERROR: cargo not found in PATH." >&2; exit 1; }
command -v systemctl >/dev/null 2>&1 || { echo "ERROR: systemctl not found (Linux + systemd required)." >&2; exit 1; }

# ---- build -----------------------------------------------------------------
echo "==> Building release binary..."
( cd "$CRATE_DIR" && cargo build --release )
BUILT_BIN="$CRATE_DIR/target/release/$BIN_NAME"
[ -x "$BUILT_BIN" ] || { echo "ERROR: build did not produce $BUILT_BIN" >&2; exit 1; }

# ---- install binary --------------------------------------------------------
INSTALL_DIR="/opt/devcanopy-agent"
if mkdir -p "$INSTALL_DIR" 2>/dev/null && [ -w "$INSTALL_DIR" ]; then
    install -m 0755 "$BUILT_BIN" "$INSTALL_DIR/$BIN_NAME"
elif command -v sudo >/dev/null 2>&1; then
    echo "==> /opt not writable; using sudo to install to $INSTALL_DIR"
    sudo mkdir -p "$INSTALL_DIR"
    sudo install -m 0755 "$BUILT_BIN" "$INSTALL_DIR/$BIN_NAME"
else
    INSTALL_DIR="$HOME/.local/bin"
    mkdir -p "$INSTALL_DIR"
    install -m 0755 "$BUILT_BIN" "$INSTALL_DIR/$BIN_NAME"
    echo "==> Installed to $INSTALL_DIR (no /opt access)."
    echo "    NOTE: edit $UNIT_DST ExecStart to $INSTALL_DIR/$BIN_NAME"
fi
echo "==> Binary installed: $INSTALL_DIR/$BIN_NAME"

# ---- env file (token) ------------------------------------------------------
mkdir -p "$(dirname "$ENV_FILE")"
EXISTING_TOKEN=""
if [ -f "$ENV_FILE" ]; then
    EXISTING_TOKEN="$(grep -E '^DEVCANOPY_AGENT_TOKEN=' "$ENV_FILE" | head -n1 | cut -d= -f2- || true)"
fi

if [ -n "$EXISTING_TOKEN" ]; then
    echo "==> Reusing existing token from $ENV_FILE"
    TOKEN="$EXISTING_TOKEN"
else
    # Generate a strong default the user can accept by pressing Enter.
    GEN_TOKEN="$( (openssl rand -hex 32 2>/dev/null) || head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    # -s: do NOT echo the secret to the terminal (no scrollback/transcript leak).
    printf "Enter bearer token [press Enter to generate]: "
    read -rs TOKEN || true
    printf '\n'  # read -s swallows the trailing newline; restore it.
    TOKEN="${TOKEN:-$GEN_TOKEN}"
fi

# ---- bind address ----------------------------------------------------------
# Default to the host's Tailscale IP so the agent only listens on the tailnet,
# never the public NIC. Honor a pre-set DEVCANOPY_AGENT_BIND (e.g. to opt into
# 0.0.0.0 behind a firewall) and reuse an existing value from the env file.
EXISTING_BIND=""
if [ -f "$ENV_FILE" ]; then
    EXISTING_BIND="$(grep -E '^DEVCANOPY_AGENT_BIND=' "$ENV_FILE" | head -n1 | cut -d= -f2- || true)"
fi

detect_tailscale_ip() {
    # Prefer the tailscale CLI; fall back to scanning the tailscale0 interface.
    if command -v tailscale >/dev/null 2>&1; then
        tailscale ip -4 2>/dev/null | head -n1 && return 0
    fi
    if command -v ip >/dev/null 2>&1; then
        ip -4 -o addr show tailscale0 2>/dev/null \
            | grep -oE 'inet [0-9.]+' | awk '{print $2}' | head -n1 && return 0
    fi
    return 1
}

BIND="${DEVCANOPY_AGENT_BIND:-$EXISTING_BIND}"
if [ -z "$BIND" ]; then
    BIND="$(detect_tailscale_ip || true)"
fi
if [ -z "$BIND" ]; then
    echo "ERROR: could not detect a Tailscale IP for DEVCANOPY_AGENT_BIND." >&2
    echo "       Bring up Tailscale, or set DEVCANOPY_AGENT_BIND explicitly" >&2
    echo "       (e.g. DEVCANOPY_AGENT_BIND=0.0.0.0 ./deploy/install.sh — only behind a firewall)." >&2
    exit 1
fi
echo "==> Binding to $BIND (tailnet interface)"

PORT="${DEVCANOPY_AGENT_PORT:-7878}"
umask 077
cat > "$ENV_FILE" <<EOF
DEVCANOPY_AGENT_TOKEN=$TOKEN
DEVCANOPY_AGENT_BIND=$BIND
DEVCANOPY_AGENT_PORT=$PORT
EOF
chmod 600 "$ENV_FILE"
echo "==> Wrote $ENV_FILE (token + bind + port, mode 600)"

# ---- systemd user unit -----------------------------------------------------
mkdir -p "$(dirname "$UNIT_DST")"
cp "$UNIT_SRC" "$UNIT_DST"
systemctl --user daemon-reload
systemctl --user enable "$BIN_NAME"
# `restart` (not `enable --now`) so a re-run actually picks up the rebuilt binary —
# `--now` only starts a stopped unit, it won't restart a running one.
systemctl --user restart "$BIN_NAME"

# Survive logout / start on boot.
if command -v loginctl >/dev/null 2>&1; then
    loginctl enable-linger "$USER" 2>/dev/null || \
        echo "    (could not enable-linger; run 'sudo loginctl enable-linger $USER' for boot start)"
fi

echo
echo "==> Done. Status:"
systemctl --user --no-pager status "$BIN_NAME" || true
echo
echo "Verify locally (sources the token from the env file — nothing secret printed):"
echo "  set -a; . $ENV_FILE; set +a"
echo "  curl -s -H \"Authorization: Bearer \$DEVCANOPY_AGENT_TOKEN\" \"\$DEVCANOPY_AGENT_BIND:\$DEVCANOPY_AGENT_PORT/v1/health\""
echo
echo "Bearer token (give this to DevCanopy): stored in $ENV_FILE (mode 600)."
echo "  Last 4 chars: ...${TOKEN: -4}   — read the full value with:"
echo "  grep '^DEVCANOPY_AGENT_TOKEN=' $ENV_FILE | cut -d= -f2-"
