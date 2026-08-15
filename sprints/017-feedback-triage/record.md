# Sprint 017 — closing the feedback loop kaed already had

*No korg proposal: this began as a question, not a plan. Branch
`017-feedback-triage`. 2026-08-13 → 2026-08-15.*

## Goal

Ken had noticed agents filing feedback through kaed and had no way to look
at it. That is the whole prompt. It turned into a sprint because the answer
was not "here is the query" — it was that **nobody had ever read the
table**, and five reports had been sitting in it for a month.

## What was actually there

Five reports on kai, none on kubs0 or kubsdb, spanning 2026-08-08 to
2026-08-12. Full triage in `docs/feedback-triage.md`; the short version:

| Row | Subject | Disposition |
|---|---|---|
| kai #1 | `search` dies on an unreadable directory (EACCES) inside a root | already shipped — 014, #1088 |
| kai #2 | single-peer root pattern proxied wholesale, peer's `hosts_unavailable` masquerades as the caller's | already shipped — 014 D-5, #1089 |
| kai #3 | `secret rotate` `also` target on the same host, different root → `unknown_root` | **filed — #1231** |
| kai #4 | secret lifecycle cannot reach root-owned, non-dotenv compose YAML | **split**: refusal half shipped in 014 (#1091); YAML op **filed — #1232** |
| kai #5 | proxied peer results lack `resultType`, every gateway call fails at a 2026-07-28 client | already shipped — 016 D-4 |

**Three of five were already fixed.** The loop was working; it just had no
reader. The two that fell through are the two whose fix did not belong to
the sprint that happened to be running when they were filed — which is a
smaller and more tractable gap than "five unread reports" suggests.

## Three findings the triage produced

**kai #3 is still live in `main` at 016**, four sprints after it was filed.
Confirmed by reading the code, not by trusting the report. `secret_rotate`
partitions `also` targets by comparing the whole root string against the
primary's (`src/server.rs:1343`), so a target on the *same host, different
root* is classed remote, and `write_value_to_peer` then asks
`fleet.routable("kubsdb")` on kubsdb itself, gets `None`, and reports the
root unknown while listing it as served. The axis is wrong: it partitions
by root, and what decides local-vs-remote is host. The fix is not one line
— `rotate_local` refuses cross-root targets and `txns.root` is single-root
by schema, so such a target cannot join the primary's atomic transaction.
It survived because the only test covering cross-root `also`
(`tests/gateway.rs:944`) uses a fixture where each host has exactly one
root, making the broken case inexpressible.

**`feedback` never proxies.** It takes no `root`, so a report lands on
whichever host served the connection rather than the host it is about.
cleo talks through the kai gateway, so all five reports — including the
three about kubsdb — are on kai. Convenient today, accidental, and not
something to build on: a client wired directly to a peer files there
instead. The skill checks all three hosts for that reason.

**The in-band invite fires only on errors.** `with_feedback_invite()` is
attached in exactly one place, `kaed_error_result` (`src/server.rs:1877`).
The four-bug/one-wish distribution is what that mechanism predicts, so the
absence of any `friction` or `praise` row is a fact about where the invite
is attached — not a verdict on how well the contract serves its users. See
D-4; it is an experiment (#1233), not a fix.

## What shipped

**`.claude/skills/triage-feedback/SKILL.md`** — the loop, end to end: read
every host, triage what is new, decide, file, update the ledger, report.
Its ordering is drawn from this pass rather than invented — *is it already
fixed?* comes first because that is the modal answer, and *verify against
current `main`* is second because kai #3 proved a report can be live at a
build far newer than the one it names. D-1 is its second section.

**`scripts/feedback-dump.py`** — prints the `feedback` table whole, with
`--since-id` for new-only passes. The companion to `journal-report.py`,
which counts this table and never prints a row — which is why "how do we
look at the feedback" had no answer (D-3).

**`docs/feedback-triage.md`** — the ledger: per-host high-water marks, one
row per report with its disposition, and what each pass showed (D-2).
Seeded with all five.

**`CLAUDE.md`** — a bullet, so a future session knows the loop exists
without rediscovering it.

## Follow-ups

- **#1231** (bug, S) — the `also` misrouting above.
- **#1232** (feature, M) — a value-substitution op for YAML/compose
  scalars. The motivating evidence is k-homelab #841, where rotating the
  kubsdb passwords by hand **leaked a live redis password into a
  transcript** because a `sed` mask encoded the expected shape rather than
  the actual one. A typed op addressing a YAML path is the class of fix
  that makes that unreachable — the same argument that produced `env_set`.
- **#1233** (research, S, *Awaiting Ken*) — the end-of-session prompt
  experiment for D-4. Carries the paste-ready prompt, the anti-sycophancy
  design notes, and the success/failure signatures to judge it by.

## Not deployed, deliberately

Nothing in `src/` changed — a skill, a script and two docs. `just check`
passes; there is no build to publish and no host to upgrade. The next
deploy is whichever sprint next touches the server.
