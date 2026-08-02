# Contract clarifications discovered while implementing

Things mcp-contract.md (draft v0) left unspecified or that implementation
forced a decision on. Fold into the first contract revision; anything an
agent should know at call time also belongs in tool descriptions.

## Paths & jail

- `..` components are rejected outright (`outside_root`), not resolved —
  an agent has no legitimate use for them since every file is addressable
  root-relatively.
- `create` makes missing parent directories automatically; there is no
  mkdir tool.
- `.git` directory contents never appear in `list` (regardless of
  `ignored`) — kaed is not a git browser, and listing a repo root would
  drown in it.

## Versions & stat

- `stat` computes `version` for binary files too (staleness probing works
  on any file ≤ `max_file_bytes`); `line_count` only for text.
- Non-UTF-8 files are treated as binary (`is_binary`), per the v0
  UTF-8-only rule.

## read

- Byte budget is applied line-wise: never a partial line, except when a
  single line alone exceeds the budget — then it is cut at a char
  boundary, `truncated: true`, and `next_offset` is absent (no line-wise
  continuation exists mid-line).
- `numbered` format is `<n>\t<text>` with absolute file line numbers.
- Range reads clamp `end` to EOF but reject `start` past EOF
  (`invalid_input`).
- Whole-file and range slices are byte-exact (line terminators included),
  so whole-file content re-hashes to `version`.

## edit

- Extra `base` entries (declared but untouched by ops) are verified as
  "assert unchanged" — a cheap cross-file consistency primitive.
- A `base` file missing from disk is `version_conflict` with
  `actual_version: "absent"`, not `not_found` — the agent's model is
  stale either way.
- `version_conflict.delta` is present when the blob store retains the
  expected version's content; otherwise a fixed "not retained; re-read"
  marker. Reads are not journaled, so a version kaed never wrote may be
  unavailable.
- Ops on a file created earlier in the same transaction need no `base`
  entry (there is nothing to verify against).
- `create` on an existing path without `overwrite` is `invalid_input`;
  with `overwrite` the old content/version are journaled. Overwriting a
  binary file is refused (`is_binary`).
- An edit whose result exceeds `max_file_bytes` is `too_large` and
  nothing applies.
- Empty `ops`, duplicate `base` entries, and editing an undeclared path
  are `invalid_input`.
- Concurrency: one global transaction lock in v0 (per-file locks only if
  real contention appears). Two racing writers: loser gets
  `version_conflict` whose `actual_version` is the winner's new version.

## list

- Contract In needs an `offset` param — `next_offset` implies resumable
  pagination but draft v0 gave no way to pass it back.
- Ordering is lexicographic by root-relative path (stable pagination).

## From the cleo live test (WIs #908–911)

- The server `instructions` must document that `version_conflict.data`
  carries `actual_version` (the retry base), not only `delta` — fixed in
  code 2026-08-02; fold into the contract doc's error-model section.
- Deliberate-decision item for the revision: journal blobs retain whole
  pre/post images (that's what powers diff-proof and conflict deltas),
  which snapshots any credential inside an edited file for
  `retention_days` — and captures third-party pre-images kaed never
  authored. Needs an explicit stance (denylist? shorter retention?
  no-blob roots?).
- Failed transactions (conflicts, rollbacks) are currently invisible in
  the journal; conflict-rate-per-author is the first health metric worth
  emitting.

## Journal

- v0 records `begin` (after staging, before renames) and `complete`
  (after all renames): an interrupted transaction is detectable as a
  pending row. Automated repair deferred until dogfooding shows what torn
  states look like in practice.
