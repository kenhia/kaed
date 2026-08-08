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

## Verified from a real client

`live-test.md` beside this file, run from cleo through the kai gateway
against `0.1.0-88005bc` after the deploy — the vantage point the findings
came from, and the one the kai host session that wrote the fix could not
provide. **Every 013 finding closed, no regressions, and nothing filed:
the first live test in this program to end that way.**

It also did something better than confirming the fix. The `#1091` hint
**corrected the original finding's own misdiagnosis** — 013 recorded
`prometheus.yml` as a root-owned *644 file* that would not write, and
reasoned from the file's mode to the right conclusion for the wrong
reason. The real obstacle is the containing directory (D-2), and the
refusal now says so in as many words. An error that tells you your model
of it is wrong is the whole point of the sprint.

And `unreadable_hidden` earned its keep on a host the sprint was not about:
`kubs0:src` reports 1, a pre-existing condition (broken symlinks in a
third-party checkout) that was silently invisible before. That is the
argument for fixing the class rather than adding `lost+found` to a deny
list, made by accident.

## Follow-ups

- **The hidden counters are lower bounds when `truncated`.** The live test
  got 1, 2 and 6 unreadable entries for one root across three sessions and
  ran the discrepancy down: the walk runs to exhaustion, so what it filters
  is counted in full, but anything found by *opening* a file is only
  counted as far as `max_results` allowed. Not a defect — the response
  already says it is partial — but it was undocumented, and three sessions
  disagreeing about one number is the cost of leaving it that way. Now
  stated in R7, in the `search` tool description, and pinned by
  `search::a_truncated_search_reports_lower_bound_counters_and_says_it_is_truncated`.
- **The kubsdb classify globs are dormant.** Those files are 0600
  root:root, so the OS refuses before kaed can read bytes to classify them
  and the refusal is `not_readable_by_service_identity`. Correct ordering
  and the more informative answer; the globs arm themselves if a mode ever
  changes, which is the korg #1085 scenario they were added for. Worth
  knowing before someone concludes they do nothing and removes them.
