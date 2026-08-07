# Sprint 010 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. PD-4 (identity propagation via per-agent
> backend tokens) was taken before this sprint started and is implemented,
> not re-decided, here.

---

## D-1 — Route on the raw argument, before any schema

The gateway decides local-vs-remote in a hand-written `call_tool` that
peeks at `arguments.root` as a string and forwards the whole call —
tool name and raw argument object, byte-for-byte — when the root's host
prefix names a routable peer. The typed parameter structs are never
involved on the proxy path.

This is two of the brainstorm's four failure modes designed out at once.
Deserializing into the gateway's own schema would (a) be the full
parse/revalidate/re-serialize cost the performance argument says to
avoid, and (b) enforce the *gateway's* schema on a payload bound for a
host that may be newer — the capability-intersection mistake at the
schema level. A mid-upgrade peer with a new optional parameter must see
that parameter arrive, not have the gateway strip or reject it.

Corollary: rmcp's `#[tool_handler]` macro only generates `call_tool` when
the impl block lacks one, so the hand-written method slots in without
forking the router plumbing.

## D-2 — Peer credentials live beside the peer, reload with SIGHUP, and are never substituted

`[peers.<host>.tokens]` maps an author name to `{token_env|token_file}`,
exactly one, same rules as `[auth]`. The proxy uses the *caller's* mapped
token; if there is none, the call refuses with `denied` /
`no_peer_credential` naming both hosts, the author, and the two remedies
(add the token, or connect to the peer directly). It never falls back to
another identity's token or a shared gateway credential — a misattributed
journal row is strictly worse than a refused call, per PD-4.

Token files are re-read by the same `AuthState::reload` that SIGHUP
already drives for inbound tokens (#914): one reload covers both
directions. Sessions notice a rotated token at checkout (the pooled
session remembers which token built it) and rebuild. `token_env` entries
keep the documented #914 limitation: a process cannot re-read its own
environment, so env-sourced peer tokens need a restart.

## D-3 — Passthrough is verbatim; the only addition is a root tag on errors

A proxied success is returned exactly as the peer produced it — nothing
added, nothing renamed, so a response via the gateway is
indistinguishable from a direct one. A proxied `isError` result passes
through with its `{code, message, data}` object intact plus one top-level
`"root"` tag, so a fan-out consumer can tell which root produced it.
`version_conflict` deltas and `ambiguous_anchor` candidates therefore
survive the hop by construction — the re-anchor-without-re-reading loop
works identically through the gateway. (Gate test: a conflict delta
produced on the backend, read through the gateway, verbatim.)

A peer that answers "tool not found" (older kaed, mid-upgrade fleet) maps
to the contract's reserved `unsupported_capability` code, now live —
that is the union-of-capabilities rule paying off at call time.

## D-4 — Reachability is observed at call time and served as data, not stored

Peer health is in-memory only: `down_since` (first failure of the current
outage), plus the roots and version from the last successful probe.
`roots` probes every routable peer in parallel with the caller's own
credential; a peer that answers is `verified: true` with its observed
version and roots (their entries passed through verbatim, capabilities
per-root — the union rule); a peer that does not answer is
`unreachable` with the observed `since`, and its *cached* root entries
are served with `status: "unreachable"` so the namespace stays visible —
`{name: "kubs0:src", status: "unreachable", since}` is the exact artifact
win #1 names. A config-declared `unreachable` peer that answers is
reported active-verified (observation beats declaration) with a warning
in the log to fix the config.

Nothing is persisted: a restart forgets health, and the first call
re-learns it. Persisting would create a second source of truth about
other hosts, which is what `[peers]` already is.

## D-5 — Fleet search is a root pattern on `search`, expanded from live probes

`search` accepts `*`-glob patterns over full root names (`*:*`, `*:src`,
`kai:*`). Patterns are matched against roots that are *known to exist*:
this instance's own, plus each routable peer's, enumerated by the same
parallel `roots` probe D-4 uses (fresh per call — correctness over the
one saved round trip; a cache can come later if measured). Each concrete
root then gets its own search — local roots in-process, peer roots as
ordinary proxied calls — in parallel.

One budget, per-root reporting: every fan-out call gets the full
`max_results` (a host cannot know what the merge will keep), the merge
takes matches in stable root order (local first, then peers in declared
order) up to the budget, and every root gets a `fanout[]` entry carrying
its own `files_searched`, `truncated`, `denied_hidden`,
`classified_hidden`, `reason`, and `merge_dropped` — the count the
*merge* discarded, distinct from what the host itself truncated. Hosts
that could not be searched at all appear in `hosts_unavailable[]` with
the same status vocabulary as `roots` (`unreachable`, `no_credential`).
Silent truncation is the one bug the proposal names as corruption; every
drop in this pipeline is attributed to the layer that made it.

Merged matches each carry a `root` tag. A single-root (concrete) search
response is unchanged — same shape as 007, no migration for the common
case. A pattern that matches no known root returns
`reason: root_pattern_matched_no_roots` with the known names, the #1066
rule applied to the fleet namespace. Patterns are refused with
`invalid_input` on every other tool: search is read-only and merge-safe;
a pattern `edit` has no honest atomicity story.

## D-6 — `journal` proxies by root filter; `feedback` stays local

`journal {root: "kubs0:src"}` asked of the gateway proxies to kubs0,
because that is where those records physically live — a proxied edit
journals *only* on the backend, under the real author (PD-4). Without a
`root`, `journal` reads the answering instance's own host-wide store,
unchanged. If the filter names a routable peer that is not answering, the
result is a `host_unreachable` error, not an empty page — an empty result
would claim "no matching records" about a store that was never consulted,
the exact world-model corruption R3/#1066/`coverage` exist to prevent.
A root filter that is not routable (historical name, undeclared host)
keeps 009's filter-don't-fail semantics on the local store.

`feedback` addresses no root and lands in the journal of the instance
that answered. An agent's friction with the *gateway* belongs on the
gateway; a report about a specific backend can name it in `context`.

## D-7 — The gateway does not journal proxied calls

A proxied edit is journaled once, on the backend, under the caller's
identity. Journaling it on the gateway too would split one action across
two stores with two txn ids and make conflict-rate-per-author (#910)
double-count every proxied edit. The cost: an unreachability window is
visible in `roots` while it lasts but leaves no durable record. Whether
proxy *attempts* (events, never payloads) deserve journal rows is left to
the evidence pass to justify — noted in the record as a follow-up, not
built speculatively.

## D-8 — Timeouts are constants, and an expired in-flight call says the outcome is unknown

Connect 5s, in-flight call 30s, compiled in (`fleet.rs`); no config knob
until real usage argues for one. The two timeouts fail differently, on
purpose: a *connect* failure means the peer never saw the call —
`not_found` / `host_unreachable`, retry when the host is back. An
*in-flight* timeout means the peer may have applied the call —
`internal` / `peer_timeout`, and the message says to check the peer's
journal rather than blindly retry. Blurring those two is how a gateway
turns a slow edit into a double edit. (Retries happen only for failures
provably before the peer processed anything: session establishment and
transport-closed on send.)
