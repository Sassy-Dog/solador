#!/usr/bin/env bash
set -euo pipefail

# Build the per-host metrics agent for its published targets (#390).
#
#   ./dev agent                      the targets this host can build
#   ./dev agent --targets "a b"      exactly these triples
#   ./dev agent --sign               …and minisign each artifact
#   ./dev agent --out-dir DIR        where the named artifacts land
#                                    (default: target/agent-release/)
#
# Four targets ship, and `scripts/config.sh` names them because a triple is part
# of the OUTPUT PATH — `--target` moves cargo's output under target/<triple>/,
# so this script and `.github/workflows/release.yml` have to agree about where
# each binary landed.
#
#   x86_64-unknown-linux-musl   static; runs on any distro
#   aarch64-unknown-linux-musl  static; ARM servers, Pi-class hosts
#   aarch64-apple-darwin        Apple Silicon
#   x86_64-apple-darwin         Intel Macs
#
# Why musl and not gnu: a dynamically linked gnu build resolves the *builder's*
# glibc and dies on any older host with `GLIBC_2.xx not found`. That failure is
# invisible to whoever built it and fatal to a stranger's first install, which
# is the exact class of problem public distribution exists to solve. So this
# script asserts staticness out of the artifact rather than trusting the target
# name to have implied it.
#
# What this deliberately does NOT do is upload anything. It builds, checks, and
# optionally signs; `release.yml` attaches the results to the same draft release
# the cockpit already publishes to — one tag, one release, both products, and no
# second release train.

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
source "$SCRIPT_DIR/lib.sh"
source "$SCRIPT_DIR/config.sh"

ROOT_DIR="$( cd "$SCRIPT_DIR/.." && pwd )"

TARGETS=""
# Under target/, which is already ignored — a build must not leave artifacts
# loose in the tree for the next `add -A` to pick up.
OUT_DIR="$ROOT_DIR/target/agent-release"
WANT_SIGN=false
SIGN_ONLY=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --sign-only)
            # Sign artifacts that are already in --out-dir, building nothing.
            #
            # This exists so the release can sign the EXACT files it verified.
            # `--version` has to be executed on a runner matching each target,
            # which is four machines none of which may hold the signing key, so
            # build → verify → sign are three stages and only the last one is
            # given the credential. Same signing code either way — a second
            # implementation in YAML is how the local path and the release path
            # would come to disagree.
            SIGN_ONLY=true
            WANT_SIGN=true
            shift
            ;;
        --targets)
            TARGETS="${2:-}"
            [[ -n "$TARGETS" ]] || { log_error "--targets needs a space-separated list of triples"; exit 2; }
            shift 2
            ;;
        --out-dir)
            OUT_DIR="${2:-}"
            [[ -n "$OUT_DIR" ]] || { log_error "--out-dir needs a path"; exit 2; }
            shift 2
            ;;
        --sign)
            # Off unless asked for, like `build.sh --sign`: a contributor
            # without the key must still be able to produce binaries.
            WANT_SIGN=true
            shift
            ;;
        -h|--help)
            sed -n '5,32p' "$0" | sed -e 's/^# //' -e 's/^#$//'
            exit 0
            ;;
        *)
            log_error "unknown argument: $1"
            exit 2
            ;;
    esac
done

# Default to the targets this host can actually produce. macOS binaries need
# Apple's linker and SDK, so a Linux box cannot make them; the Linux targets go
# through zig and can be built anywhere.
if [[ -z "$TARGETS" ]]; then
    case "$(uname -s)" in
        Darwin) TARGETS="$AGENT_MACOS_TARGETS $AGENT_LINUX_TARGETS" ;;
        Linux)  TARGETS="$AGENT_LINUX_TARGETS" ;;
        *)
            log_error "no agent targets are buildable on $(uname -s) — pass --targets explicitly"
            exit 1
            ;;
    esac
fi

# ---------------------------------------------------------------------------
# Version
# ---------------------------------------------------------------------------

