# kaed architecture sketch

Implementation notes for the contract in [mcp-contract.md](mcp-contract.md).
Sketch-level: enough to start sprint 001 without re-deciding everything,
loose enough to bend once code exists.

## Shape

One Rust binary, `kaed`, with subcommands:

- `kaed serve` — the daemon: MCP over streamable HTTP on a loopback port.
- `kaed check-config` — validate config, print resolved roots.
- (later) `kaed journal …` — local CLI peek at the journal for humans.

Single crate to start, modules cut so a workspace split stays cheap if a
client lib or shared-types crate is ever wanted:

```
src/
  main.rs        — clap subcommands
  config.rs      — TOML config: roots, tokens, limits
  server.rs      — MCP wiring (rmcp), tool registration, auth middleware
  fsops.rs       — versioned reads, atomic write/rename, path jailing
  txn.rs         — edit engine: op application, staging, rollback
  addr.rs        — addressing: anchor resolution, ranges, node selectors
  outline.rs     — tree-sitter: parsers, symbol outlines, node_id scheme
  search.rs      — grep-crate based search over roots
  journal.rs     — SQLite journal: entries, content retention, diff/revert
  errors.rs      — the R4 error model
```

## Key dependencies

| Crate | Role |
|---|---|
| `rmcp` (official Rust MCP SDK) | MCP server + streamable HTTP transport |
| `tokio`, `axum` | async runtime; HTTP (rmcp rides on axum) |
| `blake3` | content versions (16-hex-char prefix) |
| `similar` | unified diffs (edit proof, conflict deltas, `diff` tool) |
| `grep-searcher` / `grep-regex` / `ignore` | ripgrep-engine search + gitignore-aware walking |
| `tree-sitter` + grammars | outlines, node addressing, `check` parse pass. Start: rust, python, markdown, toml, yaml, json, bash, javascript/typescript |
| `rusqlite` | journal store |
| `serde` / `schemars` | tool I/O types + JSON schemas from one definition |
| `clap`, `tracing` | CLI, logs |

Version pins decided at sprint 001; prefer current stable of each.

**Protocol revision: kaed serves MCP `2026-07-28`** and negotiates down to
`2024-11-05`. That is a deliberate, declared list (`SUPPORTED_PROTOCOL_VERSIONS`
in `server.rs`), not rmcp's default — rmcp advertises every revision the *SDK*
knows, which is a claim about the SDK rather than about kaed. Sprint 015 has the
story of what that default cost; sprint 016 moved the ceiling by emitting what
`2026-07-28` requires. **The rule survives the move: the ceiling goes up when
the response shapes do, and never as a side effect of a dependency bump** — a
client asking for something newer still gets the ceiling, not an echo.

Two constants, deliberately different:

| | asks/answers | pinned to |
|---|---|---|
| `server::PROTOCOL_VERSION` | what kaed serves | `2026-07-28` |
| `fleet::PEER_PROTOCOL_VERSION` | what the gateway asks a peer for | `2025-11-25` |

The gateway is lower because rmcp 3.1.0's *client* cannot drive a `2026-07-28`
session — it omits the per-request `_meta` and SEP-2243 headers that revision
requires, and its own server rejects it. Both constants are stated rather than
defaulted, for the same reason in both directions.

## Core mechanics

**Versions & truth.** Disk is the source of truth — no long-lived in-memory
buffers. `version = blake3(file_bytes)[..16]`. Every read hashes what it
serves; every mutation re-hashes at apply time under a per-file write lock,
compares against `base`, and aborts on mismatch. Cheap at homelab file
sizes; revisit (mtime+size fast path) only if profiling says so.

**Atomic transactions.** Apply ops in memory against the base content, then:
write temp files alongside targets → fsync → rename all → fsync dirs.
Rollback = don't rename. Renames aren't atomic *across* files, so the
journal records the transaction before renames begin and marks it complete
after — an interrupted transaction is detectable and repairable on startup.
Permissions/xattrs copied from the original; executable bit settable on
create.

**Path jailing.** Roots come from config. Every path is joined, normalized,
and checked to canonicalize inside its root — symlink escapes rejected
(`outside_root`). No tool accepts absolute paths.

**Journal.** SQLite, one DB per host (`~/.local/share/kaed/journal.db`),
tables: `txns`, `txn_files` (path, old/new version), `blobs`
(content-addressed, for `diff`/`revert` reconstruction; retention window
configurable, default ~30 days), `feedback`. Author from bearer token;
`git_head` captured via `git -C` at txn time when inside a repo.

**Node IDs.** `outline` returns selector-style ids (`fn:apply/body`,
`impl:Journal/fn:record`) that are **resolved fresh against the declared
base version** at edit time — ids are selectors, not pointers into a kept
tree, preserving statelessness (R6).

## Config

`~/.config/kaed/config.toml`:

```toml
[server]
bind = "127.0.0.1:4870"        # convention: same port on every host

[[roots]]
name = "home"
path = "/home/ken"
description = "everything under ~"

[auth]
# token -> author identity; values come from the env, not this file
claude = { token_env = "KAED_TOKEN_CLAUDE" }
ghcp   = { token_env = "KAED_TOKEN_GHCP" }

[limits]
max_read_bytes = 262144
max_file_bytes = 8388608
search_max_results = 50

[journal]
retention_days = 30
```

## Deployment

- systemd **user** service per host (kai, kubs0, kubsdb; ksandbox later),
  mirroring how kmon/korg services run.
- Daemon binds loopback only; `tailscale serve` fronts it with tailnet
  HTTPS, one URL per host — the same pattern the rest of the homelab uses
  (see tailscale-serve rollout decisions in klams). LAN clients on the
  serving host use localhost.
- Install/config lands as a k-homelab recipe once past the spike stage;
  until then, `record-machine-change` discipline applies.
- Client side: `claude mcp add --transport http kaed-kai <url>` per host,
  same shape as klams/korg wiring.

## Security posture

- Mutation surface is file edits only — no exec, by contract (see
  overview non-goals). Blast radius = writable files under configured
  roots.
- Bearer tokens per agent identity; no anonymous writes; author on every
  journal row. Tokens live in env/systemd credentials, never in config or
  repo.
- Tailnet-only exposure; loopback bind means no LAN listener at all.
- Root allowlist + canonicalization jail (above).

## Testing

- **Edit engine:** golden tests — (base content, ops) → (result, diff),
  including every error path; property tests (proptest) for
  anchor-resolution and range math against random edits.
- **Concurrency:** loom-style or stress tests for the version-check-
  under-write-lock path; two writers, one loser, loser gets a coherent
  `version_conflict`.
- **Protocol:** integration tests driving the real server over HTTP with
  an rmcp client — the same calls the worked example in the contract
  shows.
- **Crash safety:** kill -9 during apply; journal-led repair on restart
  leaves no torn state.
- CI gate is `just check` = `cargo fmt --check` + `clippy --all-targets
  -D warnings` + `cargo test` (run locally before shipping, per house
  rule).
