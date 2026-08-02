# Sprint 001 — implementation plan

Contract: [mcp-contract.md](../planning/mcp-contract.md) · Architecture:
[architecture.md](../planning/architecture.md). This plan is the 001 slice
of both; where they conflict, the contract wins.

## Scope fence

**In:** `roots`, `stat`, `list`, `read` (whole / `range` / `window` /
`numbered`), `search`, `edit` with `anchor_replace` + `range_replace` +
`create`; R1–R4 semantics; atomic multi-file apply; journal *writes*
(schema + txn recording, torn-txn detection at startup); bearer auth;
streamable HTTP; systemd/tailscale deploy to kai; Desktop Claude wiring.

**Out (Next or later):** `outline`, `node_replace`, `check`, `journal` /
`diff` / `revert` tools, `feedback`, `insert` / `delete` / `rename` ops,
blob retention GC, fleet deploy, k-homelab recipe.

## Milestones

Bottom-up so every layer is tested before the one above leans on it.
Each milestone ends green under `just check` and gets a commit.

1. **Scaffold** — dependencies pinned; `errors.rs` (R4 model: every code
   in the contract, `data` payloads typed); `config.rs` + `kaed
   check-config` (TOML: roots, auth token_env indirection, limits with
   defaults, port 4870 convention).
2. **fsops** — path jailing (join → normalize → canonicalize-inside-root,
   symlink escapes rejected); versioned read (blake3, 16 hex chars);
   `stat`; `list` (gitignore-aware via `ignore` crate, budgeted +
   explicit truncation). Unit tests incl. jail escapes and binary
   detection.
3. **Edit engine** — `addr.rs` (anchor resolution with occurrence
   disambiguation; range math) + `txn.rs`. Pure core: `(base contents,
   ops) → (new contents | structured error)`, goldens for every op and
   every error path first. Then the transactional shell: per-file write
   locks, re-hash + compare against `base`, stage temps → fsync → rename
   all → fsync dirs, rollback = don't rename. `dry_run`, `return_diff`
   (via `similar`), `version_conflict` carries `{expected, actual,
   delta}`.
4. **Journal** — SQLite (`rusqlite`), one DB per host at
   `~/.local/share/kaed/journal.db`; tables `txns`, `txn_files`, `blobs`
   (content-addressed; retention GC deferred), `feedback` (schema only).
   Txn row written before renames begin, marked complete after; startup
   scan warns on incomplete.
5. **search** — `grep-searcher`/`grep-regex` over jailed roots, gitignore
   aware; each match carries the file's `version`; capped + explicit
   truncation.
6. **Server** — `rmcp` streamable HTTP on loopback; tower auth layer
   (bearer → author identity, no anonymous mutation); tool registration
   with `schemars`-derived schemas; `tracing`. Integration test: real
   HTTP client runs the contract's worked example — search → read →
   edit → conflict path.
7. **Deploy + dogfood** — build for kai, systemd user service, `tailscale
   serve`, tokens via systemd credentials/env; `claude mcp add` on cleo;
   scratch-repo dogfood. `record-machine-change` for anything outside
   k-homelab recipes. Deploy notes land in this directory.

## Dependencies (pin at current stable when added)

`rmcp` (server + streamable-http), `tokio`, `axum`/`tower` (auth layer),
`blake3`, `similar`, `grep-searcher`/`grep-regex`/`ignore`, `rusqlite`
(bundled), `serde`/`serde_json`/`schemars`, `clap` (derive), `tracing` +
`tracing-subscriber`, `toml`. Dev: `proptest` (targeted), `tempfile`,
plus an MCP HTTP client for the integration test (rmcp client feature).

## Module map (001 slice of the architecture sketch)

```
src/
  main.rs      — clap: serve, check-config
  config.rs    — TOML config: roots, tokens, limits
  errors.rs    — R4 error model (all codes, typed data)
  fsops.rs     — jailing, versioned reads, stat, list, atomic write
  addr.rs      — anchors, ranges
  txn.rs       — edit engine core + transactional apply
  search.rs    — grep-based search
  journal.rs   — SQLite schema + txn recording
  server.rs    — rmcp wiring, tools, auth
```

## Testing strategy (balanced)

- Golden tests: edit-engine core, exhaustive over ops × error codes.
- Property tests: anchor uniqueness/occurrence and range arithmetic only.
- Integration: one end-to-end suite over real HTTP incl. the conflict
  path and a concurrent two-writers-one-loser test.
- Crash safety: simulated torn txn (journal row present, rename absent)
  → startup detection warns. kill -9 stress deferred to dogfooding.
