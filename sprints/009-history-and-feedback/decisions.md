# Sprint 009 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. The gate this sprint sits behind is 008's
> D-11 (no plaintext shadow); its corollary — "history tools must redact
> legacy plaintext blobs on read" — is discharged by D-3 below.

---

## D-1 — One merged stream, not three read tools

`journal` returns `txn`, `failure` and `feedback` records interleaved by
time, newest first, discriminated by `kind` and filterable to any subset.

The alternative was three tools (or one tool with three shapes), which is
how the tables are laid out and therefore the tempting mapping. It is the
wrong one. The proposal's stated payoff is that "what did agents complain
about, and what were they doing when they did" becomes *one query* — a
feedback record is only worth writing next to the friction it describes,
and splitting the read path puts the correlation back on the caller. The
same argument runs for failures: #1049 makes the point that "did my last
edit fail, and why" beats "what did I successfully do", and an agent
mid-task should not have to know which of two tools holds the answer.

`kind` defaults to all three. Feedback is rare enough (0 rows across two
hosts before this sprint) that including it costs nothing, and excluding
it by default would recreate the unread channel the roadmap warns about.

## D-2 — Reads stay unjournaled, and the gap is stated in every response

#1049 required this call explicitly rather than by inheritance: only write
transactions and failed write *attempts* reach the journal, because
`txn_failures` is written from the transaction path. A refused read, a
`too_large` truncation, a whole-file read after an agent gave up on
anchors — none of it leaves a row.

**Decision: accept the gap in 009, and make it visible.** Two reasons for
accepting it. Reads vastly outnumber writes, so a read log changes the
journal's growth profile — and #909 settled blob retention without a read
log in view, exactly as it settled it without secrets-aware editing in
view (which is what forced 008). Landing a storage-shape change in the
same sprint as the tools that read the store is the "leak and guard in one
quarter" pattern the roadmap already warns about, wearing a different hat.

Accepting it silently was the option ruled out. Every `journal` response
carries a `coverage` block whose first note says, in the response itself,
that reads are not recorded and an absence of read-side friction here is
not evidence. That converts a blind spot an agent would have to know about
into one the tool discloses — the `files_searched` rule (#1066) and the
`denied_hidden` rule (R7) applied to time instead of to files.

`coverage` also reports when failure records actually start. kai's
failure log begins 2026-08-02T18:02, when #910 shipped, and transactions
#1–#5 predate it; a window reaching back that far must not read as "no
failures happened". The block is computed from the data, never hardcoded.

## D-3 — Redaction is enforced at the materialisation boundary

One private function turns `(root, path, side)` into servable content, and
every history surface goes through it. Not a per-tool rule: #1049 says the
rule "has to be enforced at the store, not per-tool", and the tools are
new surfaces on the same data.

Three cases, in order:

1. Blob flagged `redacted` (008 and later) → served as the rendering it is.
2. Blob **not** flagged, path classified *today* → redacted on read. This
   is D-11's corollary and it is live rather than theoretical: `**/*.env`
   only became a classification rule in 008, so pre-008 journals hold
   `korg.env` plaintext under a path that is classified now.
3. Content that cannot be rendered (classified, not dotenv-shaped) →
   withheld with a marker. Never served raw, never silently empty.

The in-file `kaedignore` marker is checked on the **blob content** as well
as on the working-tree file. Checking only the working tree would leave a
file that opted out and was later deleted with a readable history; the
blob-side check closes that for the cost of one call. It is still not
airtight — a file whose marker was *added* after the journaled version was
written has unmarked history — and that is the honest limit of an in-band
signal, already written down as 008's D-4 tradeoff.

## D-4 — `revert` refuses classified files rather than restoring renderings

For a classified file the retained blob is a *redacted rendering*, so
kaed does not hold the bytes a revert would need. Restoring the rendering
would write `⟨kaed:KEY@digest⟩` into the file as a literal — destroying
the value while reporting success, which is the exact failure D-10's
vanish guard exists to prevent, arriving through a side door.

There is a tempting partial: replay the rendering as `env_set` ops, since
D-9's placeholder passthrough resolves a placeholder whose digest still
matches. It works for keys whose values did not change and fails loudly
for the rest — which is to say it works precisely where a revert was not
needed. Refuse, name the reason, and point at `diff` plus explicit env
ops.

Two more refusals, same principle — a `revert` that bypassed any of them
would be a hole in the contract it is supposed to run through:

- **Historical root** (R8's corollary, spelled out there): the row is
  true, the root no longer resolves, and reverting would require aliasing
  a removed name back into existence.
- **A transaction that created a file**: undoing a create is a delete, and
  the `delete` op is a later slice. Refused with the reason named, not
  quietly skipped.

## D-5 — The friction prompt fires on refusals and surprises, not on malformed input

`feedback` is invited from the error itself: a `feedback_invite` object in
`data`, costing no round-trip and no standing invitation to ignore. The
prompt set is argued from sprint 006's evidence rather than from the list
of error codes:

- **`denied`** — always. "Does a refusal push an agent to ssh?" is the
  question the whole program most wants answered, and this is the only
  moment kaed can ask it.
- **`too_large`** — always. A budget refusal is friction by definition.
- **`internal`** — always. Kaed's fault by construction.
- **`version_conflict` only when the delta was withheld.** #1046 asks for
  "version_conflict where the delta did not help". Kaed cannot know if it
  helped, but it does know when it had none to give (blob GC'd past
  retention). A conflict *with* a usable delta is the contract working;
  one without is the retention window failing an agent, and that is worth
  hearing about.

Excluded deliberately: `invalid_input` (high-volume, schema-shaped, and
the message already names the valid fields), `ambiguous_anchor` and
`anchor_not_found` (the data already carries the fix, and
`ambiguous_anchor` has never once fired in recorded history — a prompt
there is dead surface).

The channel is **not** only reachable from a prompt. #1046's second
comment is the reason: sprint 006's worst incident was a *successful*
call — a silently-empty `search` that produced a wrong, acted-on
conclusion — and an error-triggered channel is structurally blind to that
class. So `feedback` is callable unprompted, with no reference required.
No speculative "suspicious success" triggers: 007 already gave that
specific class a structural fix (`files_searched` + a `reason` code), and
guessing at more of them is how a channel becomes noise.

Free text is redacted through `secrets::redact_free_text` before storage.
The likeliest thing an agent pastes into a friction report is the error it
just got, and every other derived surface in kaed is already redacted.

## D-6 — `journal` takes an optional root; an empty result explains itself

Every other tool requires `root` because it addresses a path. `journal`
does not address anything — it reads a host-wide store — and requiring a
root would make the most useful question ("what just happened on this
host") unaskable, and history under a removed root name unreadable.

So `root` is a filter. A value that does not resolve is **not** an error:
history legitimately names roots that no longer exist. But an empty result
from a typo must not look like an empty result from a quiet week, so a
zero-row response carries a `reason` — `unknown_root_filter`,
`no_matching_records` — the same mechanism 007 gave `search` and `list`
after #1066 cost a wrong conclusion.
