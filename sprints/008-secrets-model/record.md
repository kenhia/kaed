# Sprint 008 — the secrets model: policy, classification, redaction

*korg proposal 1057; work items #1047 (slice 1) and #1048 (slice 2).
Started 2026-08-07. Design source: `../planning/brainstorm-secrets-editing.md`,
constrained by PD-2 (BLAKE3 + entropy floor, not HMAC).*

## Goal

Make kaed safe around secret-bearing files without making it useless near
them. Two slices from the brainstorm's build order:

1. **Policy** — `.kaedignore` (gitignore-shaped, in-repo), an in-file
   `# kaedignore` marker, structured refusal reasons on every denial, and a
   gitignore warning on classified files that could be committed.
2. **Classification + redaction** — heuristics *classify* rather than deny;
   `.env`-shaped files get a typed, redacted read surface and typed write
   ops with placeholder passthrough; every derived surface (diff, conflict
   delta, search hits, journal blobs) is redacted too. No `reveal`.

The honest frame, which is also the complexity budget: any agent with a
shell can `cat .env`. Nothing here is access control — it is blast radius
and ergonomics. Features that make the common well-intentioned path safe
are worth a lot; features that try to stop a determined agent are worth
near zero.

## Shape of the change

New modules:

- `src/secrets.rs` — placeholder identity (BLAKE3 digest truncated to the
  same 16 hex chars as `version_of`, per PD-2), the entropy floor that
  decides whether a digest is disclosed at all, and the secret-shape
  detector used on comments (and later by write-side leak detection, #1053).
- `src/dotenv.rs` — strict dotenv parse (every line blank / comment /
  `KEY=value`, else the file is not dotenv), line-preserving redacted
  rendering, and the typed ops (`env_set`, `env_rename`, `env_delete`,
  `env_reorder`) with placeholder resolution and the vanish guard.
- `src/policy.rs` — the `.kaedignore` stack (per-file evaluation, own-file
  negation only), the in-file marker check, the classifier (second lexical
  taxonomy beside the deny list), and the best-effort gitignore check.

Changed:

- `deny.rs` / `config.rs` — `DEFAULT_DENY` loses `.env*`, `*.pem`, `*.p12`,
  `id_*`; those move to `DEFAULT_CLASSIFY`. `[security]` gains `classify`
  and `use_default_classify`.
- `errors.rs` — every refusal carries `data.reason`
  (`server_denylist` | `kaedignore` | `in_file_marker` | `classified_opaque`
  | `kaedignore_protected`) and a `hint` naming what to do instead.
- `fsops.rs` — `check_denied` also consults the `.kaedignore` stack;
  `load_text` refuses marker files and opaque-classified files and reports
  dotenv classification to its callers; `read` is split so the range/window/
  budget logic can run over a redacted rendering.
- `search.rs` — classified dotenv files are searched over their redacted
  rendering; opaque-classified files are skipped and counted in a new
  `classified_hidden`; marker files count into `denied_hidden`.
- `txn.rs` — env ops in the same transaction envelope; `drop_keys`; diffs
  and conflict deltas redacted for classified files; `.kaedignore` is
  unwritable.
- `journal.rs` — blobs gain a `redacted` column (migration on open);
  classified content is journaled as its redacted rendering; the recorder
  record decouples diffstat from blob content so opaque content can be
  withheld entirely.
- `server.rs` — `read` returns `redacted`, `dotenv` entries, `usage_hint`
  and `warnings`; `edit` takes `drop_keys`; capabilities gain `secrets`.

Decisions with reasoning: [decisions.md](decisions.md). The open questions
the WI required settling are D-8 (search oracle), D-10/D-11 (vanish guard,
no shadow), and PD-2's cross-root consistency corollary (D-6).

## What shipped

Both slices, in one branch, `just check` green throughout (fmt, clippy
`-D warnings`, 161 lib tests + 9 HTTP protocol tests + 9 installer tests).

**Slice 1 (#1047)** — `.kaedignore` (per-file evaluation, own-file `!`
negation only, unwritable and always readable through kaed), the in-file
marker (strict token, first 5 lines, enforced where content opens), the
refusal `reason`/`hint` taxonomy on every `denied` (D-5), the classifier
as a second lexical taxonomy with `DEFAULT_DENY` slimmed to credential
stores (D-1), and the gitignore warning via `git check-ignore` (D-12).
`check-config` prints classify rules beside deny rules.

**Slice 2 (#1048)** — strict dotenv parse (D-2), line-preserving redaction
(D-7) with PD-2 placeholders (BLAKE3 at `version_of` truncation, 80-bit
entropy floor, digests withheld below it), the shape detector over values
*and* comments, typed env ops riding the `edit` envelope with placeholder
passthrough and loud digest-mismatch failure (D-9), the transaction-level
vanish guard behind `drop_keys` (D-10), and every derived surface redacted:
outcome diff, `version_conflict` delta (including redact-on-read of legacy
plaintext blobs), search hits (searched over the redacted rendering — the
value-probe oracle is dead by construction, D-8), and journal blobs (new
`blobs.redacted` column, migrated on open; opaque content withheld
entirely, audit diffstat decoupled and kept). No `reveal`, no shadow
(D-11). `stat` gained `classified`, `search` gained `classified_hidden`,
`read` gained `redacted`/`dotenv`/`usage_hint`/`warnings`, capabilities
gained `secrets`.

New modules `src/secrets.rs`, `src/dotenv.rs`, `src/policy.rs`; contract
updated (R4, R7, new R9, tool sections); `config.example.toml`,
`docs/setup.md` and `CLAUDE.md` updated. End-to-end wire test drives the
full loop over real HTTP: redacted read → placeholder passthrough env op →
vanish-guard refusal → `drop_keys` confirm.

**Deploy consequence to say out loud:** on upgrade, `.env`-shaped files
flip from `denied` to redacted-readable on kai and kubs0. That is the
sprint's purpose, not a regression; existing configs need no edit
(classify defaults are on).

Shipped as PR #8 (squash `a9b8ea0`), deployed 2026-08-07 as
`0.1.0-a9b8ea0` — see [deploy.md](deploy.md) for the per-host table, the
live redacted-read smoke test, and the journal migration evidence.

## Follow-ups

- History tools (#1049) must treat `blobs.redacted = 0` rows that belong to
  now-classified paths as redact-on-read: pre-008 journals hold plaintext
  for files like `korg.env` that the old deny list never matched.
- `list` cannot see in-file markers (it never opens files) — documented
  behavior, revisit only if it bites.
- Generalized YAML/TOML redaction deliberately not chased (brainstorm rule:
  wait for demand). Opaque-classified files refuse with a precise reason
  instead.
- Measure pressure for `reveal` (slice 3, #1051) from live usage before
  building it.
