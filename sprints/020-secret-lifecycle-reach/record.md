# Sprint 020 — secret-lifecycle reach

*korg proposal 1465, covering #1231, #1232, #1363. Branch
`020-secret-lifecycle-reach`. Started 2026-08-19. (The proposal's title
says "kaed 019" — it was written before the rotation-grace-windows sprint
landed and took that number.)*

## Goal

Extend the secret lifecycle's reach in the two directions agents actually
hit its edges, then make the public explainer stop lying about the tool.
Three items, strictly ordered — each later one builds on or documents the
earlier:

1. **#1231 (bug, S)** — `secret rotate` with an `also` target on the
   *same host but a different root* is misrouted down the peer path and
   fails `unknown_root`, with an error that lists the root it claims not
   to serve. Fix the partition axis (host, not whole-root-string) and give
   same-host cross-root targets an honest local write path.
2. **#1232 (feature, M)** — value substitution for YAML/compose scalars,
   gated on a scoping pass (count the reachable target set first; settle
   addressing, the bare-scalar-in-a-sequence trap, classification, and
   round-tripping before writing code). Explicitly allowed to carry
   forward if scoping says it exceeds the sprint.
3. **#1363 (chore, S)** — refresh `docs/kaed-explained.html` last, so the
   page is touched once and lands describing the final tool surface.

## #1231 — the `also` route

The report (feedback row #3 on kai, filed during 011/013 work) still
reproduces on `main`. Three steps, all confirmed by reading current code:

1. kai proxies the whole rotate to the primary root's host — correct, R10.
2. On that host, `secret_rotate` partitions `also` targets by comparing
   whole root strings against the primary's (`src/server.rs:1343`), so a
   same-host sibling root (`kubsdb:src` vs primary `kubsdb:datastore`) is
   classed *remote*.
3. `write_value_to_peer` asks `fleet.routable("kubsdb")` **on kubsdb**,
   which is not its own peer, gets `None`, and `explain_unknown_root`'s
   `host == self.host` branch emits "unknown root `src` on kubsdb; this
   host serves […`kubsdb:src`…]".

The fix is a three-way partition: same-root targets keep joining the
primary's atomic transaction (unchanged); same-host-other-root targets get
a **local sibling of `write_value_to_peer`** — their own single-root
transaction, reported per-target; other-host targets keep the peer path.
A cross-root target *cannot* join the primary's transaction — transactions
are single-root by schema (`txns.root`) — so the response says so instead
of pretending: `targets[]` grows a `txn_id` field (see D-1 in
`decisions.md`).

Why it survived to 020: the only cross-root `also` test used a fixture
where every host has exactly one root, so the broken case was
inexpressible. The fixture now supports extra roots per host and the
regression test rotates through the gateway onto a two-root peer.

## #1232 — YAML/compose substitution

(Scoping pass to be recorded here before any code.)

## #1363 — the explainer

(Recorded when reached.)
