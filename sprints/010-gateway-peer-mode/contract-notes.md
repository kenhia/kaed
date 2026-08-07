# Sprint 010 — contract delta

The changes are already applied to
[`../planning/mcp-contract.md`](../planning/mcp-contract.md); this records
why. 007 deliberately left the vocabulary ready — host-qualified names,
per-root capabilities, `unsupported_capability` reserved, the `fleet`
block — so this sprint mostly *activates* language that was already there.

---

## R10 — new, and the only structural addition

Peer routing earned a contract-wide rule rather than tool-by-tool notes
because its guarantees are cross-cutting: identity propagation, verbatim
passthrough, reachability-as-data and the named failure domain hold for
*every* proxied call, and a tool-local phrasing would invite a tool-local
exception. The four bullets are lifted almost directly from the proposal's
"four ways to get this wrong" — each one is the inverse of a failure mode
the brainstorm predicted.

`unsupported_capability` moves from "reserved" to live, unchanged in
meaning. The reservation was the whole point: nothing an agent learned
in 007 becomes wrong in 010.

## `roots` — `verified` got sharper, not different

007 said `verified` separates declared from observed and pinned it false
for every peer because nothing probed. 010 probes, so the definition
stays and the values move: an *observed-down* peer is now
`verified: true, status: "unreachable"` — the check happened; that was
its result. The alternative (verified only when the news is good) would
make `verified` mean "answering", which `status` already says.

Two additions with no 007 analogue:

- `probe` — what this call's check did (`ok`/`failed`/`skipped` + detail).
  Without it, a peer that *could not be probed* (no URL, no credential for
  this caller) is indistinguishable from one nobody tried, which is the
  #930 confusion reborn one layer up.
- Cached root entries during an outage, marked `unreachable` with `since`.
  The brainstorm's win #1 is literally this artifact: the namespace as
  data while the host is down.

Probes run under the caller's credential — a deliberate consequence of
PD-4. There is no gateway identity to probe with, so what the fleet looks
like is per-caller: an author with no kubs0 token sees kubs0 as declared,
skipped, and knows why.

## `search` — patterns, and the response shape forks only for them

A concrete `root` returns the exact 007 shape; only a pattern gets the
fleet shape. This keeps the common case's contract untouched (no
migration, no conditional fields for single-root callers) at the cost of
two shapes for one tool — judged worth it because the two calls ask
different questions ("search this root" vs "search the fleet") and the
fleet answer *needs* fields the single answer would carry as noise.

The merge reports at three layers because drops happen at three layers:
the host's own `truncated` (its search hit `max_results`), the merge's
`merge_dropped` (the shared budget ran out), and `hosts_unavailable`
(never searched at all). Collapsing any two rebuilds silent truncation,
the one thing principle 3 forbids.

## `journal` — the filter exception

"`root` is a filter, not an address" survives with one carve-out: a
filter naming a routable peer's root proxies, because D-7 means those
records exist *only* on the peer. The alternative reading — filter the
local store, return empty — would be the exact world-model corruption
`coverage` exists to prevent, asserted confidently about a store that was
never consulted. Unreachable peer → `host_unreachable`, same reasoning.
