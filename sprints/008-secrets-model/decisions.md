# Sprint 008 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. PD-2 (BLAKE3 + entropy floor) is the decision
> this sprint implements; everything here is a call made *inside* it.

---

## D-1 — Classification is a second lexical taxonomy, and the deny list shrinks

`DEFAULT_DENY` loses `**/.env`, `**/.env.*`, `**/*.pem`, `**/*.p12`,
`**/id_rsa|id_ecdsa|id_ed25519`; a new `DEFAULT_CLASSIFY` gains them plus
`**/*.env`, `**/credentials*`, `**/*.kdbx`. Explicit config (`[security]
deny`) still hard-denies; heuristics only classify. This is the
deny-vs-classify distinction the brainstorm names and the reason #929
(kubsdb) is blocked on this sprint: under a lexical hard-deny the only
shapes for broad access are "deny so much it is narrow again" or "hand out
plaintext".

`**/*.env` is new and load-bearing: the motivating kubsdb file is
`korg.env`, which `**/.env` never matched — meaning pre-008 journals can
hold its plaintext (see D-11's corollary).

Precedence, decreasing absoluteness: server deny (builtin + config) →
`.kaedignore` → in-file marker → classification. Each layer only ever adds
restriction.

**Consequence for existing hosts:** on upgrade, `.env` files flip from
`denied` to redacted-readable. That is the sprint's purpose, not a
regression; the deploy notes must say so.

## D-2 — Dotenv-or-opaque is decided by content, not by config tags

A classified file whose every line is blank, a `#` comment, or
`KEY=value` (with an optional `export `) gets the redacted dotenv surface.
Any other classified file — `.pem`, `.kdbx`, a YAML with a sneaky name —
refuses with `reason: classified_opaque` and a hint. No format tags in
config, nothing to mis-declare; the strict grammar is the gate. A PEM
cannot pass it (armor lines are not `KEY=value`), and a multi-line quoted
dotenv value fails it too, which is the escape hatch that keeps the
line-preserving redaction honest (D-7).

Refusing opaque files sounds like the "heuristic hard-deny" the brainstorm
warns against, but the warning is about *confusing dead ends*: a bare
`denied` with no explanation. `classified_opaque` names the rule, says
kaed has no redacted surface for this shape yet, and points at the shell
convention. Structured refusal is the graceful degradation until demand
justifies more surfaces.

## D-3 — `.kaedignore`: per-file evaluation, own-file negation only

A path is refused if **any** `.kaedignore` between the root and the file
denies it, where each file is evaluated independently with full gitignore
semantics *internally*. So `!pattern` can un-deny something the same file
denied — the gitignore idiom stays available — but can never relax another
file's rule or the server's. This is deliberately *not* stock multi-file
gitignore semantics, where a deeper file's `!` overrides a shallower one:
that is exactly the "compromised agent writes `!~/.ssh`" hole the
brainstorm closes. (It also means `ignore::WalkBuilder`'s
`add_custom_ignore_filename` cannot be used for enforcement — the walkers
check the stack themselves, cached per call.)

Corollaries: `.kaedignore` is unwritable through kaed (any edit op
targeting one refuses with `kaedignore_protected`), and always *readable* —
exempt from kaedignore rules (not from server deny), because a refusal
names the file that caused it and an agent must be able to go read it.

## D-4 — The in-file marker is a strict token, checked where content is opened

`kaedignore` alone on a comment line (`# kaedignore`, `// kaedignore`,
`<!-- kaedignore -->`), within the first 5 lines. Prose mentioning the
feature does not trigger it; that strictness plus the 5-line limit is the
mitigation for the in-band-signalling (injection-shaped) tradeoff the WI
requires writing down: file content is controlling server policy here, and
a vendored file could deliberately carry the marker. Low severity — the
blast is "kaed refuses one file" — and the structured
`reason: in_file_marker` makes it diagnosable in one read.

Enforced in `load_text` (covers `read` and every edit base) and in
`search` (which opens every file anyway). **`list` and `stat` do not check
it** — `list` never opens files and scanning every listed file would turn a
directory listing into a full tree read; `stat` serves only metadata.
Documented, not hidden.

## D-5 — One refusal code, a `reason` taxonomy, and a mandatory hint

Everything stays `code: denied` — agents already know `denied` is permanent
and not worth retrying — with `data` growing
`reason: server_denylist | kaedignore | in_file_marker | classified_opaque
| kaedignore_protected` and a `hint`. The WI's rule is the design: **every
refusal says what to do instead**, because a refusal with no alternative is
how an agent ends up routing around kaed via ssh and the journal loses the
edit too. Hints name the governing file (`.kaedignore` path, config
`[security] deny`) or the working alternative (redacted read, env ops, the
shell `set -a` convention).

## D-6 — Placeholder: `⟨kaed:KEY@digest⟩`, digest = `blake3(value)` at 16 hex chars

Per PD-2. The truncation matches `fsops::version_of` — one hash story in
the codebase, and 64 bits is ample for fleet-wide non-collision. A value
below the entropy floor gets `⟨kaed:KEY⟩`, no digest: the brute-force
objection to bare hashes is answered by withholding, not keying.

The floor: estimated entropy ≥ 80 bits, from `len × log2(charset-class
size)` over the observed character classes, with a distinct-character guard
(< 5 distinct chars → withheld regardless). Conservative in the safe
direction — withholding only costs change-detection on that value, never
safety. 80 bits because BLAKE3 is *fast*: an offline enumeration of a
50-bit password space against a disclosed digest is cheap, so the bar sits
where enumeration stops being a weekend project.

Cross-root and cross-host consistency (a WI open question) falls out for
free: the digest is derived from the value alone, so the same secret
reachable through two roots — or two hosts — carries the same placeholder
by construction. Nothing to synchronize.

## D-7 — Redaction is line-preserving

The redacted rendering of a dotenv file has exactly the raw file's line
count: entry values are replaced in place, comment tokens are replaced in
place, nothing is inserted or dropped. That single property is what lets
the rest of the surface compose unchanged: `range` and `window` reads,
anchor resolution, `line_count`, and search line numbers all stay valid
over redacted text. The `version` returned with redacted content is the
**raw** file's version — a version is a content address of the bytes on
disk (R1) and must remain a usable edit base; the redacted text is a view,
not a document with its own identity.

## D-8 — `search` searches the redacted rendering; the oracle dies by construction

The WI asks two questions — does search redact its hits (yes), and what
does a pattern that matches token shapes return. Answering the second by
filtering *results* would leave an oracle: "does `hunter2` match anywhere
in `.env`" answers with match-count even if the hit text is redacted. So
search does not run over raw classified content at all: for classified
dotenv files the pattern runs over the same redacted rendering `read`
serves. A pattern can match keys, comments, placeholders — everything the
agent could see anyway — and a probe for a value literal matches nothing
because the value is not in the searched text. Opaque-classified files are
skipped and counted in `classified_hidden` (sibling of `denied_hidden`,
same R7 honesty rule: a filtered result must never look like the whole).

## D-9 — Typed env ops ride the existing `edit` envelope

`env_set` / `env_rename` / `env_delete` / `env_reorder` are new `EditOp`
variants, not a new tool: they get base versions, multi-file atomicity,
`dry_run`, `intent`, and journaling for free, and the agent keeps one
mutation vocabulary. Text ops (`anchor_replace`, `range_replace`) on a
classified dotenv file refuse with a hint naming the env ops — the
brainstorm's "make the ugly failure modes unrepresentable". Env ops also
work on *unclassified* dotenv-parseable files (they are just a better way
to edit dotenv); classification governs redaction, not the op vocabulary.

Placeholder passthrough: a value that is **exactly** a placeholder resolves
against the target file's base content — same key keeps the value, another
key's placeholder copies it (the equality-across-keys use case). A digest
that no longer matches fails loudly (PD-3's rule: current-vs-exact must
never be ambiguous). A value that merely *contains* placeholder syntax is
refused — a truncated placeholder must never land as a literal.

## D-10 — The vanish guard is a transaction-level invariant, not per-op ceremony

After all ops apply to a classified dotenv file, kaed compares the set of
non-empty values before and after: any key whose value was destroyed
(deleted, or overwritten by a new literal) must be named in the request's
`drop_keys`, or the transaction refuses with the key list and a hint.
Renames and placeholder passthrough preserve values and need no
declaration. One uniform rule regardless of which op (or `create` with
`overwrite`) caused the vanish — the guarded event is "a secret the agent
never saw stops existing", not any particular verb.

## D-11 — No recoverable shadow: the journal stores redacted renderings

The WI's deliberate-decision item, decided **against** the shadow. For
classified files, journal blobs hold the redacted rendering (new
`blobs.redacted` column; schema migrated on open), and opaque-classified
content is withheld entirely — the recorder now takes diffstat separately
from blob content so audit stats survive withholding. Reasons: (a) #909
already names journal.db "as sensitive as the most sensitive file kaed may
edit" — a plaintext shadow of every secret with a 7-day window would make
it *the* most sensitive file on the host, expanding blast radius exactly
where this sprint shrinks it; (b) the loss the shadow would recover from is
already prevented upstream — D-10 makes undeclared destruction impossible,
and declared destruction is the agent stating it understands the value is
gone; (c) like `reveal`, it can be added later if real incidents demand it,
but plaintext once written cannot be retroactively unwritten. Ship without,
measure.

**Corollary for #1049:** conflict deltas (and future history tools) must
check the blob's `redacted` flag and redact-on-read legacy plaintext blobs
of now-classified paths — pre-008 journals hold e.g. `korg.env` plaintext
because the old deny list never matched it.

## D-12 — Gitignore warning is best-effort via `git check-ignore`

On read or edit of a classified file inside a git repo, kaed asks git
whether the path is ignored; if it is not, the response carries a
`warnings` entry saying the file can be committed. Subprocess, not a
reimplementation: `journal.rs` already shells to git for `git_head`, the
check runs only on classified files (rare), and git's own answer respects
excludes/global config that a reimplementation would get subtly wrong —
the `**/secrets/**` lesson from 004 applied preemptively. No git, no repo,
or git errors → no warning (best-effort, like `git_head`). The brainstorm
ranks this #2 by value per line of code: it catches the actual disaster,
a live `.env` heading for a commit.
