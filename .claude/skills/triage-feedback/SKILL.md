---
name: triage-feedback
description: Read the friction reports agents have filed through kaed's `feedback` tool on every fleet host, triage the ones not yet triaged, decide which are actionable, and file korg work items for those. Use when asked to go through / review / consume kaed feedback, or periodically after a stretch of real use.
---

# Triage the feedback agents filed

kaed is by agents, for agents. `feedback` is the only channel where the
tool's users say what the tool got wrong, and its users are agents. So the
whole loop closes inside this skill: **you read it, you judge it, you file
it.** Ken is not the gate, and asking him to be one defeats the point.

## The call is yours

**Do not present a list and ask which ones to file.** Decide, file, then
report what you filed and why. If you were wrong, a WI is cheap to close —
much cheaper than a round trip that turns Ken into a queue.

Two things follow from that, and they pull in opposite directions on
purpose:

- **A report is not a WI.** Most feedback is already fixed, is a duplicate,
  or is one agent's confusion rather than kaed's defect. Filing everything
  is how a triage loop becomes noise and stops being read. Say in your
  report what you *declined* and why — that is the half Ken reviews.
- **A sample of one still counts.** Reads are not journaled (009 D-2), so
  friction on the read path leaves no trace anywhere except here. Never
  dismiss a report for being unreplicated; the population it is sampled
  from is invisible by construction.

The genuinely escalate-to-Ken cases are narrow: feedback that reveals a
live security exposure, or that asks for a decision about the homelab
rather than about kaed. Those go to him via `set_awaiting`, not into a
sprint.

## Where the feedback is

Per host, in the `feedback` table of `~/.local/share/kaed/journal.db`.

**`feedback` takes no `root`, so it never proxies** — a report lands on
whichever host served the connection, no matter which host it is *about*.
cleo talks through the kai gateway, so as of 2026-08-13 every report lives
on kai, including the ones about kubsdb. Do not turn that into an
assumption: **check every host**, because a client wired directly to a peer
files there instead.

There is no fleet-wide view and the gateway does not merge one. `journal`
with `kind: ["feedback"]` is the in-contract route if you have kaed MCP
tools in session, but it is still per-host, and its `root` filter is
deliberately ignored for feedback (a report is about the contract, not
about a file).

```sh
python3 scripts/feedback-dump.py --since-id <kai's high-water>   # on kai
ssh kubs0  'python3 - --since-id <n>' < scripts/feedback-dump.py
ssh kubsdb 'python3 - --since-id <n>' < scripts/feedback-dump.py
```

Arguments go **inside** the quoted remote command. `ssh h python3 - <
script -- --since-id 4` fails: the `--` reaches argparse, which reads
`--since-id` as the positional db path.

Read the script's docstring before reaching for `sqlite3` yourself:
**`immutable=1` silently reports an empty table** because the rows live in
the WAL, and that failure mode reads exactly like "no agent has ever filed
anything".

The high-water marks are in [`docs/feedback-triage.md`](../../../docs/feedback-triage.md).
Passing them is what makes this skill a *new-only* pass instead of a
re-litigation of everything.

## Triage each new row

### 1. Is it already fixed? Ask this first, not last

The single most common disposition, by a distance. Of the first five rows
ever filed, **three were already shipped** by a later sprint — feedback is
written mid-friction against the build in the agent's hands, and sprints
land fast.

Check `sprints/*/decisions.md` and the bullet list in `CLAUDE.md`, which
tracks what each sprint closed. A report whose fix shipped needs no WI —
but note it in the ledger, because "reported, then fixed" is the loop
working and it is worth being able to see that.

### 2. Verify against current `main`. Do not trust the report's build

Read the code. A report can be stale, and it can also be **live at a build
far newer than the one it was filed against** — that is exactly what
happened with feedback kai #3, still broken at 016 after being filed during
013. The report's `context` field names the build; treat it as where the
agent was standing, not as the current state of the world.

If you confirm it live, you now have the code locations, which is most of
the WI already written.

### 3. Does korg already have it?

`list_work_items` with `project: "kaed"`, `wi_status: "all"` — closed items
matter here, since a closed WI is how you learn the fix shipped. Match on
substance, not title wording.

### 4. Decide

Category is the reporter's framing, not your conclusion — re-classify
freely.

| Category | What it usually wants |
|---|---|
| `bug` | A WI if it reproduces and no WI covers it. Cite the code path. |
| `friction` | The contract got in the way. Often a `tweak` on a tool description or an error message rather than a feature — those are the cheapest high-value items kaed has shipped. |
| `wish` | A WI only if you can name what it unlocks. A wish with no bounded scope becomes a WI nobody picks up; either scope it or leave it in the ledger with a note. |
| `praise` | Never a WI. But read it — it says which affordance worked, which is evidence for where to spend next. |

A report can split. Feedback kai #4 made two asks; one had already shipped
and the other became #1232. File the live half, and say in the WI that the
other half shipped, so nobody re-derives it.

### 5. File, with provenance

Open the WI with the source: host, row id, timestamp, category, author.
Someone reading it in three months must be able to get back to the original
prose without grepping three journals.

> From agent feedback filed on kai, row **#3** (2026-08-08T21:24:52Z,
> category `bug`, author `claude`). Confirmed still live in `main` at 016.

Then write it the way the repo writes WIs: what happens, why (with file and
line), and **where the obvious fix is not sufficient**. If your reading
found that a one-line fix breaks an invariant — as #1231's did, against the
single-root `txns` schema — that finding is the most valuable thing in the
item. Do not flatten it into "fix the comparison".

Also record why it survived: a missing test, a fixture that cannot express
the case. That is what stops it recurring.

### 6. Update the ledger, then report

Append to [`docs/feedback-triage.md`](../../../docs/feedback-triage.md):
every row triaged, its disposition, and the new high-water mark per host.
**Dispositions and one-line subjects, not the verbatim prose** — the
journal is the source of truth for the text, the ledger is the index. It
also keeps host layout detail from accreting in a public repo.

Then a short report to Ken: what you filed, what you declined and why, and
anything that looks like a pattern across reports rather than a single
defect. The pattern is the part he cannot get from korg.

## What this skill does not do

- **Failed transactions.** `txn_failures` is a different stream with a
  different bias — most rows are deliberate verification probes, so it
  rewards aggregate reading, not per-row triage.
  `scripts/journal-report.py` covers it.
- **Deleting or editing feedback.** The journal is append-only and the
  ledger is the working surface. Never write to a journal.db.
- **Sprint planning.** Filing a WI is where this stops. Sequencing is
  `refill-queue` and `propose_sprint`.
