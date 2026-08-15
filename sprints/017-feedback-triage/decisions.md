# Sprint 017 — decisions

## D-1. The triaging agent decides what gets filed. Ken is not the gate.

The skill says so in its second section, before any procedure, and says it
in the imperative: *do not present a list and ask which ones to file.*

kaed's users are agents. `feedback` is the only channel where its users say
what it got wrong, and a loop that routes every one of those reports
through a human for a file/don't-file ruling re-introduces exactly the
bottleneck the tool exists to remove — it turns Ken into a queue and makes
the loop's throughput a function of his attention.

Ken called this explicitly when asking for the skill, and it is written
into the skill rather than left as a convention, because a convention that
only lives in a conversation is one the next session will not have.

Two consequences pull in opposite directions, deliberately, and both are in
the skill:

- **A report is not a WI.** Most feedback is already fixed, duplicated, or
  one agent's confusion. Filing everything is how a triage loop becomes
  noise and stops being read. So the skill requires reporting what was
  *declined* and why — that is the half Ken actually reviews, and it is the
  check on the authority D-1 grants.
- **A sample of one still counts.** Reads are not journaled (009 D-2), so
  read-path friction leaves no trace anywhere but here. "Only one agent hit
  this" is never grounds for dismissal; the population it is drawn from is
  invisible by construction.

The narrow escalations are named so they cannot be inferred more broadly:
a live security exposure, or a question about the homelab rather than about
kaed. Those go to `set_awaiting`, not into a sprint.

## D-2. The ledger is `docs/feedback-triage.md` — dispositions, never the prose.

Three candidate homes: beside the skill (`.claude/skills/…/triaged.md`),
under `sprints/`, or in `docs/`. `docs/` wins on precedent and on audience.

`docs/agent-usage-report-2026-08-06.md` is already this genre — a dated
report derived from journal data — so the directory has the shape. And the
file is legible to a stranger in a way most homelab records are not: kaed's
public thesis is that an agent-authored feedback loop is worth building,
and the ledger is the evidence of that loop running or not running. It
belongs where someone evaluating the project will find it.

Against `sprints/`: the ledger is a **running index with mutable state**
(the per-host high-water marks), and a sprint record is a narrative frozen
at its sprint. Against the skill directory: a skill folder is reference
material for the skill, not a place to accumulate project state.

**It records dispositions and one-line subjects, never verbatim feedback
text.** The journal on each host is the source of truth for the prose; the
ledger is the index into it. That keeps the file small enough to stay
readable as it grows, and it keeps host layout detail — which is what
friction reports are full of — from accreting in a public repo one report
at a time. Note this is a *tendency*, not a rule the repo enforces: 013 and
014's sprint records already discuss `/datastore` paths deliberately. The
point is that an append-only log should not do it by accident.

## D-3. A second script, not a flag on `journal-report.py`.

`scripts/feedback-dump.py` is separate because it is a different reading
mode, not a different filter. `journal-report.py` aggregates — counters,
medians, distributions over hundreds of rows. Feedback is prose written by
an agent at the moment of friction; there is nothing to aggregate and the
only useful operation is printing it whole. Bolting that onto a report
whose every other section is a summary statistic would confuse both.

`--since-id N` is what makes the skill a *new-only* pass rather than a
re-litigation of the whole table on every run, paired with the high-water
marks in the ledger.

Both scripts carry the same WAL trap in their docstrings, because it is
fatal in the same way in both and the second reader will not have read the
first: kaed runs SQLite in WAL mode and checkpoints rarely, so
`immutable=1` reports an **empty table** rather than an error — which reads
exactly like "no agent has ever filed feedback".

The `ssh` invocation is also written down in both places after getting it
wrong once: arguments go *inside* the quoted remote command, because
`ssh h python3 - < script -- --since-id 4` sends the `--` to argparse,
which then reads `--since-id` as the positional db path.

## D-4. The error-only invite is recorded as a finding, not fixed here.

`with_feedback_invite()` is attached in exactly one place —
`kaed_error_result` (`src/server.rs:1877`) — so the friction prompt fires
**only on errors**. The first five reports are four `bug` and one `wish`,
every one filed at a hard failure: precisely the distribution that
mechanism predicts. Friction that costs an agent a detour without ever
failing (a search re-run three times, a capability routed around via ssh)
cannot reach the table by the mechanism that fills it.

Not fixed in this sprint, for two reasons. It is a deliberate 009 D-5
choice — "the report worth having comes from the session that hit a wall" —
so reversing it needs evidence rather than an argument. And the fix has a
real design question inside it: attaching the invite to *every* successful
result would make it ambient and it would stop being read, which is the
failure 009 D-5 was avoiding in the first place. The version worth
designing attaches on results carrying a cost signal the server can already
see — `truncated`, a zero-result search, a `fanout` error entry.

So it is an experiment first: **#1233**, which Ken runs by pasting an
end-of-session prompt for a while. If the missing class of report does not
materialise, the answer is that 009 D-5 was right and there is no gap.
