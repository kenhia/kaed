# Sprint 016 — live test from cleo

*2026-08-12. Fresh Claude Code session on cleo against `kaed-kai`
`0.1.0-57cf254`. kubs0 and kubsdb on `0.1.0-0743ff0`.*

## Verdict

**The #1212 gate passes. A different failure blocks the fleet move.**

Tool registration — the only thing that could see 015's failure mode — is
good: all **12** `mcp__kaed-kai__*` tools registered and resolved schemas.
No empty tool list, no healthy-looking dead connection.

Every call that kai answers from its own roots works. Every call addressed
to a **peer root** fails at the client, before I see a result.

## What was run

| Call | Result |
|---|---|
| tool registration | 12 tools, schemas resolve |
| `roots` | 7 roots; kubs0 + kubsdb probed `ok` and `verified` |
| `list kai:src ai/kaed/…` | ok |
| `stat kai:src …/record.md` | ok, `version 87d7dfb01afe189a` |
| `read kai:src …/record.md` | ok |
| `search *:src` (fleet fan-out) | ok — matches from **all three** hosts, `fanout` per root |
| `stat kubs0:k-homelab` | **FAIL** |
| `list kubs0:k-homelab` | **FAIL** |
| `stat kubsdb:src` | **FAIL** |

The three failures are identical:

```
MCP server "kaed-kai" returned a malformed result that failed schema
validation: Invalid result for tools/call: missing required resultType —
servers implementing protocol revision 2026-07-28 MUST include it (the
absent-means-complete bridge applies only to earlier-revision servers)
```

The client rejects the envelope, so this is not a kaed error result — the
call's actual outcome never arrives. Confirmed on two tools and two hosts,
so it is the proxy path, not one tool and not one peer.

## Mechanism

D-1 and D-2 are both correct in isolation and collide in the gateway:

- kai **serves** `2026-07-28` to this client. Under that revision
  `resultType` is mandatory and the absent-means-complete bridge is
  explicitly unavailable.
- kai **asks** peers at `PEER_PROTOCOL_VERSION = 2025-11-25` (D-2), so a
  peer's `CallToolResult` correctly carries no `resultType`.
- The gateway returns that peer result **verbatim**, so the legitimately
  absent field rides out onto a `2026-07-28` session and is no longer
  legitimate.

rmcp's `strip_result_type_for_legacy_peer` handles the modern→legacy
direction for results rmcp builds itself. Nothing stamps a result that
arrived from a peer already deserialized.

`search` survives because kai merges peer matches into an envelope it
builds itself. `roots` survives for the same reason — the peer probes are
kai's own response. Only *direct addressing* of a peer root passes a peer
envelope through.

## Why the kai battery missed it

record.md's `list kubs0:src through the gateway → 12 entries, no error` was
run from an rmcp client, which **per D-2 cannot drive a `2026-07-28`
session**. That test necessarily ran at `2025-11-25`, where the missing
field is legal and the passthrough is correct.

The failing combination is *2026-07-28 client* **and** *proxied call*, and
it is structurally unreachable from kai — it needs the one client D-2 says
rmcp cannot be. This is the same shape as 015: the server-side battery is
green and the client half is where the contract actually bites.

## Consequence for the rollout

**`deploy-fleet` does not fix this.** Upgrading kubs0 and kubsdb to the new
build leaves `PEER_PROTOCOL_VERSION` at `2025-11-25` — the pin is about
rmcp's *client*, not the peers' builds — so kai would still ask for
`2025-11-25`, still get results without `resultType`, and still pass them
through. Moving the fleet spreads the build without closing the hole.

Blast radius while it stands: from a `2026-07-28` client, kai:* is fully
usable and fleet `search` still reaches every host, but `kubs0:*` /
`kubsdb:*` cannot be read, stat'd or **edited** — the gateway's whole
point since 010.

## Options

1. **Roll back kai** to `0.1.0-0743ff0`. Known-good: that build capped at
   `2025-11-25` and its gateway was live-tested green from cleo earlier
   today. Costs the revision, restores peer editing, moves nothing else.
2. **Fix forward**: stamp `resultType` on proxied results before returning
   them, keyed on the *serving* session's negotiated revision — the
   mirror of `strip_result_type_for_legacy_peer`, in `fleet.rs` on the way
   back out. A gateway that translates between two revisions has to own
   both directions, which is arguably the decision D-2 half-made.

