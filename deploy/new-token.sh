#!/bin/sh
# Mint or rotate a kaed bearer token. Separate from install.sh on purpose:
# an install must never silently rotate a live credential (korg #923).
#
# THIS SCRIPT NEVER PRINTS THE TOKEN. Sprint 001's token had to be rotated
# because one landed in a transcript. Read it with `cat` yourself when you
# are ready to paste it into a client, in a place you are happy to have it.
#
# install.sh puts this on PATH as `kaed-new-token`, because rotation is
# ongoing operator work and a host installed from the package store has no
# checkout to run it from.
#
# Usage:
#   kaed-new-token              mint the first token; REFUSES if one exists
#   kaed-new-token --rotate     rotate, opening a grace window
#   kaed-new-token --close      close the grace window
#
#   --identity NAME   act on that [auth] identity instead of the default
#                     one, reading its token_file and prev_token_file out
#                     of config.toml. A host with per-machine authors has
#                     several credentials and only one is `token`.
#   --force           rotate even with no grace window configured, i.e.
#                     deliberately cut every live session off.
#
# Rotation is a three-step dance because clients load MCP config only at
# session start. --rotate keeps the OLD token working alongside the new one
# so clients can pick the new value up whenever they next restart, instead
# of all at once. --close ends that window.
#
# That window only exists if config.toml declares prev_token_file for the
# identity. Sprint 019: eight of the fleet's nine credentials had no such
# line, because this script only ever mentioned it *after* rotating. It now
# refuses instead — a rotation that cannot be rolled back is --force.

set -eu

fail() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

MODE=mint
IDENTITY=
FORCE=no

while [ $# -gt 0 ]; do
    case "$1" in
        --rotate) MODE=rotate ;;
        --close) MODE=close ;;
        --force) FORCE=yes ;;
        --identity) shift; [ $# -gt 0 ] || fail "--identity needs a name"; IDENTITY=$1 ;;
        --identity=*) IDENTITY=${1#--identity=} ;;
        # Print the header block, so the help cannot drift from the file.
        -h|--help) awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "$0"; exit 0 ;;
        *) echo "usage: $0 [--rotate|--close] [--identity NAME] [--force]" >&2; exit 2 ;;
    esac
    shift
done

CONFIG_DIR="${KAED_CONFIG_DIR:-$HOME/.config/kaed}"
CONFIG="$CONFIG_DIR/config.toml"

expand_home() {
    case "$1" in
        "~/"*) printf '%s\n' "$HOME/${1#\~/}" ;;
        *) printf '%s\n' "$1" ;;
    esac
}

