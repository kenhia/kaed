# Sprint 013 — kubsdb: decide broad access, then deploy it

*korg proposal 1062, covering #929. Branch `013-kubsdb-broad-access`.
Started 2026-08-08. The last slice of program 1063 (PD-1).*

## Goal

Answer the four questions #929 left open — what "broad" means on the data
host, `/gratch`, the rsync-target problem, defaults vs. per-host — and
deploy the third kaed instance with those answers, flipping kubsdb's
declared-peer entry from `deferred` to `active`.

## The shape of the sprint

This is a decision sprint with a deploy at the end, not a feature sprint:
every mechanism it needs already shipped (evidence 006, classify/redact
008, root descriptions 007, gateway 010). The work was to make the
decisions against the host as it actually is, pin the one thing that could
silently betray them (matcher semantics), and record the target state for
`deploy-fleet` to execute from merged main.

## What the fresh recon changed

A live pass over kubsdb (recorded in `decisions.md`) mattered twice:

- **The `/datastore` layout turned out to be uniformly** compose + config
  above a `data/` subdirectory — which turned "config yes, data no" from a
  per-directory judgment into a single lexical shape, `/datastore/*/data`.
- **The homelab package store moved onto the host since 004's recon**
  (`packages/`, `package-store/`, `registry/`). Nothing in #929 or the
  proposal anticipated it; it got its own decision (D-2, denied as fleet
  supply chain) rather than riding in silently as "more config".

## Decisions

See `decisions.md`: D-1 broad-by-shape (`/datastore` rooted, data denied
lexically), D-2 package store denied, D-3 `/gratch` closed not open, D-4
deploy targets get advisory root descriptions instead of denial, D-5 the
three roots, D-6 unix ownership accepted as the fourth layer, D-7 nothing
enters the shipped defaults.

## What shipped in the repo

- `deny.rs::the_kubsdb_broad_access_shape_holds` — pins the exact deny
  globs kubsdb's config carries: absolute patterns, ancestor matching,
  separator-crossing `*`. The matcher-testing instruction #929 carried,
  discharged in CI.
- `deploy/config.example.toml` — notes that absolute deny globs are legal,
  with the `/srv/*/data` shape as the example.
- This sprint record; `deploy.md` holds the target configs and the
  verification battery for ship time.

## Verified from a real client

`live-test.md` beside this file records the client-side round, run from
cleo through the kai gateway and picking up the five battery items
`deploy.md` deferred. D-1 … D-5 all hold as specified, and #929's central
difficulty resolves in a single listing: `postgresql/data` denied by shape
while `korg/korg.env` reads redacted in the same root. PD-4 attribution
held on the third host with no extra wiring (txn 1, `author: claude`).

It also turned up three findings — F-1 and F-2 filed through `feedback`
(kai journal ids 1, 2) — carried into the follow-ups below.

## Follow-ups

- D-6 names a cheap future improvement: a structured "not writable by the
  service identity" error, gated on journal evidence that agents actually
  hit root-owned files. The host-side half — a `composer` group making
  that config group-writable, plus a mechanism for catching new root-owned
  files — is korg #1085 (claude-cleo, brainstorm).
- After deploy, the next journal-report pass gains its first third-host
  column; the roots decision here is exactly what that pass should audit.
- **F-1 (live test, feedback #1) — the one to fix.** `/datastore/lost+found`
  is `drwx------ root root`, so an unscoped `search` on `kubsdb:datastore`
  dies with a bare EACCES `internal` error: the broad root this sprint
  shipped is unsearchable without a `path` scope. Immediate fix is adding
  `/datastore/lost+found` to the deny list; the general fix is skipping
  EACCES during the walk and reporting it as a per-root count beside
  `denied_hidden`, so no future unreadable directory can do this again.
  The recon lists `lost+found/` but no D-n covers it — it is neither config
  nor `data/`, so D-1's shape rule misses it. *Filed as korg #1088; the
  interim deny entry is applied on the host and reflected in `deploy.md`.*
- **F-2 (live test, feedback #2)** — a root pattern naming exactly one peer
  (`kubsdb:*`) is proxied wholesale under 010 D-1, so the peer runs the
  fan-out and its `hosts_unavailable` reaches the caller describing the
  *peer's* reachability. It reported kai and kubs0 as `no_credential` to a
  client connected through kai. `kai:*` and `*:*` expand locally and are
  unaffected. Results are correct; only the field meant to prevent
  world-model corruption is wrong. *Filed as korg #1089.*
- **F-3 (live test)** — D-6's accepted gap presents worse than recorded:
  root-owned 600 files are listed (not invisible) before failing; read,
  write and search all collapse to the same bare `internal` os-error-13
  with no path, reason or hint (the journal row is strictly more
  informative than the agent's error); and `dry_run` returns a clean diff
  for a write that cannot succeed. Strengthens the case for D-6's own
  "not writable by the service identity" follow-up — the journal evidence
  it was gated on now exists (`failure_id 1`). *Evidence added to korg
  #1085; per Ken's read there, if the composer group lands, this class
  mostly becomes a non-issue and the tool-side error likely stays unbuilt
  (dry_run's writability blindness being the part that outlives it).*
