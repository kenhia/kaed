# Sprint 010 — gateway peer mode

*korg:1059 (proposal) covering #1050. Branch `010-gateway-peer-mode`.
Started 2026-08-07.*

## Goal

Turn on peer routing. The `[peers]` block 007 introduced gains real URLs
and per-agent backend tokens; any kaed instance can proxy calls for a
peer's roots, and whichever instance a client points at becomes the
gateway. No new binary, no new tools, no second contract doc.

The three wins, in the proposal's order of actual value:

1. **Unreachable-host-as-data** — `roots` reports a declared peer that is
   not answering as `{status: "unreachable", since: …}`, a fact to reason
   about, instead of the client's MCP wiring failing at session start.
2. **Fleet-wide search in one call** — `search` accepts a root *pattern*
   (`*:*`, `*:src`, `kai:*`) and fans out in parallel with one budget and
   per-root truncation reporting.
3. **The transport path for #1052** — the cross-session secret handoff
   (next sprint) moves values host-to-host through this proxy, never
   through the agent.

Decided up front by PD-4: the gateway holds a **per-agent token for each
backend** and proxies with the caller's real identity. Journal attribution
survives the hop; nothing about backend auth changes. The "one auth
surface" win is deliberately given up.

## Shape

- `config.toml`: `[peers.<host>]` keeps `status`/`ref`/`note`/`since`,
  uses the `url` field 007 declared, and gains `[peers.<host>.tokens]` —
  author → `{token_env|token_file}`, the caller identities this instance
  can propagate to that backend. Token files re-read on SIGHUP (#914
  extended to peer credentials, D-2).
- `src/fleet.rs` (new): pooled MCP client sessions per (peer, author),
  peer health (down-since, last-seen roots and version), the verbatim
  proxy, and the fleet-search fan-out/merge.
- `server.rs`: a hand-written `call_tool` that routes on the raw `root`
  argument before any schema is applied (D-1); `roots` aggregation with
  live probes (D-4); `search` root patterns (D-5).
- Contract: R4 gains the live `unsupported_capability` code; `roots` and
  `search` sections updated; new error reasons `no_peer_credential`,
  `peer_credential_rejected`, `peer_timeout`.

## What shipped

All of it in one slice — `just check` green (199 unit + 6 gateway + 11
http + 9 installer tests):

- Config: `[peers.<host>.tokens]` parsed, validated (exactly one source
  per entry; warnings for tokens on a deferred peer, tokens without a
  `url`, and authors absent from `[auth]`), resolved at startup and
  re-read on SIGHUP through the same `AuthState::reload` as inbound
  tokens. `check-config` prints per-peer routability and which authors
  resolved.
- `src/fleet.rs`: session pool keyed (peer, author) — rebuilt when the
  configured token rotates, retried once on transport close — health
  tracking (`down_since`, cached roots + version from the last good
  probe), the verbatim proxy with error classification per D-3/D-8, and
  the parallel `roots` probe.
- `server.rs`: raw-argument routing in a hand-written `call_tool` (D-1);
  `roots` aggregates live probes and serves cached peer roots marked
  `unreachable` when a probe fails (D-4); `search` expands root patterns
  and fans out with one budget and per-root reporting (D-5); pattern
  roots on any other tool are `invalid_input` naming `search`.
- Errors: `unsupported_capability` is live; proxied errors pass through
  verbatim plus a top-level `root` tag; new `denied` reasons
  `no_peer_credential` / `peer_credential_rejected`, `not_found` reason
  `host_unreachable` now observed rather than declared, and `internal`
  reason `peer_timeout` that says the outcome is unknown.
- Tests: `tests/gateway.rs` drives two real instances over HTTP — journal
  attribution through the hop (the PD-4 gate), verbatim conflict-delta
  passthrough, unreachable-as-data in `roots` and on the error path,
  missing/rotated peer credentials, fleet search merge and its
  empty-pattern reason, and `journal` proxied by root filter.
- Contract updated in place (`sprints/planning/mcp-contract.md`);
  reasoning in `contract-notes.md` beside this file.

## Verified from a real client

`live-test.md` beside this file records the client-side round: gateway mode
exercised from cleo over the real tailnet, with the PD-4 attribution gate,
D-3 verbatim passthrough and the conflict/re-anchor loop each checked against
a *direct* connection to kubs0 as a control. All held. The one change it
prompted was on the client, not in kaed — cleo's redundant `kaed-kubs0`
connection was removed (korg #1074).

## Follow-ups

- Deploy: add `url` + `[peers.<host>.tokens]` on kai (and kubs0 if it
  should route back) as a deliberate post-deploy config step —
  `install.sh` never touches an existing config. Recorded in `deploy.md`.
- #1052 builds the secret-handoff transport on this proxy.
- Evidence pass follow-up: should the gateway journal proxy *attempts*
  (not payloads) so unreachability windows are queryable? Deferred, D-7.
