#!/bin/sh
# Install kaed as a systemd user service. Idempotent; safe to re-run.
#
# kai's first install was hand-typed — `install -m755`, a heredoc'd unit
# file, a `sed` to add ExecReload, `head -c 48 /dev/urandom` for the token.
# Doing that twice more by hand is how three hosts end up subtly different
# from each other (korg #923).
#
# Where the binary and the unit file come from — two modes, same install:
#
#   --from-store   fetch a published version from the homelab package store
#                  (k-homelab docs/deploying.md). Needs nothing on this host
#                  but curl and sha256sum: no checkout, no cargo. This is
#                  how the fleet deploys (korg #1015).
#   (default)      build from this checkout with cargo.
#
# What this owns:      the binary, `kaed-new-token`, the unit file, and
#                      (only if absent) a starter config.
# What it never
# touches:             the token, and any config that already exists.
# What it deliberately
# leaves alone:        `tailscale serve` and per-host config content —
#                      those belong to k-homelab's recipes and to you.
#
# Usage:
#   deploy/install.sh [--dry-run] [--no-build] [--bin PATH]
#   deploy/install.sh --from-store [--store URL] [--version VER] [--dry-run]
#
#   --dry-run     print what would happen, change nothing. In store mode it
#                 still fetches and verifies into a temp dir — "would this
#                 deploy work" should include "is that version published,
#                 and does it still match its checksum".
#   --no-build    do not run cargo; use an existing release binary
#   --bin PATH    install this prebuilt binary (implies --no-build)
#   --from-store  fetch the binary, unit file, config template and
#                 new-token.sh from the package store instead of this
#                 checkout
#   --store URL   store base URL, e.g. https://host:4880. Also read from
#                 $KAED_STORE_URL. There is no default, on purpose: a
#                 guessed hostname fails later as a confusing curl error
#                 instead of naming what you forgot to set.
#   --version VER published version to install. Default is the store's
#                 `latest` pointer; naming an older version is the rollback.
#
# Bootstrapping a host with no checkout: this script is published into every
# artifact directory beside the binary, so fetch it and CHECK it rather than
# piping curl into sh —
#
#   base="$KAED_STORE_URL/artifacts/kaed"
#   v=$(curl -fsS "$base/latest")
#   curl -fsS -O "$base/$v/install.sh"
#   curl -fsS "$base/$v/SHA256SUMS" | grep ' install.sh$' | sha256sum -c -
#   sh install.sh --from-store --version "$v"

set -eu

DRY_RUN=0
NO_BUILD=0
BIN_SRC=""
FROM_STORE=0
STORE_URL="${KAED_STORE_URL:-}"
STORE_VERSION=""

say() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '[dry-run] %s\n' "$*"
    else
        printf '+ %s\n' "$*"
    fi
}

run() {
    say "$*"
    if [ "$DRY_RUN" -eq 0 ]; then
        eval "$@"
    fi
}

note() { printf '  %s\n' "$*"; }
fail() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

usage() {
    printf 'usage: %s [--dry-run] [--no-build] [--bin PATH]\n' "$0" >&2
    printf '       %s --from-store [--store URL] [--version VER] [--dry-run]\n' "$0" >&2
}

need_arg() { [ "$2" -gt 1 ] || { usage; fail "$1 needs a value"; }; }

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --no-build) NO_BUILD=1 ;;
        --from-store) FROM_STORE=1 ;;
        --bin)
            need_arg --bin $#
            shift
            BIN_SRC="$1"
            NO_BUILD=1
            ;;
        --store)
            need_arg --store $#
            shift
            STORE_URL="$1"
            ;;
        --version)
            need_arg --version $#
            shift
            STORE_VERSION="$1"
            ;;
        # Print the header block, so the help cannot drift from the file.
        -h|--help) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 0 ;;
        *) usage; fail "unknown option: $1" ;;
    esac
    shift
done

if [ "$FROM_STORE" -eq 1 ] && { [ "$NO_BUILD" -eq 1 ] || [ -n "$BIN_SRC" ]; }; then
    fail "--from-store conflicts with --no-build/--bin: pick one source for the binary"
fi
if [ "$FROM_STORE" -eq 0 ] && [ -n "$STORE_VERSION" ]; then
    fail "--version names a published version and only means something with --from-store"
fi

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd -P)
REPO_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd -P)

BIN_DST_DIR="$HOME/.local/bin"
BIN_DST="$BIN_DST_DIR/kaed"
TOKEN_TOOL_DST="$BIN_DST_DIR/kaed-new-token"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_DST="$UNIT_DIR/kaed.service"
CONFIG_DIR="$HOME/.config/kaed"
CONFIG_DST="$CONFIG_DIR/config.toml"

