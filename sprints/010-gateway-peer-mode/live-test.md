# Sprint 010 — live test from a real client

*Client-side verification of gateway peer mode, run from **cleo** (Windows,
Claude Code) against the shipped `0.1.0 (e086181 2026-08-07)` build on both
hosts. Distinct from `deploy.md`, which records verification run from the
hosts themselves.*

Two rounds, deliberately:

- **Round 1** — run while cleo still had *both* `kaed-kai` and `kaed-kubs0`
  wired, so the direct connection served as a **control**. This is the only
  window in which a proxied call and a direct call could be compared
  side by side from the same client, in the same session.
- **Round 2** — after restarting the client on the trimmed config, to confirm
  the fleet is reachable with no `kaed-kubs0` present at all.

Test targets: `kubs0:src` → `testing/wowadd/README.md` (added for this) and
`kai:src` → `testing/Toons/README.md` (the original kaed test repo). Both were
restored byte-exact afterwards.

---

## Round 1 — 2026-08-07, pre-restart

### The client change this round justified

cleo was connected directly to *both* instances. `deploy.md` predicted the
gateway would need no client change, and that was right — but "no change
needed" is not the same as "nothing to change". The direct `kaed-kubs0`
connection was redundant, and strictly worse than the gateway for a session
that reached for it: no fleet search, no unreachable-as-data, a half-fleet
`roots` view, and ten duplicate tool schemas. It was removed from
`~/.claude.json`; recorded as **korg #1074** against k-homelab.

kubs0's own instance stays a live direct backend, which is the documented
gateway-down path.

### PD-4 — identity propagation through the hop

The gate this sprint set for itself. A proxied edit sent to kai, addressed to
`kubs0:src`, landed on kubs0 as:

    txn 8 · author "claude" · root "kubs0:src" · intent preserved verbatim

Present in kubs0's journal; **absent from kai's**, whose newest row was still
txn 41 from 01:54 that morning. D-7 holds — the proxy does not double-journal,
and `conflict-rate-per-author` stays countable.

### D-3 — verbatim passthrough, proven against a control

The same edit shape was then made **directly** to kubs0 (txn 9). The two
responses were structurally identical: same fields, same `files[]` shape, same
diff rendering. A response through the gateway is not distinguishable from a
direct one.

Stronger: reading the journal *through the gateway* and *directly from kubs0*
returned **byte-identical output** — same five records, same order, same
`txn_id`s (8/9/10), same `failure_id`s (2/3).

