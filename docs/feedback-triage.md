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
| kai | 15 | 2026-09-11 |
| kubs0 | 0 (no rows) | 2026-09-11 |
| kubsdb | 0 (no rows) | 2026-09-11 |

`feedback` carries no `root`, so a report lands on whichever host served
the connection rather than the host it is about. Every report so far was
filed through the kai gateway, which is why kai holds all fifteen —
including the ones about kubsdb and kubs0. Two passes in, no report has
ever landed anywhere else; treat "read kai" as the pass in practice and
the other two hosts as a check that the assumption still holds.

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

That is a deliberate 009 D-5 choice, and whether it left a real gap was
tested by **korg #1233**, an experiment in prompting for the missing class
at end of session. **It did leave one, and the prompt reaches it** — see
the second pass below, where the category mix inverts. Read the four-bug
mix above as a fact about the invite's placement, which is exactly what it
turned out to be.

## 2026-09-11 — second pass (kai #6–#15)

Ten reports, spanning 2026-08-22 to 2026-09-11. Five work items filed,
bundled as proposal korg:2378; three needed none; two are praise.

| Row | Cat | Subject | Disposition |
|---|---|---|---|
| kai #6 | friction | An ops-shaped session wrote ~12 files across two hosts and never once considered kaed | **Already filed** — korg #1560 (kaed) + #1562 (agent-skills), program 1563; kaed half shipped in sprint 021 |
| kai #7 | friction | Partial root coverage on a symmetric two-host task selects against kaed harder than no coverage | **Already shipped** — sprint 021, PD-8 (`kubs0:scratch`) and PD-9 (systemd-user, out of scope), korg #1560 |
| kai #8 | praise | `roots` returning per-root `path` let a session audit its own coverage in one call | **No action** — affordance to protect |
| kai #9 | friction | `ambiguous_anchor` returns bare line numbers, and `read` has no occurrence picker | **Filed — korg #2373** |
| kai #10 | friction | No multi-file read, so ssh is cheaper for 3–6 small files — and loses the versions | **Filed — korg #2374** |
| kai #11 | praise | The `edit` diff plus per-file `new_version` replaced every verification read across seven edits | **No action** — affordance to protect |
| kai #12 | friction | `**/secrets` denies the store's plaintext metadata file, so bookkeeping edits leave the journal | **Filed — korg #2375** (change lands in k-homelab) |
| kai #13 | friction | No delete op: every create-then-remove lifecycle splits across two tools | **Filed — korg #2377** |
| kai #14 | friction | A root's deny policy is invisible, so an agent guessed `.git` was denied and used ssh | **Filed — korg #2376.** Guess was wrong — verified writable by `dry_run` during triage |
| kai #15 | praise | A `create`'s version stayed a valid edit base across a branch switch, squash merge and pull | **No action** — affordance to protect |

### What the second pass showed

**The category mix inverted, and that answers korg #1233.** Six `friction`
and three `praise` against a prior corpus of four `bug` and one `wish`.
Every report here is about a call that **succeeded** or was **never made**
— which is precisely the class §2 predicted existed and the in-band invite
could not reach.

**Three of the five filed items are near-misses: reports about calls that
were never made.** #10 priced kaed against ssh and chose ssh; #14 guessed a
path was denied and never asked; #13 never had the op to consider. There is
no error, no refusal and no journal row behind any of them. This is the
finding that rules *against* #1233 §6's favoured option — widening
`with_feedback_invite()` cannot reach a call that was never made, because
there is no response to attach an invite to. The prompt can, and did.

**Verify the guess, not just the complaint.** #14 reported that `.git`
*seemed* denied. It is not — neither the default deny list nor either
host's config mentions it, confirmed with a `dry_run` `create` at the exact
reported path. The report was still correct about the cost: the agent paid
it on a belief, and the belief was never tested because testing it cost a
round trip. A triage pass that had only checked "is this a real denial?"
would have closed it as invalid and missed the actual finding, which is
that kaed gives an agent no way to know.

**The praise rows earn their place by naming what they replaced.** #11 and
#15 both name a specific affordance and the call it removed — the `edit`
diff standing in for a verification read, and `version` being a content
address rather than a session handle (one survived a branch switch, a
squash merge and a pull, and still worked as an edit base). That is the
form §4 asked for, and it is worth protecting deliberately: both are
properties a future refactor could quietly break with every test still
green.

**One report was not prompt-driven.** #12 arrived alone, mid-session, at a
`denied` refusal — the in-band invite working exactly as designed. Rows
#6–#8 were filed at Ken's explicit request after he noticed the behaviour
himself, and are **not** a prompt result; the provenance note on korg #1233
explains why that distinction matters. Rows #9–#11 and #13–#15 arrived as
end-of-session batches in the prompt's shape.
