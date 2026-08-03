#!/bin/sh
# Install kaed as a systemd user service. Idempotent; safe to re-run.
#
# kai's first install was hand-typed — `install -m755`, a heredoc'd unit
# file, a `sed` to add ExecReload, `head -c 48 /dev/urandom` for the token.
# Doing that twice more by hand is how three hosts end up subtly different
# from each other (korg #923).
#
# What this owns:      the binary, the unit file, and (only if absent) a
#                      starter config.
# What it never
# touches:             the token, and any config that already exists.
# What it deliberately
# leaves alone:        `tailscale serve` and per-host config content —
#                      those belong to k-homelab's recipes and to you.
#
# Usage:
#   deploy/install.sh [--dry-run] [--no-build] [--bin PATH]
#
#   --dry-run   print what would happen, change nothing
#   --no-build  do not run cargo; use an existing release binary
#   --bin PATH  install this prebuilt binary (implies --no-build)

set -eu

DRY_RUN=0
NO_BUILD=0
BIN_SRC=""

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --no-build) NO_BUILD=1 ;;
        --bin)
            shift
            [ $# -gt 0 ] || { echo "--bin needs a path" >&2; exit 2; }
            BIN_SRC="$1"
            NO_BUILD=1
            ;;
        -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
        *) echo "usage: $0 [--dry-run] [--no-build] [--bin PATH]" >&2; exit 2 ;;
    esac
    shift
done

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd -P)
REPO_DIR=$(cd -- "$SCRIPT_DIR/.." && pwd -P)

BIN_DST_DIR="$HOME/.local/bin"
BIN_DST="$BIN_DST_DIR/kaed"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_DST="$UNIT_DIR/kaed.service"
CONFIG_DIR="$HOME/.config/kaed"
CONFIG_DST="$CONFIG_DIR/config.toml"

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

# --- 0. Pre-flight --------------------------------------------------------

command -v systemctl >/dev/null 2>&1 \
    || fail "systemctl not found; this script installs a systemd user service"

[ -f "$SCRIPT_DIR/kaed.service" ] \
    || fail "missing $SCRIPT_DIR/kaed.service"

# --- 1. Binary ------------------------------------------------------------

if [ -n "$BIN_SRC" ]; then
    [ -x "$BIN_SRC" ] || fail "no executable at $BIN_SRC"
elif [ "$NO_BUILD" -eq 1 ]; then
    BIN_SRC="$REPO_DIR/target/release/kaed"
    [ -x "$BIN_SRC" ] || fail "no binary at $BIN_SRC (drop --no-build, or pass --bin)"
else
    command -v cargo >/dev/null 2>&1 \
        || fail "cargo not found; build elsewhere and pass --bin PATH"
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

case ":$PATH:" in
    *":$BIN_DST_DIR:"*) ;;
    *) note "NOTE: $BIN_DST_DIR is not on your PATH." ;;
esac

# --- 2. Unit file ---------------------------------------------------------

run "install -d -m 0755 '$UNIT_DIR'"
run "install -m 0644 '$SCRIPT_DIR/kaed.service' '$UNIT_DST'"

# --- 3. Config — created only if absent, never overwritten ---------------

run "install -d -m 0700 '$CONFIG_DIR'"

CONFIG_IS_NEW=0
if [ -f "$CONFIG_DST" ]; then
    note "config exists, left untouched: $CONFIG_DST"
else
    CONFIG_IS_NEW=1
    run "install -m 0600 '$SCRIPT_DIR/config.example.toml' '$CONFIG_DST'"
    note "wrote a STARTER config: $CONFIG_DST"
    note "its roots are placeholders — kaed will refuse to start until you edit them"
fi

# --- 4. Enable ------------------------------------------------------------

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

# --- 5. What is left for a human -----------------------------------------

printf '\ndone.\n'
if [ "$CONFIG_IS_NEW" -eq 1 ]; then
    cat <<EOF

Next, in order:

  1. Edit $CONFIG_DST
     — roots: name the directories an agent should reach. NOT \$HOME.
     — allowed_hosts: if a proxy fronts kaed, its hostname in BOTH the
       bare and :4870 forms, or requests die before auth runs.
  2. $SCRIPT_DIR/new-token.sh
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
fi
