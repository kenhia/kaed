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

**`src/perm.rs`** — the new module, and the sprint in one file: is this
failure a permission refusal, is this directory writable, and what does a
refusal say. Everything else calls into it, which is what keeps #1088's
walker, #1091's per-file ops and #1092's probe speaking one vocabulary
instead of three.

**Code**

- `search` / `list` skip and count unreadable entries as
  `unreadable_hidden`, propagated through the fleet-search merge. Two
  things that were *silently* skipped before are now counted: an
  unreadable file (a bare `continue`) and an unreadable directory (which
  used to be fatal).
- `load_text` / `resolve_existing` map EACCES to a structured `denied`.
- `txn::apply` probes writability before the dry-run return, so both
  paths refuse identically, with a reclassifying backstop at `stage()`.
- `remote_target` never proxies a pattern; `fleet_search` does not probe
  hosts the pattern excludes.

**Tests that are the argument, not just coverage**

- `perm::writability_is_a_property_of_the_directory_not_the_file` — D-2
  stated as an executable claim, including an actual write through the
  atomic path to prove the naive check would have been wrong.
- `txn::a_dry_run_into_an_unwritable_directory_refuses_instead_of_promising`
  — asserts the dry and wet errors are the same object, which is the whole
  of D-4.
- `gateway::a_single_peer_pattern_is_expanded_by_the_host_that_was_asked`
  — reproduces kubsdb's topology (declares the fleet, holds no peer
  tokens) and pins both halves of D-5 *plus* the control that an honest
  credential gap still reports.
- `policy::the_kubsdb_classify_shape_holds` — 013's deny-shape test's
  sibling, pinning that the managed compose files stay readable.
- `fsops::kubsdbs_secret_bearing_service_files_classify_opaque_not_redacted`
  — #1093's open question, answered by observation.

**Docs** — R4 gains the OS as the fifth refusing layer, R7 gains
`unreadable_hidden`, R10 gains the pattern-expansion rule; the server's
own instructions and tool descriptions carry all of it, since that is
where an agent actually reads. `docs/overview.md`, `docs/setup.md`'s
troubleshooting table and the explainer get the public version.

**`deploy.md`** — kubsdb's three config edits (drop the `lost+found`
interim deny, add five classify globs, extend the root description), and
the one behaviour change that can make a green call go red.

## What did NOT ship here

**#1094, the live-test re-run, is strictly post-deploy** — running it
against the current build verifies the previous sprint. `live-test.md`
beside this file is the prepared script; it must be run from cleo through
the kai gateway, because that is the vantage point the findings came from
and the host session that wrote the fix can prove nothing about the
contract. Its two bookkeeping obligations (a korg report with `finding`
edges, and a comment on #1085 turning analysis into observation) are
listed there.

## Follow-ups

*(filled in as they appear)*
