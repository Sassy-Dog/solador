#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

# The cross-platform Tauri cockpit is the only app now; the original macOS app it
# grew out of was deleted once panel parity landed. exec keeps the caller's
# environment (SOLADOR_SEED_HOST, SOLADOR_STORE_DIR, ...) reaching the app.
ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"

# --release selects the cargo profile; anything else is the app's own
# argument (e.g. the --dump fixture modes documented in app/README.md).
CARGO_PROFILE="debug"
CARGO_FLAGS=()
APP_ARGS=()
for arg in "$@"; do
    case $arg in
        --release)
            CARGO_PROFILE="release"
            CARGO_FLAGS+=("--release")
            ;;
        *)
            APP_ARGS+=("$arg")
            ;;
    esac
done

log_info "Building $TAURI_PACKAGE ($CARGO_PROFILE)..."
cargo build --manifest-path "$ROOT_DIR/Cargo.toml" -p "$TAURI_PACKAGE" \
    ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"}

TAURI_BINARY="$ROOT_DIR/target/$CARGO_PROFILE/$TAURI_PACKAGE"
# What we exec, and what codesign is pointed at. Both are reassigned to the
# .app below on macOS; off macOS the bare binary stays correct.
LAUNCH_BINARY="$TAURI_BINARY"
SIGN_TARGET="$TAURI_BINARY"

# Wrap the binary in a throwaway .app so the Dock, ⌘-Tab and the menu bar
# show the mark and the product name instead of a generic executable.
#
# An icon cannot come from the binary. tauri-build embeds an Info.plist into
# __TEXT,__info_plist (that is where "Solador" in the menu bar comes from),
# but CFBundleIconFile names a file in Contents/Resources and a bare Mach-O
# has no Resources — so a Dock icon needs a real bundle, and `bundle.active`
# is false because we are not shipping from here.
#
# The binary is *launched from inside* the bundle rather than via `open`:
# macOS reads the enclosing bundle either way, and exec keeps stdout,
# stderr and the caller's environment (SOLADOR_SEED_HOST, SOLADOR_STORE_DIR,
# the --dump modes) attached to the terminal. `open` would detach all of it.
if [[ "$(uname -s)" == "Darwin" ]]; then
    # plutil reads JSON, so every value below comes from tauri.conf.json
    # rather than a second copy that drifts from it.
    TAURI_CONF="$ROOT_DIR/app/src-tauri/tauri.conf.json"
    BUNDLE_NAME="$(plutil -extract productName raw -o - "$TAURI_CONF")"
    BUNDLE_ID="$(plutil -extract identifier raw -o - "$TAURI_CONF")"
    BUNDLE_MIN_OS="$(plutil -extract 'bundle.macOS.minimumSystemVersion' raw -o - "$TAURI_CONF" 2>/dev/null || echo "14.0")"
    APP_BUNDLE="$ROOT_DIR/target/$CARGO_PROFILE/$BUNDLE_NAME.app"
    ICNS="$ROOT_DIR/app/src-tauri/icons/icon.icns"

    rm -rf "$APP_BUNDLE"
    mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
    # A copy, not a symlink: codesign will not sign a bundle whose
    # executable is a link out of it, and cargo replaces the file on every
    # relink so a hard link would go stale. On APFS this is a clone.
    cp "$TAURI_BINARY" "$APP_BUNDLE/Contents/MacOS/$TAURI_PACKAGE"

    if [[ -f "$ICNS" ]]; then
        cp "$ICNS" "$APP_BUNDLE/Contents/Resources/icon.icns"
    else
        log_warning "app/src-tauri/icons/icon.icns missing — run ./scripts/generate-icons.sh"
    fi

    cat > "$APP_BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>$BUNDLE_NAME</string>
	<key>CFBundleDisplayName</key><string>$BUNDLE_NAME</string>
	<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
	<key>CFBundleExecutable</key><string>$TAURI_PACKAGE</string>
	<key>CFBundleIconFile</key><string>icon</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>LSMinimumSystemVersion</key><string>$BUNDLE_MIN_OS</string>
	<key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
    LAUNCH_BINARY="$APP_BUNDLE/Contents/MacOS/$TAURI_PACKAGE"
    SIGN_TARGET="$APP_BUNDLE"
fi

# cargo stamps a *fresh ad-hoc* signature on every relink, and a new signing
# identity invalidates the Keychain ACLs the app's stored credentials carry —
# so every rebuild re-prompts for every item (4+ prompts per run). Re-sign with
# the stable Apple Development identity (the team `config.sh` resolves, the
# same one the original Debug build uses) so the ACLs keep matching across
# rebuilds. macOS only, and silently skipped where no identity is installed
# (CI, other machines) — an unsigned run still works, it just re-prompts.
if [[ "$(uname -s)" == "Darwin" ]]; then
    CODESIGN_IDENTITIES="$(security find-identity -v -p codesigning 2>/dev/null || true)"
    SIGN_LINES="$(printf '%s\n' "$CODESIGN_IDENTITIES" | grep -F "Apple Development" || true)"
    # Narrow to this team when we know it. `--sign "Apple Development"` is a
    # PREFIX match, so a keychain holding certs for two teams makes codesign
    # fail with an ambiguity error — and that failure lands in the `||`
    # below as a warning nobody reads, leaving the binary ad-hoc signed and
    # the Keychain prompting on every run. The exact thing this block exists
    # to prevent.
    if [[ -n "${DEVELOPMENT_TEAM:-}" ]]; then
        TEAM_LINES="$(printf '%s\n' "$SIGN_LINES" | grep -F "($DEVELOPMENT_TEAM)" || true)"
        [[ -n "$TEAM_LINES" ]] && SIGN_LINES="$TEAM_LINES"
    fi
    # The SHA-1, not the name: a hash names exactly one certificate.
    SIGN_ID="$(printf '%s\n' "$SIGN_LINES" | head -n1 | awk '{print $2}')"
    if [[ -n "$SIGN_ID" ]]; then
        # Sign the BUNDLE, not the loose binary — signing the copy inside it
        # is what the ACLs will be matched against at launch.
        #
        # `--identifier` is pinned deliberately. Left alone, codesign takes
        # the identifier from CFBundleIdentifier, which would move the
        # designated requirement from `solador-app` (derived from the bare
        # binary's filename) to `app.solador.desktop`. Keychain ACLs are
        # bound to that requirement, so the change would re-prompt for every
        # stored credential exactly once — the storm this block exists to
        # prevent. Pinning keeps every ACL already granted valid.
        #
        # A real bundled release will sign as app.solador.desktop and pay
        # that one-time cost then, deliberately, rather than surprising
        # someone mid-dev-loop.
        codesign --force --identifier "$TAURI_PACKAGE" --sign "$SIGN_ID" \
            "$SIGN_TARGET" >/dev/null 2>&1 ||
            log_warning "Could not re-sign $TAURI_PACKAGE — the Keychain may re-prompt"
    fi
fi

log_info "Running $TAURI_PACKAGE ($CARGO_PROFILE)..."
exec "$LAUNCH_BINARY" ${APP_ARGS[@]+"${APP_ARGS[@]}"}