# --- 0. Pre-flight --------------------------------------------------------

command -v systemctl >/dev/null 2>&1 \
    || fail "systemctl not found; this script installs a systemd user service"

if [ "$FROM_STORE" -eq 1 ]; then
    for cmd in curl sha256sum; do
        command -v "$cmd" >/dev/null 2>&1 \
            || fail "$cmd not found on this host; --from-store needs it"
    done
    [ -n "$STORE_URL" ] || fail \
        "no package store URL — pass --store URL or set KAED_STORE_URL (e.g. https://<host>:4880)"
    STORE_URL=${STORE_URL%/}
fi

# --- 1. Where the assets come from ----------------------------------------

# Everything installed below is read from $ASSET_DIR: this checkout's
# `deploy/`, or a verified temp copy of one published artifact directory.
# Keeping it to one variable is what makes the two modes the same install.
ASSET_DIR="$SCRIPT_DIR"

if [ "$FROM_STORE" -eq 1 ]; then
    # The published filename carries the target arch, so a wrong-arch host
    # gets a 404 naming what it asked for rather than an ELF that will not
    # exec.
    SUFFIX="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
    STORE_BASE="$STORE_URL/artifacts/kaed"

    if [ -z "$STORE_VERSION" ]; then
        latest=$(curl -fsS "$STORE_BASE/latest") \
            || fail "cannot read $STORE_BASE/latest — is the store reachable from here?"
        STORE_VERSION=$(printf '%s' "$latest" | tr -d '[:space:]')
        [ -n "$STORE_VERSION" ] || fail "the latest pointer at $STORE_BASE/latest is empty"
        note "resolved latest = $STORE_VERSION"
    fi

    VERSION_URL="$STORE_BASE/$STORE_VERSION"
    WORK=$(mktemp -d)
    trap 'rm -rf "$WORK"' EXIT INT TERM

    printf 'fetching kaed %s (%s) from %s\n' "$STORE_VERSION" "$SUFFIX" "$STORE_URL"

    SUMS=$(curl -fsS "$VERSION_URL/SHA256SUMS") \
        || fail "cannot read $VERSION_URL/SHA256SUMS — is $STORE_VERSION published?"

    # Fetch and verify everything before installing anything: a checksum
    # failure on the unit file must not leave a new binary already in place.
    # A version published before the bundle existed (kaed 005) holds only the
    # binary, and fails here on the first missing file. That is the right
    # answer: mixing a new unit file with an old binary would be worse, and a
    # clone-less host has nothing to fall back to. Roll back with `.prev` or
    # `--bin` instead.
    for f in "kaed-$SUFFIX" kaed.service config.example.toml new-token.sh; do
        curl -fsS -o "$WORK/$f" "$VERSION_URL/$f" \
            || fail "fetch failed: $VERSION_URL/$f — is $STORE_VERSION published, for $SUFFIX, with a full deploy bundle?"
        line=$(printf '%s\n' "$SUMS" | grep -E "[[:space:]][*]?$f\$" | head -1)
        [ -n "$line" ] || fail "$f is not listed in $VERSION_URL/SHA256SUMS"
        ( cd "$WORK" && printf '%s\n' "$line" | sha256sum -c --status - ) \
            || fail "checksum MISMATCH for $f — refusing to install"
        note "$f  checksum OK"
    done

    BIN_SRC="$WORK/kaed-$SUFFIX"
    chmod 0755 "$BIN_SRC"

    # The checksum proves the transfer; this proves the *label*. A binary
    # published under the wrong version installs cleanly and then lies to
    # `kaed --version` — which is precisely the signal k-homelab's
    # min_build_date floor reads. `just publish` derives the published
    # version the same way, from the same output, so this is exact.
    stamp=$("$BIN_SRC" --version 2>/dev/null) \
        || fail "the fetched kaed-$SUFFIX will not run --version here — wrong arch, or not a kaed binary"
    reported=$(printf '%s\n' "$stamp" | awk '{ gsub(/[()]/, "", $3); print $2 "-" $3 }')
    [ "$reported" = "$STORE_VERSION" ] || fail \
        "the fetched binary reports $reported but is published as $STORE_VERSION — the store's labelling is wrong, not this host"
    note "reports $stamp"

    ASSET_DIR="$WORK"
fi

for f in kaed.service config.example.toml new-token.sh; do
    [ -f "$ASSET_DIR/$f" ] || fail "missing $ASSET_DIR/$f"
done

