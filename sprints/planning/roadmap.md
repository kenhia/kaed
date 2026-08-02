# Roadmap

> The general plan for this project. Keep it current; detail lives in the
> sprint records. Design docs: [summary](summary.md) · [overview](overview.md)
> · [mcp-contract](mcp-contract.md) · [architecture](architecture.md).

## Now

- **Fleet deploy** is the next natural step now that 002 shrank the blast
  radius: kubs0 + kubsdb, k-homelab recipe for install/config/token
  layout, korg project registration.

## Next

- **History tools:** `journal`, `diff`, `revert` — including a read path
  for the `txn_failures` 002 started writing.
- **Structure:** tree-sitter `outline` + `node_replace` + `check` parse
  diagnostics (rust, markdown, toml first).
- **`feedback` tool** — then act on it: first contract revision driven by
  real agent friction reports.
- **Remaining edit ops:** `insert`, `delete`, `rename`; `window`-mode and
  `numbered` reads if not already in 001.
- **Auth-layer metrics:** per-identity 401 and grace-token counters. 002
  logs both at `warn`; the journal will never see them (401s are rejected
  before the transaction layer). Pairs with the conflict rate from #910 as
  the first thing kmon scrapes off this service.
- **Dogfood report:** a written comparison (round-trips, failures, token
  cost) of kaed vs rclone-mount vs base64-over-ssh on real editing tasks
  — the evidence for whether the bet is paying off.

## Later / Ideas

- `apply_patch` (unified-diff input) if dogfooding misses it.
- Check hooks beyond parse: fmt/lint/`just check` integration in the edit
  response.
- LSP bridge: rename-symbol, find-references, live diagnostics.
- Leases/locks — only if optimistic versioning shows real contention.
- ksandbox deployment.
- Local (cleo) kaed as an opt-in power tool alongside built-in editing
  tools — the "if we got it right" bet; needs the dogfood report first.
- Per-region versioning for conflict-free disjoint edits.
- MCP resources/subscriptions; metrics & observability; binary/non-UTF-8
  handling; journal → korg feedback promotion automation.

## Done

- **Sprint 002 — blast-radius hardening** (2026-08-02). All six
  sprint-001 follow-ups. Three-layer path deny list enforced in the
  resolver *and* both directory walkers (#908, new `denied` code +
  contract R7); RFC 6750 attributes on 401s (#913); contract R1 now states
  versions are durable content addresses (#915); failed transactions
  journaled, conflict-rate-per-author is one query (#910); blob retention
  made real — GC'd, 7 days, metadata kept forever, DB 0600 (#909);
  SIGHUP token reload with a rotation grace window (#914). kai re-deployed
  on narrowed roots and every exit criterion re-verified live. Record:
  `sprints/002-blast-radius-hardening/`; decisions for #909/#914 in its
  `decisions.md`; contract delta in its `contract-notes.md`.
- **Sprint 001 — walking skeleton** (2026-08-02). The six core tools,
  R1–R4 semantics, atomic apply, journal writes, bearer auth over
  streamable HTTP; deployed to kai (systemd user unit + tailscale
  serve) and verified end-to-end from Desktop Claude on cleo. Record:
  `sprints/001-walking-skeleton/`; contract clarifications in its
  `contract-notes.md`; follow-up WIs #908–#911, #913, #914.
