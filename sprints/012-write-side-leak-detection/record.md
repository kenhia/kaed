# Sprint 012 — write-side leak detection

*korg proposal 1061; work item #1053 (M). Started 2026-08-08. Design
sources: `../planning/brainstorm-secrets-editing.md` § "Things not on the
list" item 1 (ranked **first** by value per line of code), the proposal's
notes, PD-2 (BLAKE3 identity), 008 D-6/D-10 (digests, the vanish-guard
precedent), 011 D-6 (the audit stream). Depends on 008 and 011 (both
shipped); fleet-wide queries ride 010's fan-out, already settled by 011
D-8.*

## Goal

Catch the incident that actually happens. Both of the original secrets
ideas were read-side, but kaed sees **every write**, and the
higher-frequency real incident runs the other direction: a secret written
*into* a README, a test fixture, a doc, a commit. Writing the klams token
into `.env` is expected; writing it into `README.md` is the incident, and
before this sprint nothing noticed.

Scope, per the proposal:

- **Refuse or flag** content matching a known secret's BLAKE3 digest, a
  provider prefix (`sk-ant-`, `ghp_`, `AKIA`, …), or a high-entropy shape —
  into a file **not** classified as secret-bearing.
- Precision split (the proposal's second deliberate call): digest and
  provider-prefix matches are precise → **refuse with a named override**;
  the generic entropy heuristic will false-positive on fixtures, checksums
  and UUIDs → **flag**, until evidence says otherwise.
- Every refusal says what to do instead (reference the variable, not the
  value), and every refused write lands in the secrets audit stream —
  a refused leak is exactly the event worth counting later.
- The **known-digest reverse lookup** is the one piece of new machinery:
  011 already shipped `occurrences` (single-host, digest-keyed) and settled
  fleet-wide occurrence queries as `search {pattern: <digest>, root: "*:*"}`
  (011 D-8), so the cross-file index item from #1053 reduces to the index
  that powers the write-side precise tier.

## Shape of the change

New module:

- `src/leak.rs` — tokenization of newly-introduced content and the three
  detection tiers; pure functions, no I/O.

Changed:

- `src/journal.rs` — `secret_digests` table (digest → root/path/key,
  digests only, above-floor only): backfilled from retained redacted blobs
  on open, fed by every redacted blob journaled, every secret event, and
  redacted classified-dotenv reads. Two new audit actions: `leak_refused`,
  `leak_flagged`.
- `src/txn.rs` — the check on the write path, beside the vanish guard;
  `allow_secrets` on the edit request; heuristic flags ride `warnings`.
- `src/server.rs` — `allow_secrets` param; digest capture on classified
  reads.
- `src/config.rs` — `[secrets] leak_checks = "refuse" | "flag" | "off"`
  (default `refuse`) as the measured-rollout lever.
- Contract: new R12; `edit` and `journal` sections updated.

## What shipped

The whole slice, one branch, `just check` green throughout.

**The check** (`src/leak.rs` + the hook in `txn.rs`, D-1..D-3): every
touched unclassified file is scanned after ops apply, beside the vanish
guard. Newly-introduced token runs (with trimmed and `=`-split variants,
so quotes, JSON, `KEY=value` and sentence punctuation don't hide a match)
are checked strongest-tier-first: known digest → provider prefix /
private-key armor → entropy shape. Precise tiers refuse with
`invalid_input` / `reason: secret_leak`, a match list that never echoes
the token, and the exact `allow_secrets` override; the digest tier's
refusal names the variable and file to reference instead. The heuristic
tier warns and applies — tightened beyond 011's `looks_secret_shaped`
(digit required, bare UUIDs exempt) after the first test run showed
snake_case identifiers and doc UUIDs would have made warnings noise.
`revert` takes `allow_secrets` too: restoring a removed secret is a
re-leak.

**The index** (`secret_digests` in journal.db, D-4): digests only,
above-floor only, locations included. Fed by every redacted blob
journaled, every secret event, and every redacted `read` served (the
load-bearing feed: it is how a hand-provisioned klams token becomes
precisely refusable without any walk); backfilled idempotently on every
open from `secret_events` and retained redacted blobs. Engine access is
two new `TxnRecorder` methods (`known_digests`, `leak_events`) with
honest no-op defaults.

**The audit trail** (D-3): `leak_refused` / `leak_flagged` /
`leak_allowed` rows in the 011 secrets stream, readable as `journal` kind
`"secret"`; dry runs check but never journal. **The lever** (D-5):
`[secrets] leak_checks = refuse|flag|off`, printed by `check-config`,
stamped onto `ResolvedRoot`. Capabilities gained `leak_detection` so a
mid-upgrade fleet says which hosts check.

Tests: 10 leak.rs unit tests (including the trailing-period and
already-present-token traps that failed first and shaped the design), 3
journal index tests (round-trip, begin-feed, backfill-excludes-leak-rows),
7 engine tests (refuse→override loop, digest tier naming the variable,
heuristic warn-and-apply, classified exemption, dry-run predicts without
journaling, existing-token file stays editable, config lever), and one
end-to-end wire test — redacted read teaches the digest, the README paste
refuses, the override lands with a warning, provider prefix refuses with
no index, both events countable in the journal. Contract gained R12 and
the edit/journal/revert updates; README, `docs/setup.md`,
`deploy/config.example.toml` and `CLAUDE.md` updated.

## Follow-ups

- Promote (or demote) the heuristic tier with `leak_flagged` evidence
  once real usage accumulates — the D-3 measurement, not before.
- `search` does not feed the digest index (only `read` and the write
  path do); add it if coverage evidence says redacted search hits are a
  common first contact with a secret.
- The tokenizer's variant expansion is deliberately simple (trim + `=`
  splits). JWTs and other dotted composites match by digest only if
  pasted whole; revisit if a real miss shows up.
