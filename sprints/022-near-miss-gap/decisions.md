# Sprint 022 decisions

## D-1 — `ambiguous_anchor` carries each match's line content, capped and truncated explicitly

`AmbiguousAnchorData` was `{path, occurrences: Vec<usize>}`. It is now
`{path, occurrences: [{line, text}], total, truncated, hint}`.

The bare line numbers were not enough to pick with, so the cheapest-looking
next move was "read a range around one of them" — a guess, and the reported
instance guessed wrong and needed a third read.

`invites_feedback()` **excluded** this error already, justified by a comment
saying its `data` "already carries the fix". That was false. The rule the
episode leaves behind, written into the comment: *an exclusion justified by
"the data carries the fix" is a claim about a payload, and it has to be
re-checked when that payload changes.* The exclusion stays, and is now true.

Caps: 20 occurrences, each line trimmed and clipped to 120 **characters**
(never bytes — a multi-byte boundary must not split). Past the cap the list
truncates with `truncated: true` and `total`, and the `hint` changes to name
`search`, which answers in one call where a guessed range costs two. Truncation
is explicit, never silent — the core invariant.

## D-2 — `read`'s `window` takes `occurrence`, the same name and semantics as `anchor_replace`

`EditOp::AnchorReplace` has carried `occurrence: Option<usize>` all along;
`WindowParam` had no equivalent. So an agent hitting an ambiguous anchor on the
*write* path could say "the third one", and on the *read* path could not
express that at all. The asymmetry was invisible from the tool surface and had
no stated reason.

Same field name, same 1-based semantics, same `invalid_input` on out-of-range,
so the two paths read alike. `occurrence` with `line` rather than `anchor` is
`invalid_input`: it picks among anchor matches and means nothing otherwise.

## D-3 — `roots` publishes each root's deny and classify patterns

`RootInfo` gains `policy: {deny, deny_prefixes, classify, also_enforced}`.

The finding was an agent that *guessed* `.git` was denied and took the
base64-over-ssh fallback to write a cross-tool lock file. The guess was wrong —
verified twice, in triage and again at this sprint's premise check with a
`dry_run` create that returned a clean diff. The path was writable the whole
time.

The incentive is what makes this structural rather than a one-off: assuming
denial costs an agent nothing it can perceive, while trying and being refused
costs a visible round trip. So the incentive runs *toward* never asking, and
every wrong guess is a silent, permanent loss of coverage producing no error,
no refusal and nothing in any journal.

**Disclosing the patterns is safe by construction.** Deny matching is lexical
and absolute — it never touches the filesystem and answers identically for
paths that exist and paths that do not (001). Publishing therefore discloses
*policy*, not filesystem contents; a denied path was deliberately built not to
be an existence oracle, and this is the payoff.

The counter-argument, weighed and recorded as R10's author asked: publishing
the deny list tells a client exactly what is being kept from it. Against kaed's
actual threat model — authenticated agents, per-author tokens, a journal naming
every edit — that is the right trade. It is now a stated decision rather than
an omission.

Classify globs are published too, though the item called them only "arguably"
worth it: they do not refuse, they redact, and an agent that knows a path comes
back redacted does not route around kaed to avoid a refusal that was never
going to happen.

`also_enforced` names the three layers this list *cannot* enumerate up front —
`.kaedignore` files, the in-file marker, and unix ownership — so the published
policy is honest about being partial rather than read as exhaustive. For those,
and for writability, `dry_run` remains the definitive answer.

`deny_prefixes` is filtered to the built-ins that actually fall inside the root
being described, so it is usually empty and never noise.

Peer roots pass through verbatim (010 D-3), so a peer's patterns ride along
with no gateway work.

## D-4 — multi-file read is `paths` on `read`, shared budget, partial success

Option (a) from the item, over (b) `list` gaining inline contents.

**Why (a):** `list` enumerates rather than addresses. Serving content from it
would make it a content-*opening* tool, which means it would need the in-file
`kaedignore` marker check it deliberately cannot do today — a real change to
the 008 policy surface, not an add-on. `read` already opens content and already
carries every policy layer. Smaller blast radius, and the item expected this to
win.

Three sub-decisions:

- **One shared byte budget, spent in request order** — not a per-file cap. The
  point of the call is one *bounded* round trip, and a per-file cap makes the
  total unbounded in the number of paths. Running out is stated
  (`budget_exhausted`) and the paths it stopped at are still listed, each with
  its own error. Never a silent omission.
- **Partial success, not all-or-nothing.** A read has none of the atomicity
  argument that makes all-or-nothing right for `edit`. A denied or missing path
  returns its own structured error *in place*, keeping its position in the
  list, and never sinks the rest.
- **Whole files only.** `range` and `window` address one file by nature; a
  window meaning something different in each of six files would be a worse tool
  than six calls. Passing either with `paths` is `invalid_input`.

