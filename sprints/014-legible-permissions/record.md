# Sprint 014 — EACCES made legible, dry_run made honest, kubsdb config finished

*korg proposal 1095, covering #1088, #1089, #1091, #1092, #1093, #1094.
Branch `014-legible-permissions`. Started 2026-08-08.*

## Goal

Close out every finding sprint 013's live test left open. Three of the six
items are one bug class seen from three angles — the OS says `EACCES` and
kaed has no vocabulary for it — so the sprint's real deliverable is that
vocabulary, used consistently by the walker, the per-file operations and
`dry_run`.

## The shape of the sprint

013 shipped kubsdb's broad `/datastore` root and then discovered, from a
real client, that the capability it shipped did not work: one
`drwx------ root root` directory killed every unscoped `search` over the
root. Pulling that thread found two more faces of the same gap and one
unrelated routing bug.

| # | Face of the problem |
|---|---|
| 1088 | the **walker** — one unreadable directory kills the whole call |
| 1091 | the **per-file op** — bare `internal` / os error 13, no path, no reason |
| 1092 | the **prediction** — `dry_run` green for a write that cannot land |
| 1089 | unrelated: single-peer root pattern proxied wholesale |
| 1093 | config-only: classify kubsdb's secret files, route the managed set |
| 1094 | the verification: re-run 013's live test from cleo |

## Decisions

See `decisions.md`. The load-bearing ones:

- **D-1** — EACCES is `denied` with two new reasons, not a new error code.
- **D-2** — the answer kaed gives is about the **parent directory**, because
  that is what kaed's atomic write actually needs. A root-owned `0644` file
  in a writable directory *is* writable through kaed, and the naive
  file-mode check would have called it unwritable.
- **D-3** — the "editable copy" hint is not hardcoded: it is the root's own
  `description`, carried into the error. That is what makes #1093's advisory
  and #1091's hint the same words by construction rather than by discipline.
- **D-4** — `dry_run` probes for real (option 1 of #1092), and the probe runs
  on the real write path too, so there is one code path and one vocabulary.
- **D-5** — patterns never proxy; the answering instance always expands them.

## What shipped

*(filled in as it lands)*

## Follow-ups

*(filled in as they appear)*