# The `[auth]` line for an identity, or nothing. Deliberately literal about
# the shape config.example.toml ships — a one-line inline table. Anything
# else (an `[auth.name]` subtable, a multi-line entry) reads as absent, and
# the caller turns that into a refusal rather than a guess: this script
# mints credentials, so guessing wrong writes a secret to the wrong path.
auth_line() {
    [ -f "$CONFIG" ] || return 0
    awk -v id="$1" '
        /^[ \t]*\[/ { in_auth = ($0 ~ /^[ \t]*\[auth\][ \t]*$/); next }
        !in_auth || /^[ \t]*#/ { next }
        {
            n = index($0, "=")
            if (n == 0) next
            key = substr($0, 1, n - 1)
            gsub(/[ \t"]/, "", key)
            if (key == id) { print; exit }
        }
    ' "$CONFIG"
}

# `token_file` / `prev_token_file` out of such a line. Anchoring on the `{`
# or `,` that must precede a key is what keeps `token_file` from matching
# inside `prev_token_file`.
field() {
    printf '%s\n' "$1" | sed -n "s/.*[{,][ 	]*$2[ 	]*=[ 	]*\"\([^\"]*\)\".*/\1/p"
}

# Which identity owns a given token path — how the no-`--identity` default
# finds out whether it has a grace window, without the caller having to know
# that the legacy `claude` entry is the one pointing at plain `token`.
identity_owning() {
    [ -f "$CONFIG" ] || return 0
    awk '
        /^[ \t]*\[/ { in_auth = ($0 ~ /^[ \t]*\[auth\][ \t]*$/); next }
        !in_auth || /^[ \t]*#/ { next }
        index($0, "=") { print }
    ' "$CONFIG" | while IFS= read -r line; do
        f=$(field "$line" token_file)
        [ -n "$f" ] || continue
        [ "$(expand_home "$f")" = "$1" ] || continue
        printf '%s\n' "$line"
        break
    done
}

# GRACE is yes / no / unknown, and UNKNOWN_WHY says which flavour of
# unknown: there may be no config at all (minting before writing config.toml
# is the normal first-install order), or a config that simply names no entry
# for this path. Neither can be a refusal, but they are different facts and
# the warning has to say which one it actually saw.
GRACE=unknown
UNKNOWN_WHY=
if [ -n "$IDENTITY" ]; then
    [ -f "$CONFIG" ] ||
        fail "no $CONFIG — --identity reads that identity's token paths from it"
    LINE=$(auth_line "$IDENTITY")
    [ -n "$LINE" ] || fail "$CONFIG declares no [auth] entry \"$IDENTITY\" (it must be a
       one-line inline table under a plain [auth] header). Minting to a path
       nothing references would create a credential that authenticates
       nothing and looks exactly like one that does."
    TOKEN=$(field "$LINE" token_file)
    [ -n "$TOKEN" ] || fail "[auth] $IDENTITY has no token_file. An identity on token_env cannot be
       rotated from a file at all — the value is frozen into the process at
       exec, so changing it needs a restart, not this script."
    TOKEN=$(expand_home "$TOKEN")
    PREV=$(field "$LINE" prev_token_file)
    if [ -n "$PREV" ]; then
        PREV=$(expand_home "$PREV"); GRACE=yes
    else
        PREV="$TOKEN.prev"; GRACE=no
    fi
else
    TOKEN="$CONFIG_DIR/token"
    PREV="$CONFIG_DIR/token.prev"
    LINE=$(identity_owning "$TOKEN")
    if [ -n "$LINE" ]; then
        p=$(field "$LINE" prev_token_file)
        if [ -n "$p" ]; then PREV=$(expand_home "$p"); GRACE=yes; else GRACE=no; fi
    elif [ -f "$CONFIG" ]; then
        UNKNOWN_WHY="$CONFIG has no [auth] entry whose token_file is $TOKEN"
    else
        UNKNOWN_WHY="there is no $CONFIG to consult"
    fi
fi

# 48 chars of base64 with the awkward characters removed — it rides in an
# HTTP header, so keep it to things no shell or URL will reinterpret.
mint() {
    umask 077
    head -c 48 /dev/urandom | base64 | tr -d '/+=\n' | head -c 48 > "$1"
    chmod 600 "$1"
    [ -s "$1" ] || fail "wrote an empty token to $1 — refusing to continue"
}

reload_if_running() {
    if systemctl --user is-active --quiet kaed 2>/dev/null; then
        systemctl --user reload kaed
        printf '  reloaded kaed (SIGHUP; no restart, live sessions survive)\n'
    else
        printf '  kaed is not running; it will read the new token at startup\n'
    fi
}

mkdir -p "$CONFIG_DIR"
chmod 700 "$CONFIG_DIR"

case "$MODE" in
    mint)
        if [ -e "$TOKEN" ]; then
            fail "$TOKEN already exists. Refusing to overwrite a live credential — use --rotate."
        fi
        mint "$TOKEN"
        printf 'Minted %s (mode 0600).\n' "$TOKEN"
        printf '  Not printed here, by design. Read it when you are ready to\n'
        printf '  paste it into a client, and keep it out of any configured root.\n'
        if [ "$GRACE" = no ]; then
            printf '\n  NOTE: this identity has no prev_token_file, so its first\n'
            printf '  rotation will refuse. Add one now, while nothing depends on it:\n'
            printf '      prev_token_file = "%s.prev"\n' "$TOKEN"
            printf '  Config *shape* is a restart, not a reload.\n'
        fi
        ;;

    rotate)
        [ -e "$TOKEN" ] || fail "no $TOKEN to rotate — run without --rotate to mint one"
        if [ "$GRACE" = no ] && [ "$FORCE" != yes ]; then
            fail "no prev_token_file configured for this token, so rotating it is a HARD
       CUT: the old value stops authenticating the moment kaed reloads and
       every live session on it 401s until it restarts. Add

           prev_token_file = \"$PREV\"

       to its [auth] entry in $CONFIG and restart kaed (config shape is a
       restart, not a reload), then rotate. Or pass --force to cut now."
        fi
        if [ "$GRACE" = no ]; then
            mint "$TOKEN"
            printf 'Rotated with NO grace window (--force). The old token is dead.\n'
            printf '  Every client still holding it will 401 until it is updated.\n'
            reload_if_running
            exit 0
        fi
        if [ "$GRACE" = unknown ]; then
            printf 'WARNING: %s,\n' "$UNKNOWN_WHY"
            printf '  so this rotation assumes a grace window at\n'
            printf '  %s. If the entry that ends up naming this token\n' "$PREV"
            printf '  omits prev_token_file, the old value stops working at reload.\n\n'
        fi
        cp "$TOKEN" "$PREV"
        chmod 600 "$PREV"
        mint "$TOKEN"
        printf 'Rotated. Grace window OPEN: the old token still works.\n'
        reload_if_running
        cat <<EOF

  Update your clients, then close the window with --close. While it is
  open, every request still on the old token logs:

      WARN kaed::server: authenticated with the PREVIOUS token; this client
      has not picked up the new one yet

  That line going quiet is how you know it is safe to close.

  If this credential is consumed by a gateway (kai proxies to its peers as
  the caller), the new value has to reach that host's
  [peers.<this-host>.tokens] entry too. The window is what makes that a
  copy you can take your time over instead of a race.
EOF
        ;;

    close)
        [ -e "$PREV" ] || fail "no $PREV — no grace window is open"
        rm -f "$PREV"
        printf 'Grace window CLOSED: the old token no longer authenticates.\n'
        reload_if_running
        ;;
esac