Cap of 32 paths, refused rather than silently trimmed, with the refusal naming
`list`/`search` as the tools for surveying more.

Every entry carries its own `version`, pinned by a test that feeds one straight
back as an edit base. That invariant is the whole reason the ssh route lost:
the reporting agent took ssh for the round trip, then had to come back through
kaed for the versions anyway.

## D-5 — a delete is recoverable exactly when kaed could have *edited* the file

The bound is the existing one, not a new number. kaed already journals
`blob_old`/`blob_new` for every edited file under `blob_retention_days`, with a
`redacted` flag. A delete blob is therefore not a new exposure class — same
blob, same retention, same redaction machinery — and #909's retention decision
stays closed. 009 D-2 (no read journal) and 008 D-11 (no plaintext shadow) both
stay intact.

Two things fall out for free, exactly as the item predicted: **binaries and
oversized files need no special case**, because `load_text` refuses them by the
same rule that governs everything else; and **no new config knob**, so there is
no second number to keep consistent with the first.

**A classified file gets no blob, which is deliberately stricter than `edit`.**
`edit` journals a redacted rendering. A delete must not, and the reason is
recorded here so nobody later "harmonises" the two: *you cannot restore from a
redacted blob.* Restoring one writes `⟨kaed:KEY@digest⟩` as literal text — a
file that looks real and is corrupt, with the values gone either way. It buys
audit value at the price of **negative** recovery value. Hard delete is both
simpler and honest.

The framing that settles the data-loss objection is Ken's, and it is worth
keeping: **the route an agent reaches for otherwise is `rm`, unrecoverable 100%
of the time.** Partial recoverability strictly dominates. kaed does not have to
be perfect here, only better than the route it is competing with — and it is
competing whether or not it has the op.

## D-6 — the response says `recoverable` per file, at the time of the act

`FileChange` gains `deleted`, `recoverable` and `unrecoverable_because`.
`new_version` becomes optional, because a deleted path has no content to
address.

An agent must know whether an action was reversible *when it takes it*, not on
some later attempt to revert it.

In the journal the row records `new_version = "absent"` — the same word a
`version_conflict` already uses for a file that is no longer there. One
vocabulary for one fact, it can never collide with a real content address
(those are hex digests), and it keeps the column NOT NULL without a migration.

## D-7 — `delete` refuses directories

An agent that wants a tree gone names the files. It bounds the blast radius,
keeps the transaction's atomicity story unchanged, and avoids a journal entry
that cannot honestly represent what it destroyed. Revisit only with evidence.

## D-8 — the acknowledgement gate is **unrecoverability**, not classification

The item recommended requiring an explicit acknowledgement for classified files
only. This goes one step wider: `drop_paths` is required whenever kaed will
retain no content — which is classified files, and would be binaries or
oversized ones if `load_text` had not already refused them upstream.

**Why wider than the recommendation.** The gate's job is "you are about to do
something kaed cannot undo, and you should know that before rather than after".
Stating the predicate as *classified* rather than *unrecoverable* would make
the rule true today by coincidence and wrong the moment the set changes. The
response already computes recoverability for D-6, so the gate is exactly "if I
am about to tell you this was unrecoverable, you have to have said so first".

It keeps the item's recommendation intact — classified is unrecoverable, so it
is still gated — and an ordinary delete still needs nothing, because the blob
makes it reversible. Same shape and same reason as `drop_keys` (008 D-10), and
the refusal names the exact paths to pass, like `allow_secrets` does for a leak
(012 D-2).

The vanish guard does not also fire on a delete: a delete destroys every value
at once, which is what `drop_paths` acknowledges, and enumerating the keys
would be a second, weaker gate on the same act.

## D-9 — `revert` restores a delete, and the ways it cannot are named separately

Restoring a deleted file is the natural inverse and the most valuable revert
kaed has. Four refusals, each with its own reason, because they are different
facts an agent acts on differently:

- `never_recoverable` — classified, or the blob is a redacted rendering.
  Nothing was ever retained; no retention setting would have helped.
- `blob_expired_or_absent` — it **was** restorable and the window passed.
- `path_reoccupied` — something is there again; restoring would clobber it.
- `nothing_to_restore` — no pre-image on the row at all.

"This was never recoverable" and "this was recoverable until last Tuesday" are
the distinction the item asked for, and they are not collapsed.

## Repaired in passing

**`revert` of a create now works.** It refused with
`revert_of_create_needs_delete`, and its message said undoing a create "means
deleting the file, and kaed has no `delete` op yet… a later slice". This is
that slice. The refusal was a placeholder for exactly this work, the fix is
mechanical, and the round trip (create → revert → delete → revert → restored)
is pinned by a test.

The revert supplies `drop_paths` on the caller's behalf there: the content
being removed is that transaction's own post-image, still in the journal, and
the caller asked for precisely this — so the gate has nothing to tell them they
do not already know.
