#!/bin/sh
# Mint or rotate kaed's bearer token. Separate from install.sh on purpose:
# an install must never silently rotate a live credential (korg #923).
#
# THIS SCRIPT NEVER PRINTS THE TOKEN. Sprint 001's token had to be rotated
# because one landed in a transcript. Read it with `cat` yourself when you
# are ready to paste it into a client, in a place you are happy to have it.
#
# Usage:
#   deploy/new-token.sh              mint the first token; REFUSES if one exists
#   deploy/new-token.sh --rotate     rotate, opening a grace window
#   deploy/new-token.sh --close      close the grace window
#
# Rotation is a three-step dance because clients load MCP config only at
# session start. --rotate keeps the OLD token working alongside the new one
# so clients can pick the new value up whenever they next restart, instead
# of all at once. --close ends that window.

set -eu

MODE=mint
case "${1:-}" in
    "") ;;
    --rotate) MODE=rotate ;;
    --close) MODE=close ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "usage: $0 [--rotate|--close]" >&2; exit 2 ;;
esac

CONFIG_DIR="${KAED_CONFIG_DIR:-$HOME/.config/kaed}"
TOKEN="$CONFIG_DIR/token"
PREV="$CONFIG_DIR/token.prev"

fail() { printf 'ERROR: %s\n' "$1" >&2; exit 1; }

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
        ;;

    rotate)
        [ -e "$TOKEN" ] || fail "no $TOKEN to rotate — run without --rotate to mint one"
        cp "$TOKEN" "$PREV"
        chmod 600 "$PREV"
        mint "$TOKEN"
        printf 'Rotated. Grace window OPEN: the old token still works.\n'
        reload_if_running
        cat <<EOF

  Make sure config.toml has prev_token_file set for this identity, or the
  old token stops working immediately and the window buys you nothing:

      claude = { token_file = "~/.config/kaed/token", prev_token_file = "~/.config/kaed/token.prev" }

  Update your clients, then close the window with --close. While it is
  open, every request still on the old token logs:

      WARN kaed::server: authenticated with the PREVIOUS token; this client
      has not picked up the new one yet

  That line going quiet is how you know it is safe to close.
EOF
        ;;

    close)
        [ -e "$PREV" ] || fail "no $PREV — no grace window is open"
        rm -f "$PREV"
        printf 'Grace window CLOSED: the old token no longer authenticates.\n'
        reload_if_running
        ;;
esac
