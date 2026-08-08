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

## Follow-ups

- D-6 names a cheap future improvement: a structured "not writable by the
  service identity" error, gated on journal evidence that agents actually
  hit root-owned files.
- After deploy, the next journal-report pass gains its first third-host
  column; the roots decision here is exactly what that pass should audit.