# ONE call to the one owner. `agent/build.rs` compiles the same number in (via
# crates/buildversion, which shells out to this same script), and the artifact
# is checked against this value below rather than assumed to have got it — the
# same derive-then-assert standard the macOS bundle's plist keys are held to.
#
# An explicit MARKETING_VERSION wins here exactly as it does there, because
# `publish.sh` pins it so the artifact carries the version the *tag* carries.
MARKETING_VERSION="${MARKETING_VERSION:-$(bash "$SCRIPT_DIR/get-version-info.sh" --version)}"
export MARKETING_VERSION
if [[ -z "$MARKETING_VERSION" ]]; then
    log_error "could not derive a version — is this a full git checkout? (a shallow clone cannot count this month's commits; see docs/VERSIONING.md)"
    exit 1
fi

# ---------------------------------------------------------------------------
# Toolchains
# ---------------------------------------------------------------------------

is_linux_target() {
    [[ "$1" == *-linux-musl ]]
}

ensure_rust_target() {
    local triple="$1"
    if rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
        log_debug "rust target $triple already installed"
        return 0
    fi
    log_info "Installing rust target $triple"
    rustup target add "$triple"
}

# cargo-zigbuild, from PyPI, into a venv this script owns.
#
# PyPI rather than crates.io on purpose: the `cargo-zigbuild` wheel depends on
# `ziglang`, so one pin brings the driver AND the linker at a pairing its
# publisher tested. `cargo install cargo-zigbuild` pins only the driver and
# leaves zig to whatever is on PATH, which is the half that does the linking.
#
# A venv rather than `pip install --user`: modern distros mark the system
# interpreter externally-managed (PEP 668) and refuse, and a venv also keeps
# this out of any shared site-packages.
ZIG_VENV="$ROOT_DIR/target/.zigbuild-venv"

ensure_zigbuild() {
    local installed=""
    if [[ -x "$ZIG_VENV/bin/cargo-zigbuild" ]]; then
        # `|| true`: under `set -euo pipefail` a failing command substitution in
        # an assignment takes the whole script down, silently when its stderr is
        # discarded. A venv holding a cargo-zigbuild too old to understand this
        # invocation is a case to REPORT and reinstall over, not to die on.
        installed="$("$ZIG_VENV/bin/cargo-zigbuild" --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    fi
    if [[ "$installed" == "$CARGO_ZIGBUILD_VERSION" ]]; then
        log_debug "cargo-zigbuild $installed already installed"
        return 0
    fi

    command_exists python3 || { log_error "python3 not found — needed to install the pinned cargo-zigbuild $CARGO_ZIGBUILD_VERSION"; exit 1; }
    log_info "Installing cargo-zigbuild $CARGO_ZIGBUILD_VERSION (+ ziglang) into $ZIG_VENV"
    python3 -m venv "$ZIG_VENV"
    "$ZIG_VENV/bin/pip" install --quiet --upgrade pip
    "$ZIG_VENV/bin/pip" install --quiet "cargo-zigbuild==$CARGO_ZIGBUILD_VERSION"

    installed="$("$ZIG_VENV/bin/cargo-zigbuild" --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    if [[ "$installed" != "$CARGO_ZIGBUILD_VERSION" ]]; then
        log_error "cargo-zigbuild is '${installed:-<missing>}' after installing $CARGO_ZIGBUILD_VERSION"
        exit 1
    fi
}

