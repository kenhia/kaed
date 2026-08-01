# List available recipes
default:
    @just --list

# Run CI gates (fmt, clippy incl. test targets, tests)
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test
