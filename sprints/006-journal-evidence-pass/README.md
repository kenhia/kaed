# Sprint 006 — journal evidence pass

*korg proposal 1054, slice 1 of program 1063. Covers WI #1044.*
*2026-08-06. No code changes to the daemon.*

## What this was

The first pass over kai's and kubs0's journals, asking what agents actually do
with kaed rather than what we assume they do. It exists because Ken asked how to
get feedback "from the agent's view", and the answer taken (PD-6) was
**instrument, don't survey**: kaed has recorded revealed preference since #910
journalled failed transactions, and that evidence costs a `SELECT`.

Three later slices were waiting on it — #929 asks for exactly this in its own
text, #1049 should expose query axes that proved useful rather than guessed
ones, and #1046 needs to know where friction clusters.

## What shipped

- **`docs/agent-usage-report-2026-08-06.md`** — the report. Written as a
  *baseline*, not a study; see below on why that framing matters.
- **`scripts/journal-report.py`** — the queries, committed and re-runnable, so
  the follow-up pass in a few weeks is one command instead of archaeology.
  Verified against both hosts, including the documented cross-host invocation.
- Planning artifacts from the sequencing session ride along in this branch:
  `sprints/planning/decisions.md` (PD-1..PD-6) and the roadmap header pointing
  at it.

## The headline

**kaed is doing the job it exists for, and what it edits is overwhelmingly
documentation.** 81% of file touches are `.md`/`.html`, line churn is 16:1
additive, kubs0 is 100% markdown, and *no Rust source file has ever been edited
through kaed outside the two verification fixtures* — in a homelab whose most
active projects are Rust.

Those two clauses are not in tension, and an early draft of this record framed
them as if they were. **kaed's primary purpose is remote editing** — replacing
rclone-mount and base64-over-ssh for agents working the Linux hosts from
elsewhere — and it is doing that daily on two hosts. The doc-heavy mix describes
**what remote sessions are** (planning and design, driven from cleo) rather than
a ceiling on the tool: code sessions run locally on the host where the code is,
where the built-in editors are the correct choice.

The question that survives is narrower and better: *was there remote work that
wanted to edit code and didn't?* kaed has no `insert`/`delete`/`rename`, no
tree-sitter `outline`/`node_replace` and no `apply_patch` — all on the roadmap.
If remote code editing is being routed to ssh because anchor/range edits are too
blunt for refactoring, that is a capability gap that should reorder those items.
The journal cannot tell the two apart (G3).

Reliability looks good: 2 real failures in 34 organic transactions, both
recovered inside 30 seconds off the error payload, one of them self-documenting
(txn #18's intent records that it was re-applied after the conflict). And
`intent` is populated on **41 of 41** transactions with a median of ~140
characters — R6's promise being kept with nothing enforcing it.

## Three things that were not expected

1. **The `feedback` table already exists.** `journal.rs:65` creates it; nothing
   in `src/` writes to it and no MCP tool is exposed. #1046's storage half is
   done, so that slice re-sizes downward.
2. **Reads are not journaled at all.** #1044 asked for read shapes and
   "`denied` then what did the agent do next" — for reads, both are
   structurally unanswerable. That blind spot currently sits over the question
   the program most wants answered: whether refusals push agents to ssh. It is
   an accident of the design, not a decision, and #1049 should make it one.
3. **The corpus is tiny — 41 transactions.** Found during recon, before the
   analysis, and it changed the deliverable rather than cancelling it: at this
   n every row can be read by hand, so the report enumerates rather than
   samples, and its value is as a fixed comparison point. Ken's framing ("an
   asset to compare against when we do another one in a couple of months") is
   what makes small-n fine.

## A number that must not be quoted out of context

kaed accounts for ~7% of file-changes across kai's active repos since
2026-08-02, and five repos saw work with zero kaed involvement. **That is not a
bypass rate.** Ken confirmed those were sessions run locally on the machine,
where the built-in editing tools are correct and kaed was never the intended
path. The right denominator — edits attempted by *remote* sessions — is not
recoverable from the journal (G3 in the report), and closing that gap needs
client-side attribution, not more rows.

The report says this in the same breath as the number, deliberately. A future
reader finding "7%" alone would draw the wrong conclusion, which is the same
failure mode #930 was filed about.

## Two tooling traps, both now documented

- **No `sqlite3` CLI on kai or kubs0.** The "#910 makes it one query" story
  assumes a query path that is not installed; `python3` is it.
- **kai's `journal.db` is 4 KB and its WAL is 1.7 MB.** Every row lives in the
  WAL. Opening with `immutable=1` reports an **empty database** rather than an
  error — so the natural defensive read gives you "kaed has never been used".
  `mode=ro` reads through the WAL correctly. Called out in the script header
  because it will otherwise be rediscovered the hard way.

## Effect on the program

| slice | change |
|---|---|
| #1046 feedback tool | Re-size down (storage exists); prompt selectively — `ambiguous_anchor` has never fired, so wiring a prompt to it is dead surface. Must cover reads, which the txn path cannot. |
| #1049 history tools | Build the axes in `journal-report.py`. Take an explicit decision on read logging. |
| #929 kubsdb | Strengthened, and in the direction #929 already leaned: agents edit docs and config, not data or code. "Config dirs yes, data dirs no" now has evidence behind it. |
| #1045 addressing | New wrinkle: kai's journal references a `home` root that 002 narrowed away. Root renames orphan history; host-qualification will do it again at scale. Decide migrate-vs-accept. |
| #1048 secrets | No signal. The only `denied` ever recorded was a deliberate test. No evidence yet of agents meeting secret files in real work — consistent with shipping redaction without `reveal`. |

## Exit criteria

- [x] Written report in-repo, with the sample-size limitation stated rather than
      buried
- [x] Queries committed and verified re-runnable on both hosts
- [x] Bypass cross-check performed, and its result correctly qualified
- [x] Findings routed to the slices that were waiting on them
