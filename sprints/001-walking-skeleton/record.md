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

## Shipped

_(filled in as work lands)_

## Follow-ups

_(collected as they appear; promoted to korg items at ship time)_
