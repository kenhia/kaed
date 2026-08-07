# Sprint 009 — history tools and the friction-triggered feedback channel

korg:1058 · work items #1049 (history tools, M) and #1046 (`feedback`,
re-shaped, S) · branch `009-history-and-feedback`

## Goal

Make kaed's own history legible **to the agents it is for**, not only to
Ken over ssh. R6 has promised a durable attributed journal since sprint
001; until now redeeming that promise meant opening SQLite on the host.
This sprint adds `journal`, `diff` and `revert` — plus a read path for
`txn_failures`, which is the higher-value half — and turns the `feedback`
table (a schema stub since 001, zero rows on both hosts) into a real
channel that fires at the moment of friction.

The gate was real and is satisfied: 008 shipped journal-blob redaction, so
these tools serve a redacted store rather than turning a *retained*
plaintext blob into a *served* one. There is a test that says so.

## What shipped

Four tools, one new module (`src/history.rs`), and a friction prompt on
the errors that are plausibly kaed's fault.

### `journal`

One merged, time-ordered stream of three record kinds — `txn`, `failure`,
`feedback` — newest first, filterable by `kind`, `root`, `path`, `author`
and `since`. The query axes are the ones `scripts/journal-report.py`
actually needed in sprint 006's evidence pass, not guesses.

Merging the three was the shape decision (D-1). "What did agents complain
about, and what were they doing when they did" is the proposal's stated
point, and it is one call rather than three.

Every response carries a **`coverage`** block, and that is the part worth
defending. It states what this history cannot see — reads are not
journaled at all (D-2) — and when failure records actually begin, so a
window that predates #910 cannot be read as "no failures happened". Same
honesty rule as `files_searched` (#1066) and `denied_hidden` (R7): a
partial answer that looks whole is the failure mode this project keeps
paying for.

Rows whose `root` no longer resolves on this host are labelled
**`historical`** with a structured reason rather than rewritten or aliased
back into existence — R8's corollary, and the case is live: kai's journal
holds five transactions naming a `home` root that sprint 002 removed.

### `diff`

`{root, path, from, to?}` where each side is a `version`, a `txn_id` (the
state that transaction *produced*), or `"current"`. Content comes from the
journal's blob store, and this is the leak surface, so redaction is
enforced **at the materialisation boundary** rather than per-tool (D-3):

- a blob flagged `redacted` is served as the rendering it is;
- a blob **not** flagged, whose path is classified *today*, is redacted on
  read — that is D-11's corollary made real, and it is not hypothetical:
  pre-008 journals hold `korg.env` plaintext because the old deny list
  never matched `**/*.env`;
- content that cannot be rendered is withheld outright, never served raw.

### `revert`

A revert **is** a new journaled transaction (never history rewriting): it
runs through `txn::apply` with `base` set to the version that transaction
produced, so a file that moved since gets a `version_conflict` with a
delta, exactly like any other edit, and the revert is itself revertible.

It refuses, with reasons, in three cases — each one a place where a force
would have been a hole in the contract: a historical root (R8), a
classified file (the retained blob is a *rendering*; restoring it would
write placeholders as literals — D-4), and a transaction that created a
file (undoing a create needs the `delete` op, which is a later slice).

### `feedback`

One required field: `summary`. `category` defaults to `friction`.
Free text is run through the same redactor as every other derived surface
before it is stored, because the most likely thing an agent pastes into a
friction report is the error it just got.

And the trigger, which is the actual re-shaping #1046 asked for: errors
that are plausibly kaed's fault carry a `feedback_invite` in their `data`
— no extra round-trip, no standing invitation to ignore. The prompt set is
narrow and argued from the evidence rather than from the list of error
codes (D-5).

## The finding: the gate test earned its keep

The proposal demanded a test asserting a known secret never appears in
`journal`/`diff` output *before either tool is exposed*. It failed on the
first run, and not on the case anyone was watching.

008's redaction model reasoned about file **content**. `intent` is
agent-supplied free text — and until this sprint it was write-only, so
plaintext in it went no further than a 0600 SQLite file. `journal` serves
it. That is exactly the "a retained blob becomes a served blob" transition
D-11's gate is about, arriving through a field nobody had classified as
content. An agent writing `intent: "rotating KLAMS_TOKEN to <the token>"`
walked straight past every guard 008 built.

Closed at both ends: redacted at the store, so the blast radius stays
where 008 left it, and again on read, so rows kai and kubs0 already wrote
under a pre-009 kaed are covered too. Error messages ride the same path.

The general form is worth carrying forward: **a redaction rule scoped to
"content" does not automatically cover the metadata around it**, and the
moment a write-only field becomes readable is when that stops being
academic. Had the gate been "reason carefully about redaction" rather than
"write the test that says so", this would have shipped.

## Decisions

In [decisions.md](decisions.md): D-1 (one merged stream), D-2 (reads stay
unjournaled — the explicit call #1049 required, made visible rather than
inherited), D-3 (redaction at the materialisation boundary), D-4 (revert
refuses classified files), D-5 (the friction prompt set), D-6 (`journal`
takes an *optional* root).

Contract delta in [contract-notes.md](contract-notes.md).

## Verification

`just check` green: 192 unit tests, 20 integration. Two of the integration
tests drive the new surface over real HTTP end to end — `edit → journal →
diff → revert` in one session, and a refusal that carries a
`feedback_invite` whose report then reads back through `journal`.

## Follow-ups

- **A read log** — the deliberate gap D-2 leaves open, and the one that
  sits over the program's central question (do refusals push agents to
  ssh?). Needs #909's retention decision reopened first, because reads
  vastly outnumber writes.
- **The `delete` op**, without which `revert` cannot undo a create.
- **Act on the channel** — the roadmap's second half. An unread feedback
  channel is worse than none.
