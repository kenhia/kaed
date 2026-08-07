# Sprint 009 — contract delta

The changes are already applied to
[`../planning/mcp-contract.md`](../planning/mcp-contract.md); this records
why. The History and Meta sections were draft-v0 sketches written before
anything was built, so most of this is a sketch meeting reality.

---

## `journal` — root became optional, and the shape became one stream

**Draft:** `{root, path?, author?, since?, max?}` → a list of transactions.

**Shipped:** `root` is optional and is a *filter*; entries are `txn`,
`failure` and `feedback` merged newest-first, selectable by `kind`.

`root` had to move because the draft was written when every tool
addressed a path. `journal` addresses nothing — it reads a host-wide store
— and requiring a root would have made "what just happened on this host"
unaskable and history under a removed root name unreadable. The removed
root name is not hypothetical: kai's journal holds five transactions
naming a `home` root sprint 002 deleted.

The merge is D-1. The draft's shape follows the *tables*, which is the
tempting mapping and the wrong one: a feedback record only earns its
keep next to the friction it describes, and `txn_failures` is the half an
agent mid-task actually wants.

## `journal` — `coverage`, which the draft had no notion of

New, and the part most worth defending. Every response states what the
history cannot see (reads are never journaled — D-2) and when failure
records actually begin. Neither is a detail: a window reaching back past
the failure log looks *clean* rather than *silent*, and that is the same
class of wrong conclusion `denied_hidden` (R7), explicit truncation (R3)
and `files_searched` (#1066) each exist to prevent. This project has paid
for that lesson three times; the fourth surface got it up front.

`records_scanned` and `reason` are the same rule at the query level,
lifted directly from what 007 gave `search` and `list`.

## `diff` — the selector grammar became explicit, and so did retention

The draft said `from: <version|txn_id>` without saying how they are told
apart or what a txn id *means*. Both were decided: a version is 16 hex
chars (R1 already fixed that), which disambiguates with no type tag; and
a txn id names the state the transaction **produced**, with the contract
now pointing at `journal`'s `old_version`/`new_version` for "what did this
one transaction do".

The draft's "works for any version the journal still knows" quietly
assumed the interesting case away. #909 retains metadata forever and
content for a window, so a version can be *named* and not *renderable*.
That is now a structured `not_found` carrying the window — an agent that
asks for a two-month-old diff learns why it cannot have one instead of
getting an empty one.

## `diff`/`revert` — redaction moved into the contract as a store-level rule

The draft predates the secrets model entirely. #1049's own wording is the
rule now stated in the contract: enforcement is **at the store, not
per-tool**, because the tools are new surfaces on the same data. Written
down alongside it: the legacy-plaintext case (a blob predating a
classification rule), and the marker check on journalled content.

## `revert` — the refusals are contract, not implementation detail

The draft mentioned one failure mode (`version_conflict` on overlap). Four
more are real, and each is a place where a force would have been a hole:
a historical root, a classified file whose retained pre-image is a
*rendering*, a create with no `delete` op to undo it, and an expired blob.
They are in the contract because "revert refuses and says why" is a
promise an agent plans around; "revert sometimes doesn't work" is not.

`intent?` was added — a revert is a write, and R6 wants a reason on
writes. The automatic `revert of txn N` is prepended either way.

## `feedback` — `category` stopped being required

Draft: `{category, summary, detail?, context?}`. Shipped: `summary` alone,
`category` defaults to `friction`. #1046's argument, taken literally —
anything that costs an agent a thinking step loses to finishing the task,
and the report worth having comes from the session with the least
patience for ceremony.

Two additions the draft could not have anticipated, both from sprint 006's
evidence:

- `data.feedback_invite` on errors (R4). A standing invitation is answered
  by the agents that were already having a good time.
- Redaction of the report text (R9). Nothing in the draft's model of
  secrets covered *free text*, which is exactly the gap that produced the
  finding below.

## R6 grew a "what this does not cover" clause

Reads are not journaled, and R6 now says so. The alternative was leaving
it as an accident of where `txn_failures` is written from, which is how
the gap survived three sprints unstated.

## The finding: `intent` was a leak, and the gate test caught it

Not a contract change so much as the reason one clause exists. 008's
redaction model reasoned entirely about file **content**. `intent` is
agent-supplied free text, and before 009 it was write-only — plaintext in
it went no further than a 0600 SQLite file. `journal` serves it, which is
precisely the "retained becomes served" transition 008's D-11 gate names,
arriving through a field nobody had classified as content.

Found by the sprint's own gate test — the one the proposal demanded before
either tool was exposed — rather than by reasoning. Closed at both ends:
redacted at the store (so the blast radius stays where 008 left it) and
again on read (so rows already written by pre-009 kaed on kai and kubs0
are covered). Error messages ride the same path.

The general form is worth keeping: **a redaction rule scoped to "content"
does not automatically cover the metadata around it**, and the moment a
write-only field becomes readable is when that stops being academic.