# The pinned minisign signer. Same shape as build.sh's `ensure_tauri_cli`: the
# check is on the INSTALLED version, not on presence, so a machine carrying some
# other project's rsign does not sign a release with it.
ensure_rsign() {
    local installed=""
    if command_exists rsign; then
        # `|| true` for the reason ensure_zigbuild states above.
        installed="$(rsign --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    fi
    if [[ "$installed" == "$RSIGN_VERSION" ]]; then
        log_debug "rsign $installed already installed"
        return 0
    fi
    if [[ -n "$installed" ]]; then
        log_warning "rsign $installed found, this repo pins $RSIGN_VERSION — replacing it"
    else
        log_info "rsign not found — installing the pinned $RSIGN_VERSION"
    fi
    cargo install --locked "rsign2@$RSIGN_VERSION"
    installed="$(rsign --version 2>/dev/null | awk 'NR == 1 { print $2 }')" || true
    if [[ "$installed" != "$RSIGN_VERSION" ]]; then
        log_error "rsign is '${installed:-<missing>}' after installing $RSIGN_VERSION — is ~/.cargo/bin on PATH?"
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Assertions on the artifact
# ---------------------------------------------------------------------------

# Static, asserted out of the ELF rather than inferred from the target name.
#
# TWO accepted spellings, and this is the trap: rustc's musl targets produce a
# static PIE, which `file` describes as "static-pie linked", not "statically
# linked". Grepping only for the latter rejects every correct build. What both
# actually mean is "needs no dynamic loader", so where readelf is available this
# also asserts the thing that is really at stake — no PT_INTERP segment, i.e.
# no `ld-musl-*.so` for the kernel to go find on a host that has none.
assert_static() {
    local bin="$1" desc readelf_bin=""

    command_exists file || { log_error "'file' not found — cannot verify $bin is statically linked, and an unverified Linux artifact is exactly what #390 refuses to publish"; exit 1; }
    desc="$(file -b "$bin")"
    case "$desc" in
        *"statically linked"*|*"static-pie linked"*) ;;
        *)
            log_error "$(basename "$bin") is not statically linked: $desc"
            log_error "a dynamically linked build dies on any host older than this one with 'GLIBC_2.xx not found'"
            exit 1
            ;;
    esac

    for candidate in readelf llvm-readelf; do
        if command_exists "$candidate"; then
            readelf_bin="$candidate"
            break
        fi
    done
    if [[ -z "$readelf_bin" ]]; then
        log_warning "neither readelf nor llvm-readelf found — checked '$desc' only, not the absence of a PT_INTERP segment"
    elif "$readelf_bin" -l "$bin" 2>/dev/null | grep -q "INTERP"; then
        log_error "$(basename "$bin") carries a PT_INTERP segment — it wants a dynamic loader despite '$desc'"
        exit 1
    fi

    log_success "$(basename "$bin"): $desc"
}

# Can this host execute a binary built for `$1`?
#
# Used to decide whether the version can be read back HERE. It is not the
# release gate: `release.yml` runs `--version` for every target on a runner
# matching it, because "the artifact starts" is a claim only a matching machine
# can make and an artifact that does not start is worse than no artifact.
host_can_run() {
    local triple="$1" host
    host="$(rustc -vV | awk '/^host: / { print $2 }')"
    [[ "$triple" == "$host" ]]
}

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------

# Build every triple in $TARGETS, appending each named artifact to `built`.
build_targets() {
    local need_zigbuild=false triple bin got artifact

    for triple in $TARGETS; do
        if is_linux_target "$triple"; then
            need_zigbuild=true
        fi
    done
    $need_zigbuild && ensure_zigbuild

    log_info "Building $AGENT_PACKAGE $MARKETING_VERSION for: $TARGETS"

    for triple in $TARGETS; do
        ensure_rust_target "$triple"

        if is_linux_target "$triple"; then
            # zig as the linker. BOTH Linux targets go through it, not only the
            # cross one: one code path for Linux is what keeps "it worked on the
            # x86 runner" from being a different build than the ARM artifact.
            log_info "cargo zigbuild --target $triple"
            PATH="$ZIG_VENV/bin:$PATH" cargo zigbuild --locked --release \
                --manifest-path "$ROOT_DIR/Cargo.toml" -p "$AGENT_PACKAGE" --target "$triple"
        else
            # Apple's own toolchain cross-links between the two darwin triples
            # natively, so zig has nothing to add here and would only bring its
            # SDK handling into a path that does not need it.
            log_info "cargo build --target $triple"
            cargo build --locked --release \
                --manifest-path "$ROOT_DIR/Cargo.toml" -p "$AGENT_PACKAGE" --target "$triple"
        fi

        bin="$ROOT_DIR/target/$triple/release/$AGENT_PACKAGE"
        if [[ ! -x "$bin" ]]; then
            log_error "the build reported success but produced no binary at $bin"
            exit 1
        fi

        is_linux_target "$triple" && assert_static "$bin"

        # Read the version back OUT of the binary wherever this host can run it.
        # `agent/build.rs` compiles it in from git; nothing forces that to equal
        # what this script derived, so assert it rather than assume it.
        if host_can_run "$triple"; then
            got="$("$bin" --version)"
            if [[ "$got" != "$MARKETING_VERSION" ]]; then
                log_error "$triple reports version '$got', expected '$MARKETING_VERSION'"
                log_error "(agent/build.rs derives it from git; a stale target dir, or a MARKETING_VERSION pin seen by only one of the two, lands here)"
                exit 1
            fi
            log_success "$triple reports $got"
        else
            log_info "$triple cannot be executed on this host — release.yml runs --version for it on a matching runner"
        fi

        artifact="$OUT_DIR/$AGENT_PACKAGE-$MARKETING_VERSION-$triple"
        # Raw binaries, not tarballs, and that is load-bearing rather than lazy:
        # the update feed compares the published artifact's content hash against
        # the INSTALLED binary to decide whether a release actually changed the
        # agent. An archive hashes differently from the file inside it, so it
        # would force every host to download and unpack before it could answer
        # the question the hash exists to answer without downloading.
        install -m 0755 "$bin" "$artifact"
        built+=("$artifact")
    done
}

