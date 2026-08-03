# Brainstorm — one gateway MCP vs. one MCP per host

*Status: brainstorm, nothing decided. 2026-08-02, from a session with Ken.*
*Companion to `brainstorm-secrets-editing.md`; touches WI #914 (token rotation)
and #910 (per-author conflict metrics).*

## The question

kaed must run *on* each target — local edits are most of the point. But that
means a client wires up `kai_kaed`, `kubs0_kaed`, `kubsdb_kaed`, each exposing
the same six tools. Ken's proposal: a single **gateway MCP** (probably on the
calling host), with tools re-signatured as `tool_call(host, ...)` plus a
`list_available_targets`.

Verdict: **better for the agent, and the gap widens with fleet size** — but the
win comes from somewhere other than tool count, and one thing breaks quietly if
it isn't designed for.

---

## The structural insight: `root` is already the indirection

kaed's tools don't take paths, they take `(root, root-relative path)`. Roots are
*named* and resolved server-side, and `roots` is already the discovery tool. So
the gateway doesn't need a `host` parameter at all — it needs **host-qualified
root names**:

```
kai:src, kai:scratch, kubs0:src, kubsdb:data
```

Every tool signature stays byte-identical. `mcp-contract.md` doesn't fork.
There's no new "which host" argument to get wrong, and no second addressing
vocabulary.

Corollary: **don't add `list_available_targets`.** `roots` *is* that tool; it
just gains `host`, `status`, and `capabilities` fields per entry. Two discovery
mechanisms answering the same question is worse for an agent than either alone.

The gateway is cheap here precisely because v0's addressing was already
indirection-shaped. It would be expensive in a server that took absolute paths.

---

## The tool-count argument is the weakest one

At N=2 hosts × 6 tools that's 12 schemas, and harnesses increasingly load MCP
schemas lazily — a Claude Code session lists `mcp__kaed__*` by name and fetches
schemas on demand. So per-host context cost is lower than the raw count
suggests, in *some* clients. Don't build the gateway for tool count. Build it
for the three below.

### 1. Unreachable-host-as-data, not connection failure

The big one. Per-host: kubs0 is down → that MCP server fails at session start,
and the agent cannot tell whether the host is down, the token rotated, or Ken
never wired it. Gateway: `roots` returns
`{name: "kubs0:src", status: "unreachable", since: ...}` — a fact the agent can
reason about and report. That matches the posture of the rest of the contract
(structured errors, explicit truncation) instead of fighting it.

### 2. Fleet-wide search in one call

"Where does this value appear across the fleet" — exactly what the
rotate-everywhere idea in `brainstorm-secrets-editing.md` needs — is a manual,
serial, merge-it-yourself chore across N servers, and a single parallel call
through a gateway. A capability, not just ergonomics.

### 3. One auth surface

One token in the client config instead of N, and #914's rotation story stops
being O(hosts × agents) client-side. Trade: that token is now fleet-wide, so its
compromise is worse. Name it up front. (And kaed reads token env only at
startup — that gotcha now bites at the gateway too.)

---

## The thing that must not break: journal identity

The journal attributes every edit to an author. If the gateway proxies under its
own credential, **every edit on kubs0 is journaled as "gateway"** and the audit
story — who edited what, conflict-rate-per-author (#910), "has any agent ever
seen this" — collapses into one indistinguishable actor.

Identity propagation is therefore a hard requirement. Options, in rough order of
trust:

1. Gateway forwards a **signed author assertion**.
2. Backends accept a forwarded-author header **only from an allowlisted peer**
   presenting its own credential.
3. Gateway holds a **per-agent token for each backend** and proxies with the
   caller's real identity — nothing about backend auth changes at all.

(3) is simplest and keeps the auth model untouched, at the cost of giving up win
#3 above. Given the choice, take identity fidelity over token-count reduction.

---

## Other risks, all manageable

- **Version skew.** If kai has tree-sitter node edits and kubs0 doesn't, what
  does the gateway advertise? Advertise the **union**, with per-root
  `capabilities` in the `roots` response, and return a structured
  `unsupported_capability` on a call to a root that lacks it. Do *not* advertise
  the intersection — that silently hides working features on the most-updated
  host.
- **Error passthrough must be verbatim.** `version_conflict` with its delta,
  `ambiguous_anchor` with its candidates — design principle 4. A gateway that
  rewraps errors in its own envelope breaks the contract. Rule: proxy the error
  object unchanged, add a root tag, nothing else.
- **Merged response budgets.** A cross-host search needs one budget with
  *per-host* truncation reporting. Principle 3 calls silent truncation
  corruption of the agent's world model; a naive merge is how you'd introduce it.
- **Failure domain.** Gateway down = fleet down. Mitigate by keeping the direct
  per-host URLs documented and working as a fallback.

---

## Where it runs

Resist a new binary. Make **gateway a mode of kaed itself** — a `[peers]` block
in `config.toml`, so any instance can proxy to its peers and whichever one you
point at becomes the gateway. No second artifact, no second deploy recipe, no
second contract doc, and a peer's version is trivially discoverable because it
speaks the same protocol.

Running it on kai rather than cleo is probably right: one Linux host with
tailscale serve already configured, versus a Windows process subject to the
MSIX/PATH gotchas. Cost is a second tailnet hop for kubs0 calls
(cleo → kai → kubs0), single-digit milliseconds against a round trip already
being paid.

---

## Performance

Negligible, given two things: **persistent HTTP connections** to each peer (no
reconnect per call) and **parallel fan-out** for multi-root operations. A
tailnet proxy hop is noise next to the existing ts.net round trip, and kaed's
operations are I/O-bound on disk and network, not CPU.

What *would* cost: a gateway that fully deserializes, revalidates, and
re-serializes every payload. Proxy the body through — the gateway needs to parse
only enough to route on `root`.

---

## Suggested sequencing

The addressing change is what locks you in, and it's nearly free today. Split it:

1. **Now** — make root names host-qualified and add `host` / `status` /
   `capabilities` to the `roots` response, even with a single instance. Nothing
   in the contract changes later, and a client wired directly to kai already
   speaks the gateway's vocabulary.
2. **When kubs0 + kubsdb land, or when fleet-wide search is wanted** — whichever
   comes first — turn on peer mode. By then the addressing is already right and
   it's a routing feature, not a redesign.
