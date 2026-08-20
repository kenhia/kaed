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

## #1232 — YAML/compose substitution: scoped, and the answer is "don't build it"

The proposal gated this on a measurement — count the reachable target set
before settling any design question — and the measurement ended the item.
Run 2026-08-19 through the kai gateway against the live fleet (7 roots,
3 hosts):

- **Compose files, fleet-wide** (`**/*compose*.y*ml`, 32 files searched):
  every secret-shaped hit in a reachable file is a `${VAR}` interpolation
  resolved from a dotenv, a comment, or a test literal. Zero literal
  credentials.
- **All YAML, fleet-wide** (`**/*.y*ml`, 312 files searched, pattern
  requiring a literal-looking value after the key): exactly **one** match,
  `POSTGRES_PASSWORD: klams_test` in klams's *test* compose file.
- **The motivating set — kubsdb's postgresql/mongodb/redis compose files
  (#841) — is unreachable by ownership, not shape**: `0600 root:root`,
  refused `not_readable_by_service_identity` with 014's root advisory.
  The readable YAML on `kubsdb:datastore` (grafana, korg, kwebi,
  package-store, prometheus, registry, unpoller) holds no literals; the
  managed dirs' sources on `kubs0:k-homelab` were searched too and are
  clean.

So a YAML substitution op shipped today would have **zero files it could
act on**. The WI framed the shape gap and the ownership gap as separable
halves; the measurement says they are the *same three files* — and the
durable fix for those is host-side: extract the literals to dotenv +
`${VAR}`, the convention every other service in the homelab already
follows, at which point kaed's **existing** lifecycle reaches them and
the op is permanently unnecessary. Filed as a k-homelab work item;
#1232 parked with the un-park trigger recorded (a reachable YAML file
holding a literal secret actually existing). Design questions 2–5 were
deliberately not settled — the gate exists so they don't get designed
for an empty target set.

This is the proposal's blessed exit ("ship 1231 + 1363 and carry 1232"),
taken one step further because the finding was about the premise, not
the size.

## #1363 — the explainer

Done last as ordered, and the ordering paid off in a small way: with #1232
parked, the tool count stays twelve and the page was touched exactly once.
What changed in `docs/kaed-explained.html`:

- **kubsdb is live**, not "deferred by design" — the card now tells the
  013 story (broad access made a decision: data denied by shape, managed
  config labeled). This was the highest-value fix; the same stale claim in
  another copy already produced a wrong conclusion once (k-homelab #1201).
- **"Six tools" → twelve**, named by group; the status blurb also says
  three hosts, not two. The roadmap rows moved with it: fleet deploy and
  history are `shipped`, and two shipped rows the old page predated —
  secrets and the gateway — were added. "Structure" stays `next`.
- **The architecture SVG is captioned as the simple case** rather than
  redrawn (the judgement call the WI left open): the figure is still a
  fair picture of one agent → one host, and the caption now says the same
  connection reaches the fleet by proxy under the caller's identity.
- **The sweep for pre-010/pre-008 claims found two more falsehoods**: the
  page listed `.env` (and `*.pem`, `id_*`) as *deny* globs, but since 008
  those classify instead — `DEFAULT_DENY` is only the credential stores
  (`.ssh`, `.gnupg`, `.aws`, `.netrc`, `.git-credentials`, `.config/gh`).
  Fixed in both the deny-layer card (which now explains the
  deny-vs-classify split) and the SVG's refused box. The dogfood-report
  row was also updated honestly: the usage report exists and is
  re-runnable, the ssh head-to-head is still owed.