Either way a regression test wants the shape "proxied call on a
`2026-07-28` session carries `resultType`", which needs a raw-JSON client
rather than rmcp's.

---

## Resolution — fixed forward

**Option 2**, in `proxy_to_peer` rather than `fleet.rs` (that is where the
serving session's revision is in hand, and it covers proxied *error* results
by the same path). Recorded as **D-4**; regression test is
`a_proxied_result_is_stamped_for_the_revision_it_is_returned_on` in
`tests/gateway.rs`, raw JSON-RPC as this document called for.

Two things were checked before choosing, rather than assumed:

- **Reproduced in-repo and on the canary.** Raw `2026-07-28` calls to kai:
  `stat kai:src` → `resultType: "complete"`; `stat kubs0:src` and `stat
  kubsdb:src` → field absent. The analysis above is exactly right.
- **"Only kai is deployed" was tested as a hypothesis and does not hold.**
  kai's own new build, asked for a `2025-11-25` session, omits `resultType`
  too — which is precisely what an upgraded peer would send kai, since the
  pin is on what kai *asks for*, not on what the peer can serve. Deploying
  the fleet would have spread the build without closing the hole, as §"Consequence
  for the rollout" says.

Second canary: **`0.1.0-a7f81ef`**, kai only. Every call listed as FAIL above
now returns `resultType: "complete"` — both hosts, both tools, and the
proxied *refusal* path too. `record.md` has the table.

**Still needs a re-run from cleo.** Server-side is as green as it can be
made from kai, and the whole point of this document is that green-from-kai
is not the gate.

---

## Re-test from cleo — `0.1.0-a7f81ef`: PASS

*2026-08-12, fresh Claude Code session on cleo after an app restart. kai on
`a7f81ef`; kubs0 and kubsdb still on `0743ff0` — deliberately the same
mixed-build state that produced the failure above.*

**Every call that failed in the first pass now succeeds.** The new gate — a
real `kubs0:*` / `kubsdb:*` call, not tool registration — is met.

| Call | Result |
|---|---|
| tool registration | 12 tools, schemas resolve |
| `stat kubs0:k-homelab` | ok — **was FAIL** |
| `list kubs0:k-homelab` | ok, 18 entries, `denied_hidden: 1` — **was FAIL** |
| `stat kubsdb:src` | ok — **was FAIL** |
| `read kubs0:k-homelab inventory.yml` | ok, 45 lines, `version eaad82eb5715155a` |
| `list kubs0:k-homelab secrets` | `denied` — correct refusal, full `data` |
| `stat kubsdb:src no/such/path` | `not_found` — correct refusal |
| `edit kubs0:k-homelab` (dry run) | ok — diff + `old_version`/`new_version` |
| `edit kubs0:k-homelab` (stale base) | `version_conflict` with `actual_version` |
| `search *:*` | ok — all 7 roots, 36,868 files, per-root hidden counters |

Both edits were `dry_run`. Nothing was written to any peer.

The refusal rows are the ones worth having. A proxied `denied` came back
with its `reason`, `rule` and `hint` intact, and a proxied `version_conflict`
with its `actual_version` — so the fix carries error *data* across the hop,
not just success envelopes. In the first pass these were unreachable: the
client rejected the envelope, so a refusal and a success were
indistinguishable from cleo.

### What this test can and cannot see

It observes that **the client accepts the envelope** — it does not read the
`resultType` field directly. The raw-JSON assertion that the field says
`"complete"` lives in `tests/gateway.rs`; this pass confirms the thing that
test cannot, which is that a real client on a real `2026-07-28` session is
satisfied. Stating it that way on purpose: mistaking one test for a
neighbouring one is what this sprint was about twice.

### Cleared for the fleet move

The post-`deploy-fleet` state should be indistinguishable from what was just
tested — upgraded peers are still *asked* at `PEER_PROTOCOL_VERSION =
2025-11-25` and will still answer as legacy, so the stamp in `proxy_to_peer`
stays on the same path it is on now. Worth one confirming `kubs0:*` call
after the deploy anyway, on the same principle as everything above.
