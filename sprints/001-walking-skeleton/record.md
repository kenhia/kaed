# Sprint 001 — walking skeleton

**korg:** WI #897, proposal 898 · **branch:** `001-walking-skeleton` ·
**started:** 2026-08-02

## Goal

The smallest kaed that beats base64-over-ssh: `roots`, `stat`, `list`,
`read`, `search`, and `edit` (`anchor_replace` / `range_replace` /
`create`) with full R1/R2 version semantics, atomic multi-file apply,
bearer auth, streamable HTTP. Journal writes recorded (schema in place);
no `journal`/`diff`/`revert` tools yet. Deploy to kai as a systemd user
service behind tailscale serve, wire into Desktop Claude, dogfood on a
scratch repo.

Implementation order and per-module decisions: [plan.md](plan.md).

## Decisions

- **Sprint directory, not single file** — this sprint carries a plan doc
  and (later) deploy notes; one file would sprawl.
- **`read` ships all shaped modes in 001** (`range`, `window` with
  line-or-anchor, `numbered`) — token-shaped reads are the core value
  proposition and cost little once range math exists; deferring them made
  the skeleton weaker for no real savings.
- **Journal in 001 records begin/complete and detects torn transactions
  at startup (warn + refuse nothing); automated repair is deferred** —
  detection is the safety property; repair needs dogfooding data on what
  torn states actually look like.
- **Balanced TDD per Ken's preference**: golden tests for the edit engine
  (every op, every error path) written before/with the engine; property
  tests only where they pay (anchor resolution, range math); integration
  test drives the real HTTP server end-to-end. Fortify when something
  slips through.

## Log

- 2026-08-02 — Sprint started. Planning docs read (summary, contract,
  architecture, overview). Record + plan written; korg proposal 898
  active.
- 2026-08-02 — Milestones 1–6 landed in order, each green under `just
  check`: scaffold/errors/config → addr+fsops → edit engine → journal →
  search → MCP server. 80 unit + 5 integration tests. Contract
  clarifications accumulated in [contract-notes.md](contract-notes.md).
  rmcp 3.1.0 (API read from registry source — training-data versions were
  long stale). Notable: rmcp's default `allowed_hosts` is loopback-only
  (DNS-rebinding guard), so the tailnet hostname must be configured at
  deploy — `server.allowed_hosts` config field exists for exactly this.

- 2026-08-02 — Deployed to kai (we're *on* kai, so local install):
  systemd user unit + `tailscale serve --https=4870`. Live-dogfooded the
  full loop through the ts.net URL. Details + the Host-validation gotcha
  in [deploy.md](deploy.md). k-homelab WI #907 records the machine
  change; klams carries the deployment knowledge.

## Shipped

- The six walking-skeleton tools over streamable HTTP with bearer auth,
  R1–R4 semantics throughout, atomic multi-file apply with mode
  preservation, SQLite journal (txns/files/blobs/feedback schema) with
  torn-txn detection at startup, conflict deltas served from retained
  blobs. `kaed serve` + `kaed check-config`.
- Deployed and verified on kai: https://kai.<tailnet>.ts.net:4870/mcp
  (journal txn 1 = the dogfood edit, attributed and intent-tagged).

## Remaining for sprint close

- Desktop Claude on cleo wired to kaed (needs Ken: cleo is his Windows
  desktop; connector URL + token per deploy.md) and a real dogfood
  session from there — the sprint's actual exit criterion.

## Follow-ups

_(collected as they appear; promoted to korg items at ship time)_
