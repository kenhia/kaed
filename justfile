# List available recipes
default:
    @just --list

# Run CI gates (fmt, clippy incl. test targets, tests)
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Build a release and publish the binary to the homelab package store
# (k-homelab docs/deploying.md). Hosts deploy by fetching
# artifacts/kaed/<version>/ from the store — no clone needed on them.
publish:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release
    v="$(cargo pkgid | sed 's/.*[@#]//')-$(git rev-parse --short HEAD)"
    d=$(ssh -n kubsdb mktemp -d)
    scp target/release/kaed kubsdb:"$d"/kaed-x86_64-linux
    ssh -n kubsdb "kpkg artifact kaed $v $d/kaed-x86_64-linux && rm -rf $d"
