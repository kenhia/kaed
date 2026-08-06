# List available recipes
default:
    @just --list

# Run CI gates (fmt, clippy incl. test targets, tests)
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Build a release and publish the deploy bundle to the homelab package
# store (k-homelab docs/deploying.md). Hosts install by fetching
# artifacts/kaed/<version>/ — no clone and no cargo needed on them.
#
# The bundle is the binary PLUS everything install.sh needs, so the unit
# file and config template that shipped with a build stay recoverable with
# that build. The version is read out of the binary just built rather than
# recomputed from git: build.rs already stamps it with `git describe`, and
# a second, separate derivation is one that can drift. deploy/install.sh
# re-derives it from `--version` the same way and refuses a mismatch.
publish:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "$(git status --porcelain)" ]]; then
        echo "publish: refusing to publish from a dirty tree — a published version must name a commit" >&2
        exit 1
    fi
    cargo build --release
    stamp="$(./target/release/kaed --version)"
    v="$(printf '%s\n' "$stamp" | awk '{ gsub(/[()]/, "", $3); print $2 "-" $3 }')"
    case "$v" in
        *dirty*|*unknown*)
            echo "publish: binary stamped '$stamp' — that names no reproducible commit" >&2
            exit 1 ;;
    esac
    # A branch commit vanishes from history at squash-merge, so a branch
    # build may exist in the store (to prove a path) but must never become
    # what `latest` points the fleet at.
    latest_arg=""
    if [[ "$(git rev-parse --abbrev-ref HEAD)" != "main" ]]; then
        latest_arg="--no-latest"
        echo "publish: not on main — publishing $v WITHOUT moving the latest pointer" >&2
    fi
    arch="$(uname -m)-$(uname -s | tr '[:upper:]' '[:lower:]')"
    echo "==> publishing kaed $v as $stamp"
    d=$(ssh -n kubsdb mktemp -d)
    scp target/release/kaed kubsdb:"$d/kaed-$arch"
    scp deploy/install.sh deploy/kaed.service deploy/config.example.toml \
        deploy/new-token.sh kubsdb:"$d"/
    ssh -n kubsdb "kpkg artifact $latest_arg kaed $v $d/* && rm -rf $d"