# Collect artifacts already in $OUT_DIR instead of rebuilding them. The version
# prefix is the filter: a leftover artifact from an earlier version in the same
# directory must not pick up a signature under this release's name.
collect_existing() {
    local artifact
    shopt -s nullglob
    for artifact in "$OUT_DIR/$AGENT_PACKAGE-$MARKETING_VERSION-"*; do
        [[ "$artifact" == *.minisig ]] && continue
        built+=("$artifact")
    done
    shopt -u nullglob
    if [[ ${#built[@]} -eq 0 ]]; then
        log_error "--sign-only found no $AGENT_PACKAGE-$MARKETING_VERSION-* artifacts in $OUT_DIR"
        log_error "nothing to sign is a failure, not a no-op — it would publish a release whose binaries are unsigned"
        exit 1
    fi
    log_info "Signing ${#built[@]} existing artifact(s) in $OUT_DIR"
}

mkdir -p "$OUT_DIR"

built=()

if [[ "$WANT_SIGN" == true ]]; then
    ensure_rsign
fi

if [[ "$SIGN_ONLY" == true ]]; then
    collect_existing
else
    build_targets
fi


# ---------------------------------------------------------------------------
# Sign
# ---------------------------------------------------------------------------

if [[ "$WANT_SIGN" == true ]]; then
    key="${SOLADOR_AGENT_SIGNING_KEY:-}"
    if [[ -z "$key" || ! -f "$key" ]]; then
        log_error "--sign needs SOLADOR_AGENT_SIGNING_KEY to point at the agent's minisign secret key file"
        log_error "it lives in CI secrets and nowhere else; see docs/AGENT-DISTRIBUTION.md §5"
        exit 1
    fi
    pubkey="$ROOT_DIR/$AGENT_SIGNING_PUBKEY"
    [[ -f "$pubkey" ]] || { log_error "missing $AGENT_SIGNING_PUBKEY — the committed public half of the agent keypair"; exit 1; }

    for artifact in "${built[@]}"; do
        name="$(basename "$artifact")"
        rm -f "$artifact.minisig"
        # The trusted comment is covered by the signature, so putting the
        # artifact's identity in it means a signature cannot be lifted onto a
        # different target's binary and still describe itself correctly.
        #
        # `-W` and `</dev/null`: the CI key is unencrypted (the secret store is
        # the protection), and a signer that falls back to a password prompt on
        # a runner hangs until the job times out instead of failing.
        rsign sign -W -s "$key" -x "$artifact.minisig" \
            -t "$name" -c "solador-agent release signature" "$artifact" </dev/null >/dev/null

        # Verified against the COMMITTED public key, not against the key that
        # just signed. That is the whole check: it proves the private key in CI
        # is the one this repo publishes, so a mis-provisioned secret fails the
        # release instead of shipping signatures nobody can check.
        if ! rsign verify -q -p "$pubkey" -x "$artifact.minisig" "$artifact"; then
            log_error "$name does not verify under $AGENT_SIGNING_PUBKEY"
            log_error "the signing key is not the published keypair's private half"
            exit 1
        fi
        log_success "Signed $name (verified under $(head -n1 "$pubkey" | sed 's/^untrusted comment: //'))"
    done
fi

echo
log_success "$AGENT_PACKAGE $MARKETING_VERSION → $OUT_DIR"
for artifact in "${built[@]}"; do
    echo "  $(basename "$artifact")  ($(du -h "$artifact" | cut -f1))"
done
