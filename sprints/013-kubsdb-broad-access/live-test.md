# Sprint 013 — live test from a real client

*Client-side verification of kubsdb's broad access, run from **cleo**
(Windows, Claude Code) through the **kai gateway** against
`0.1.0 (0ed27cc 2026-08-07)` on all three hosts. Companion to 010's
`live-test.md`; distinct from `deploy.md`, which records verification run
from the hosts themselves.*

This run picks up the five battery items `deploy.md` explicitly deferred to
it — fleet-search fan-out, hvsim dry_run, journal read, root-owned io
error, value-probe search — and checks each decision D-1 … D-6 against the
host as a client actually sees it.

**The client is gateway-only.** cleo dropped its direct `kaed-kubs0` entry
in 010 (korg #1074) and never had one for kubsdb, so every call below was
proxied by kai. Notably **no client restart was needed** for kubsdb to
appear: `roots` is computed server-side, so flipping `[peers.kubsdb]` made
the three new roots addressable in an already-running session. (New
*tools* — `secret`, `secret_reveal` from 011 — did arrive too, so the
client did re-fetch its tool list.)

**Test repo:** `kubsdb:src` → `testing/kaedprobe`, created through the
gateway as the host's very first transaction and then `git init`ed.
`~/testing` was the natural lean but sits **outside every root kubsdb
serves** (D-5 roots `~/src`, not `$HOME`), so it went under `~/src` to
match the `testing/` convention already used on kai (`Toons`) and kubs0
(`wowadd`).

---

## What passed

### Reachability and D-5's three roots

`roots` on kai probes kubsdb live: `status: active`, `verified: true`,
`probe: ok`, version matching the other two hosts. The `deferred` /
`ref: korg:929` entry is gone, replaced by the sprint's note. All three
roots — `kubsdb:datastore`, `kubsdb:hvsim`, `kubsdb:src` — are addressable
through the gateway.

`*:src` now fans out across all three hosts in one call (63,788 files;
kai 27,764 · kubs0 34,950 · kubsdb 1,074), which is exactly the uniformity
D-5 gave as its reason for including `kubsdb:src` at zero marginal cost.

### D-1 — broad-by-shape, both halves

The deny half, seen from a client:

| call | result |
|---|---|
| `list kubsdb:datastore` | 11 entries, `denied_hidden: 2` (`_retired`, `packages`) |
| `list kubsdb:datastore postgresql` | only `docker-compose.yml`, `denied_hidden: 1` (the `data/` dir) |

The classify half — **#929's central difficulty, resolved in one
listing**. `postgresql/data` is denied by shape while `korg/korg.env`,
its neighbour in the same root, reads:

```
DATABASE_URL=⟨kaed:DATABASE_URL@05119dd15299304b⟩
KORG_TIMEZONE=⟨kaed:KORG_TIMEZONE@e1e43d10d5b8e8a5⟩
```

with `redacted: true`, per-key `dotenv` metadata (length + shape) and the
`set -a` usage hint. Editable, attributed, revertable — and no value
crossed the wire. That is the deny-vs-classify distinction doing the job
PD-1 sequenced it for.

### The value probe (battery item)

Stronger than a literal probe, because it needs no known plaintext:
`search` for `://` scoped to `korg` returned **zero matches over two files
searched** — and `korg.env` is one of those two, holding a `DATABASE_URL`
whose value is a URI. The value is present on disk, was searched, and
contributed nothing. Separately `postgres(ql)?://` over `kubsdb:src`
returned zero matches with `classified_hidden: 2`. Values are unsearchable
by construction, and the hiding is *counted* rather than silent.

### D-2 — package store denied

`read kubsdb:datastore packages/registry/index.json` → `denied`,
`reason: server_denylist`, `rule: /datastore/packages`, with a hint saying
the policy is permanent and no path variation will succeed. The refusal
names the rule that caused it, which is what makes it actionable rather
than mysterious.

### D-3 — `/gratch` is closed, structurally

`stat kubsdb:datastore ../gratch/bitlocker-keys` → `outside_root`. Not a
deny rule that could be edited away — there is simply no root that reaches
it, which is the stronger form.

### D-4 — deploy targets are told, not denied

A `dry_run` create against `kubsdb:hvsim` returned a clean diff with
`applied: false`: the root is fully writable, exactly as decided. The
advisory rides the `roots` description verbatim — *"DEPLOY TARGET: source
of truth is fun/hv-simulator on kai; `just deploy` overwrites
compose.yaml"* — and `kubsdb:datastore`'s description carries the
`kwebi/app` advisory the same way. PD-5's criterion holds: the answer is
in the surface every client already reads before addressing a root.

### PD-4 on a third host

The test repo landed as **txn 1, `author: "claude"`, `root: "kubsdb:src"`**
— one atomic three-file transaction, journaled on kubsdb, read back
through the gateway. Identity propagation holds on the newly added host
with no additional wiring, which is the payoff of doing 010 first.

A nice honesty detail: kubsdb's `coverage` reports `txns_from` as the
timestamp of that very transaction. A brand-new host does not pretend to
have history.

---

## Findings

Three things a client sees that the sprint record does not predict. Two are
filed through kaed's own `feedback` channel (kai journal, ids 1 and 2).

### F-1 — `lost+found` makes `kubsdb:datastore` unsearchable *(feedback #1)*

`search` on `kubsdb:datastore` fails outright:

```
internal: /datastore/lost+found: IO error for operation on
/datastore/lost+found: Permission denied (os error 13)
```

`/datastore/lost+found` is `drwx------ root root`; kaed runs as ken, the
walker cannot descend, and the whole call dies. Reproduced three ways —
concrete root (hard failure, zero results), `kubsdb:*`, and `*:*` (a
per-root `error` entry in `fanout` with zero `files_searched`). A
`path`-scoped search works, which is the workaround.

This lands on the sprint's headline capability: `/datastore` is the broad
root 013 exists to open, and unscoped search over it does not work. The
recon in `decisions.md` lists `lost+found/` in the layout but no D-n covers
it — it is neither config nor `data/`, so the shape rule misses it.

Credit where due: the fan-out case degrades honestly (the error is
attributed to its root, so D-5's no-silent-truncation rule holds even
here). Only the concrete-root call is a hard failure.

One-line fix is `/datastore/lost+found` in the deny list. The general form
is worth a thought though: *any* unreadable directory under a root does
this, so skipping EACCES and reporting it as a per-root count — alongside
`denied_hidden` / `classified_hidden` — would make the class non-fatal
instead of needing a deny rule per occurrence.

### F-2 — a single-peer root pattern is proxied wholesale *(feedback #2)*

`search {root: "kubsdb:*"}` came back carrying:

```jsonc
"hosts_unavailable": [
  {"host": "kai",   "status": "no_credential", "author": "claude"},
  {"host": "kubs0", "status": "no_credential", "author": "claude"}
]
```

That is *kubsdb's* world-model — it holds no peer tokens by design — passed
through verbatim. From the caller's frame it is false: kai is the host it
is talking to, and kubs0 is reachable through it.

This looks like 010 D-1 (route on the raw `root` string when its host
prefix names a routable peer) firing ahead of 010 D-5 (expand patterns
locally against known roots). `kubsdb:*` has host prefix `kubsdb`, which
names a routable peer, so the whole call forwards.

Only the single-peer case is affected: in the same session `kai:*` and
`*:*` both expanded locally and returned no `hosts_unavailable` at all.
Results are correct and nothing is lost — the damage is confined to the one
field whose entire job is stopping an agent forming a wrong picture of what
was *not* looked at. An agent reading it could reasonably conclude the
fleet is degraded when it is healthy.

### F-3 — D-6 presents worse to a client than D-6 describes

D-6 accepts the unix-ownership gap, and that acceptance is sound. But the
recorded shape differs from the observed one in three ways:

1. **Root-owned 600 files are not "invisible".** `list postgresql` shows
   `docker-compose.yml` with its 388-byte size; the file is discoverable.
   Reading it then fails. Invisible would arguably be kinder than
   discoverable-then-opaque.
2. **The error teaches nothing.** Read, write and search all collapse to
   the same bare `internal` / `"Permission denied (os error 13)"` — no
   `path`, no `reason`, no hint, and nothing saying this is an accepted
   condition rather than a kaed defect. By the contract's own standard
   (structured errors carry recovery data) this is the one error shape that
   carries none. The journal record is strictly better: `failure_id 1`
   names `prometheus/prometheus.yml`, which the response to the agent did
   not.
3. **`dry_run` gives a false green.** A dry_run write to root-owned
   `prometheus/prometheus.yml` returned a clean diff and a plausible
   `new_version`; the identical real write failed. An agent validating a
   risky edit with dry_run first — the thing dry_run is for — learns
   nothing about whether it can actually write.

None of this argues against D-6's decision. It argues that the cheap
follow-up D-6 already names (a structured "not writable by the service
identity" error) has a clearer case than "if journals later show agents
hitting these" — the journal now shows exactly one such failure, filed
deliberately, and the surrounding evidence is in this file.

---

## Verdict

Every decision D-1 through D-5 holds as specified when exercised by a real
client through the gateway, and #929's central difficulty — `postgresql`
beside `korg.env` — resolves cleanly in a single directory listing. The
three findings are all in the seams the sprint knowingly left: two are
EACCES handling (F-1, F-3), one is a 010 routing interaction the pattern
namespace made reachable for the first time (F-2).

F-1 is the one worth fixing before the root is used in anger: unscoped
search over `/datastore` is the capability this sprint shipped, and it does
not currently work.