# --- 2. Binary ------------------------------------------------------------

if [ -n "$BIN_SRC" ]; then
    [ -x "$BIN_SRC" ] || fail "no executable at $BIN_SRC"
elif [ "$NO_BUILD" -eq 1 ]; then
    BIN_SRC="$REPO_DIR/target/release/kaed"
    [ -x "$BIN_SRC" ] || fail "no binary at $BIN_SRC (drop --no-build, or pass --bin)"
else
    command -v cargo >/dev/null 2>&1 \
        || fail "cargo not found; install from the store instead (--from-store), or pass --bin PATH"
    run "cd '$REPO_DIR' && cargo build --release"
    BIN_SRC="$REPO_DIR/target/release/kaed"
    if [ "$DRY_RUN" -eq 0 ] && [ ! -x "$BIN_SRC" ]; then
        fail "cargo build produced no binary at $BIN_SRC"
    fi
fi

run "install -d -m 0755 '$BIN_DST_DIR'"
# Install to a temp name and mv into place: a running kaed holds the inode,
# and replacing the file wholesale avoids ETXTBSY on the upgrade path.
run "install -m 0755 '$BIN_SRC' '$BIN_DST.new'"
# Keep the outgoing binary as the rollback target. A deploy that cannot be
# undone without a rebuild is not a deploy you want to run at 2am, and the
# build it replaced is the one thing guaranteed to have worked here.
# `kaed --version` on the .prev tells you exactly which build it is.
if [ -f "$BIN_DST" ]; then
    run "mv -f '$BIN_DST' '$BIN_DST.prev'"
fi
run "mv -f '$BIN_DST.new' '$BIN_DST'"

# Token minting and rotation are operator work that outlives the install, and
# a store-installed host has no checkout to run them from. Ships as a command
# rather than a file you have to go and find.
run "install -m 0755 '$ASSET_DIR/new-token.sh' '$TOKEN_TOOL_DST'"

case ":$PATH:" in
    *":$BIN_DST_DIR:"*) ;;
    *) note "NOTE: $BIN_DST_DIR is not on your PATH." ;;
esac

# --- 3. Unit file ---------------------------------------------------------

run "install -d -m 0755 '$UNIT_DIR'"
run "install -m 0644 '$ASSET_DIR/kaed.service' '$UNIT_DST'"

# --- 4. Config — created only if absent, never overwritten ---------------

run "install -d -m 0700 '$CONFIG_DIR'"

CONFIG_IS_NEW=0
if [ -f "$CONFIG_DST" ]; then
    note "config exists, left untouched: $CONFIG_DST"
else
    CONFIG_IS_NEW=1
    run "install -m 0600 '$ASSET_DIR/config.example.toml' '$CONFIG_DST'"
    note "wrote a STARTER config: $CONFIG_DST"
    note "its roots are placeholders — kaed will refuse to start until you edit them"
fi

# --- 5. Enable ------------------------------------------------------------

run "systemctl --user daemon-reload"
run "loginctl enable-linger '$(id -un)'"

# Only start a host that is actually configured. A brand-new install has
# placeholder roots, so starting it would just log a crash loop.
if [ "$CONFIG_IS_NEW" -eq 1 ]; then
    run "systemctl --user enable kaed"
    note "NOT started: finish the config first (see next steps below)"
elif systemctl --user is-active --quiet kaed 2>/dev/null; then
    # Upgrade path: the binary changed under a running daemon.
    run "systemctl --user restart kaed"
else
    run "systemctl --user enable --now kaed"
fi

# --- 6. What is left for a human -----------------------------------------

printf '\ndone.\n'
if [ "$CONFIG_IS_NEW" -eq 1 ]; then
    cat <<EOF

Next, in order:

  1. Edit $CONFIG_DST
     — roots: name the directories an agent should reach. NOT \$HOME.
     — allowed_hosts: if a proxy fronts kaed, its hostname in BOTH the
       bare and :4870 forms, or requests die before auth runs.
  2. $TOKEN_TOOL_DST
  3. kaed check-config          # read the roots and deny rules it prints
  4. systemctl --user start kaed
EOF
else
    printf '\n  kaed check-config     # confirm what this host can reach\n'
    printf '  systemctl --user status kaed\n'
    if [ -f "$BIN_DST.prev" ]; then
        printf '\n  rollback: mv -f %s.prev %s && systemctl --user restart kaed\n' \
            "$BIN_DST" "$BIN_DST"
    fi
    if [ "$FROM_STORE" -eq 1 ]; then
        printf '            (or re-run with --version <older>, from the store)\n'
    fi
fi