The detail that makes this evidence rather than coincidence: the `coverage`
block returned via the gateway was **kubs0's** (`txns_from
2026-08-03T00:53:12Z`), not kai's (`2026-08-02T05:17:39Z`). D-6's root-filter
proxy genuinely consulted the backend store. Had it filtered kai's local store
instead, `coverage` would have carried kai's dates — the exact world-model
corruption D-6 says `coverage` exists to prevent, and it would have been
invisible in the entries themselves.

### The gateway holds no stale view of the backend

Designed as a trap for a caching bug. Sequence:

1. Gateway edit → version `533dfff0484c630a` (txn 8).
2. **Direct** edit to kubs0, bypassing kai entirely → `cf8bad68f7c41000`
   (txn 9). The gateway never saw this write.
3. Gateway edit declaring base `533dfff0484c630a`.

Step 3 failed with `version_conflict`, and its `delta` was exactly the direct
edit from step 2. The gateway reads through to the live backend; there is no
layer between them holding an opinion about file state.

Re-anchoring purely from that delta's `actual_version` — **no intervening
read** — applied cleanly as txn 10. The
conflict → re-anchor → retry loop the contract promises works identically
through the proxy, which is what makes a `version` usable as a durable content
address across the hop.

### Content addressing survives the round trip

Restoring the file through the gateway produced version `953e8b543a715376` —
byte-for-byte the value read before any test began. The full chain:

    953e8b543a715376  (start)
      → 533dfff0484c630a  txn 8   via gateway
      → cf8bad68f7c41000  txn 9   direct
      → 46d34a5fd26a70ed  txn 10  via gateway, re-anchored
      → 953e8b543a715376  txn 11  via gateway, restore

### Store separation

A local edit on `kai:src` through the same connection that had just proxied to
kubs0 journaled on kai (txn 42→43) while kubs0's counter ran 8→11. kai's
journal held only the Toons row; kubs0's only the wowadd rows. Local routing is
unaffected by the presence of peers.

### Failure paths

All three behaved as contracted, with actionable errors:

| probe | result |
|---|---|
| `stat {root: "*:*"}` via kai | `invalid_input`, naming `search` as the only tool taking patterns (D-5) |
| `read {root: "kai:src"}` via kubs0 | `denied` / `no_peer_credential`, naming both hosts, the author, and **both** remedies (D-2) |
| the two deliberate conflicts | journaled as failures on the **backend** (`failure_id` 2, 3), where they were processed |

The `no_peer_credential` result is correct, not a gap: kubs0 is a deliberate
plain backend with no peer tokens, per the deploy plan.

### Fleet search — the clearest argument for entering through the gateway

The same query (`pattern: "wowadd"`, `root: "*:*"`) run from each side:

| from | roots searched | files | `hosts_unavailable` |
|---|---|---|---|
| **kai** (gateway) | 4, across 2 hosts | 63,148 | kubsdb `deferred` / korg:929 |
| **kubs0** (backend) | 2, its own | 35,137 | kai `no_credential`, kubsdb `deferred` |

kai's `fanout`: `kai:src` 27,761 · `kai:scratch` 250 · `kubs0:src` 34,950
(`classified_hidden: 59`) · `kubs0:k-homelab` 187 (`denied_hidden: 1`).

Both answers are *correct*. kubs0 misses 28,011 files and **says so** rather
than returning a quiet half-answer — D-5's no-silent-truncation rule doing its
job even on the instance that can see less. But only one of the two is the
answer a session should get by default, and that is the whole case for pointing
clients at the gateway.

### Round 1 verdict

Every guarantee R10 makes was exercised against a real client over the real
tailnet, and held. Nothing needed fixing on the kaed side; the only change was
removing a redundant client connection.

---

## Round 2 — post-restart, on the trimmed config

The client was restarted with `kaed-kubs0` removed. Everything below was done
with **no direct kubs0 connection available anywhere in the session** — the
condition round 1 could not create.

### The client is genuinely gateway-only

Confirmed positively rather than by absence: a keyword search of the tool
registry for `kaed` returned ten tools, all `mcp__kaed-kai__*`. The config on
disk survived the restart intact (`kaed-kai`, `klams`, `korg`; still no BOM —
the client rewrites this file on exit, so it was worth re-checking).

`roots` through kai alone returned all four roots, with kubs0 `probe: ok`,
`verified: true`, at the matching build `0.1.0 (e086181 2026-08-07)`.

### A `version` survives a client restart

The proxied edit deliberately declared a base recorded **before** the restart
(`953e8b543a715376`), with no intervening read, across a new process, a new
MCP session and a re-established peer connection. It applied cleanly as
txn 12.

This is the contract's "a version is a content address, not a session handle"
claim tested where it would most plausibly break — through a proxy, across a
reconnect on both legs. No defensive re-read was needed, and one would have
been wasted work.

### Attribution and continuity

txn 12 landed on kubs0 as `author: "claude"`, `root: "kubs0:src"`. Reading the
journal through the gateway showed it directly above round 1's txn 11 — one
continuous history across the client restart, which is the point of journaling
on the backend rather than the client's side of the hop.

Restoring the file produced `953e8b543a715376` for the second time (txn 13).

### Fleet search, with an arithmetic self-check

`*:*` still fanned out over all four roots on both hosts, reporting **63,149**
files — exactly one more than round 1's 63,148, with `kai:src` moving
27,761 → 27,762. That one file is `live-test.md`, written between the rounds.
The fan-out accounting is consistent with a change made through the same
interface it is reporting on.

The search also located the round-2 marker on `kubs0:src` and returned it with
a `version` matching txn 12's `new_version` — search → edit with no read in
between works across the hop.

### Round 2 verdict

A client pointed at kai alone has strictly more capability than one wired to
both, and gives up nothing. Sprint 010's claim that "cleo needs no client
change at all" was accurate about *necessity*; in practice the right change was
to remove the connection that had become redundant.

---

## Where this leaves the fleet

| host | role | client-visible route |
|---|---|---|
| kai | gateway | direct — the only kaed connection cleo has |
| kubs0 | plain backend | proxied through kai; own URL kept as the gateway-down fallback |
| kubsdb | deferred (korg:929) | none, by decision |

When kubsdb's access design settles it joins through kai and needs **no cleo
change** — which is the durable win here, more than any single test above.
