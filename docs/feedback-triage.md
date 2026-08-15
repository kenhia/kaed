# Feedback triage ledger

kaed's users are agents, so kaed's bug reports are written by agents. The
`feedback` tool (sprint 009) lets one file a friction report at the moment
it hits friction, into the same journal as the edits it is about.

This file is the index of what has been done with those reports: every row
triaged, what was decided, and how far each host has been read. It is not a
copy of the reports — the journal on each host holds the prose, and this
records only the disposition. The process is the `triage-feedback` skill.

## High-water marks

The last `feedback.id` triaged on each host. A triage pass reads only rows
above these.

| Host | Triaged through | As of |
|---|---|---|
| kai | 5 | 2026-08-13 |
| kubs0 | 0 (no rows) | 2026-08-13 |
| kubsdb | 0 (no rows) | 2026-08-13 |

`feedback` carries no `root`, so a report lands on whichever host served
the connection rather than the host it is about. Every report so far was
filed from cleo through the kai gateway, which is why kai holds all five —
including the three about kubsdb.

## 2026-08-13 — first pass (kai #1–#5)

Five reports, spanning 2026-08-08 to 2026-08-12. Two work items filed;
three needed none.

| Row | Cat | Subject | Disposition |
|---|---|---|---|
| kai #1 | bug | `search` dies on an unreadable directory (`lost+found`, EACCES) inside a root | **Already shipped** — sprint 014, korg #1088 |
| kai #2 | bug | A single-peer root pattern (`kubsdb:*`) is proxied wholesale, so the peer's `hosts_unavailable` masquerades as the caller's | **Already shipped** — sprint 014 D-5, korg #1089 |
| kai #3 | bug | `secret rotate`: an `also` target on the same host but a different root routes to the peer path and fails `unknown_root` | **Filed — korg #1231** |
| kai #4 | wish | The secret lifecycle cannot reach root-owned, non-dotenv compose YAML | **Split.** Clearer refusal for the ownership boundary shipped in sprint 014 (korg #1091); the YAML value-substitution op **filed — korg #1232** |
| kai #5 | bug | Proxied peer results lack `resultType`, so every gateway call fails at a 2026-07-28 client | **Already shipped** — sprint 016 D-4, korg #1212/#1214/#1221 |

### What the first pass showed

**The loop already works — it just had no reader.** Three of five reports
were fixed within days, because the agent that filed them (or the next one)
carried them into a sprint directly. The two that fell through are the two
whose fix did not belong to the sprint that was running at the time. That
is the gap this ledger exists to close, and it is a smaller gap than the
raw count suggests.

**A report can be live at a much newer build than it names.** kai #3 was
filed during sprint 013 and is still reproducible in `main` at 016. Triage
verifies against current code; the `context` field records where the
reporting agent was standing, not the state of the world.

**Agents report the general form, not just their instance.** kai #1 asked
for the specific deny-list entry *and* named the underlying class —
"an unreadable directory anywhere under a root has this effect". Sprint 014
shipped the general fix (`unreadable_hidden`) and dropped the specific
workaround. Reports written this way are worth more than their category
suggests; the summary line usually undersells the detail field.

**Nothing has ever been filed as `friction` or `praise`.** Four `bug`, one
`wish`, every one filed at a hard failure. That is not a coincidence:
`with_feedback_invite()` is attached in exactly one place,
`kaed_error_result` (`src/server.rs:1877`), so **the in-band invite fires
only on errors**. Friction that costs an agent a detour without ever
failing — a search re-run three times, a capability routed around via ssh —
cannot reach this table by the mechanism that fills it.

That is a deliberate 009 D-5 choice, and whether it leaves a real gap is
being tested: **korg #1233** is an experiment in prompting for the missing
class at end of session. Until it reports, read the category mix here as a
fact about the invite's placement, not about how well the contract serves
its users.
