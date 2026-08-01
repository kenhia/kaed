# kaed MCP contract — draft v0

Status: **draft for planning** (2026-08-01). This is the starting point the
first sprints implement against; it will be revised from dogfooding and
agent-filed feedback. Nothing here is frozen.

## Transport & auth

- **Streamable HTTP MCP** (the current MCP HTTP transport), served by the
  kaed daemon on a loopback port; HTTPS exposure to the tailnet via
  `tailscale serve`, following the homelab convention used by klams/korg.
- **Auth:** `Authorization: Bearer <token>`; one token per agent identity
  (e.g. `claude`, `ghcp`). The token binds an **author identity** recorded
  on every journal entry. No anonymous mutation.
- **Statelessness:** no tool depends on hidden per-session server state.
  Any session, including a brand-new one, can act given only tool results.

## Contract-wide rules

- **R1 — versions everywhere.** Every response that carries file content
  also carries that file's `version`: the first 16 hex chars of the BLAKE3
  hash of the file's bytes. Directory-level results don't need versions.
- **R2 — mutations declare base versions; transactions are atomic.** Every
  mutating call names the version of each file it touches. All ops in a
  call apply, or none do. `dry_run` is available on every mutation.
- **R3 — budgeted responses, explicit truncation.** Size-capped results set
  `truncated: true` plus enough information (`next_offset`, remaining
  count) to continue. Never silently truncate.
- **R4 — structured errors.** Failures are `isError` results carrying
  `{code, message, data}`. Codes: `not_found`, `outside_root`,
  `version_conflict`, `ambiguous_anchor`, `anchor_not_found`,
  `invalid_input`, `too_large`, `is_binary`, `parse_unavailable`,
  `internal`. `data` makes the error actionable: `version_conflict`
  carries `{expected_version, actual_version, delta}` (a compact diff of
  what changed since the agent last looked); `ambiguous_anchor` carries the
  candidate line numbers.
- **R5 — three addressing modes, one engine.** Content **anchor** (unique
  match, robust to line drift), **range@version** (line numbers valid only
  against a declared version), **node** (tree-sitter selector). Identical
  transactional semantics underneath.
- **R6 — durable, attributed history.** Every applied transaction gets a
  journal entry: id, author (from token), timestamp, files + diffstat,
  optional agent-supplied `intent` string, git HEAD of the enclosing repo
  if any.

Paths are always relative to a configured **workspace root** (see `roots`).
Absolute paths and `..`-escapes are `outside_root` errors. Line numbers are
1-based, ranges inclusive. v0 assumes UTF-8 text files; binary files can be
`stat`ed and listed but not read/edited (`is_binary`).

## Tools

### Discovery & reading

#### `roots`
List configured workspace roots.
- **In:** —
- **Out:** `{ roots: [{name, path, description?}] }`
- Every other tool takes a `root` (name) + relative `path`.

#### `stat`
- **In:** `{root, path}`
- **Out:** `{kind: "file"|"dir"|"symlink", size, version?, line_count?,
  language?, executable, modified, binary}`
- Cheap staleness probe: one `stat` tells an agent whether its held copy
  (by `version`) is still current.

#### `list`
- **In:** `{root, path?, glob?, depth? (default 1), max? }`
- **Out:** `{entries: [{path, kind, size}], truncated, next_offset?}`
- Respects `.gitignore` by default (`ignored: true` to include).

#### `read`
- **In:** `{root, path, range?: {start, end}, window?: {line | anchor,
  context (default 10)}, numbered? (default false), max_bytes?}`
- **Out:** `{content, version, range: {start, end}, total_lines,
  truncated, next_offset?}`
- Modes: whole file (capped), explicit line `range`, or `window` — N
  context lines around a line number or around a unique anchor string.
  `window` + anchor is the cheap "show me where I'm about to edit" read.
- Whatever the mode, `version` is the whole file's version — immediately
  usable as an edit base.

#### `outline`
- **In:** `{root, path, depth?}`
- **Out:** `{version, language, symbols: [{name, kind, range: {start,
  end}, node_id, children?}]}`
- Tree-sitter symbol skeleton — functions, types, impls, headings —
  bodies elided. The token-efficient first read of any nontrivial file.
  `node_id` is a selector usable in `edit`'s `node_replace` **against the
  same version**; on other files/languages, `parse_unavailable`.

#### `search`
- **In:** `{root, pattern, regex? (default true), glob?, path?,
  context? (default 2), max_results? (default 50)}`
- **Out:** `{matches: [{path, version, line, col, text, before[],
  after[]}], truncated, total?}`
- Ripgrep-grade server-side search. Each match carries its file's
  `version`, so a hit is directly addressable: search → edit with no read
  in between, safely (a stale hit becomes `version_conflict`, not a wrong
  edit).

### Mutation

#### `edit`
One tool, transactional, all addressing modes.
- **In:**
  ```jsonc
  {
    "root": "kai-home",
    "base": [ {"path": "src/txn.rs", "version": "9f3ac2d41b7e5860"} ],
    "ops": [
      // one or more of:
      {"op": "anchor_replace", "path": "src/txn.rs",
       "old_text": "…unique text…", "new_text": "…", "occurrence?": 1},
      {"op": "range_replace", "path": "src/txn.rs",
       "start": 41, "end": 58, "new_text": "…"},
      {"op": "node_replace", "path": "src/txn.rs",
       "node_id": "fn:apply/body", "new_text": "…"},
      {"op": "insert", "path": "src/txn.rs", "after_line": 12, "text": "…"},
      {"op": "create", "path": "src/new.rs", "content": "…",
       "executable?": false, "overwrite?": false},
      {"op": "delete", "path": "src/old.rs"},
      {"op": "rename", "from": "src/a.rs", "to": "src/b.rs"}
    ],
    "dry_run?": false,
    "return_diff?": true,     // default true
    "check?": false,          // parse-check touched files post-edit
    "intent?": "extract rollback into its own fn"   // journaled
  }
  ```
- **Out:** `{txn_id, files: [{path, old_version, new_version}], diff,
  diagnostics?, applied: true|false /* false = dry run */}`
- **Semantics:**
  - Every path referenced by a non-`create` op must appear in `base` with
    its version; mismatch at apply time → `version_conflict`, nothing
    applied.
  - Multiple ops may target one file; they are applied in order against
    the evolving buffer, but versions are checked once against `base`.
  - Atomic across files: staged as temp files, fsynced, renamed; any
    failure rolls back the whole set.
  - Permissions and the executable bit are **preserved** on edit and
    settable on create (a designed-in fix for the rclone-mount papercut).
  - `anchor_replace` requires exactly one match unless `occurrence` is
    given (`ambiguous_anchor` / `anchor_not_found` otherwise).
  - The returned `diff` is the proof-of-what-changed; with
    `return_diff: true` a well-formed edit needs **no verification read**.
  - `check: true` re-parses touched files (tree-sitter) and returns
    syntax diagnostics in the same response — the edit still applies;
    the agent decides whether to revert. (Hookable to fmt/lint Later.)

### History

#### `journal`
- **In:** `{root, path?, author?, since?, max? (default 20)}`
- **Out:** `{entries: [{txn_id, author, time, intent?, files: [{path,
  old_version, new_version}], diffstat, git_head?}], truncated}`
- The cross-session, cross-agent memory: "what happened here recently and
  who did it." A resuming session reads the journal instead of re-deriving
  the world.

#### `diff`
- **In:** `{root, path, from: <version|txn_id>, to?: <version|txn_id|"current">}`
- **Out:** `{diff, from_version, to_version}`
- Works for any version the journal still knows (journal retains content
  needed to reconstruct; retention window configurable).

#### `revert`
- **In:** `{root, txn_id, dry_run?}`
- **Out:** same shape as `edit` (a revert **is** a new journaled
  transaction, never history rewriting).
- Fails with `version_conflict` if later transactions touched the same
  regions; the agent resolves via `diff` + a fresh `edit`.

### Meta

#### `feedback`
- **In:** `{category: "friction"|"bug"|"wish"|"praise", summary,
  detail?, context?}`
- **Out:** `{id}`
- The evolution loop, in-band: an agent that just fought the contract
  files the report *through* the contract, attributed and timestamped.
  Stored server-side; review/promotion to korg work items is a human/agent
  chore for now (open question in overview.md).

## Worked example — the core loop

```text
1. search {pattern: "fn apply_txn", root: "kai-home", path: "src/ai/kaed"}
     → match in src/txn.rs line 41, version 9f3ac2d41b7e5860
2. read {path: "src/txn.rs", window: {line: 41, context: 20}}
     → 40 lines, version 9f3ac2d41b7e5860  (still current)
3. edit {base: [{src/txn.rs @ 9f3ac2…}],
         ops: [{anchor_replace …}], check: true}
     → txn_id 1082, new_version 77b0e1c92aa4f3d5, diff (+6 −2), no diagnostics
DONE — three calls, no verification read, journaled and attributed.
```

And the conflict path, which is the point of it all:

```text
3'. edit {base: [{src/txn.rs @ 9f3ac2…}], …}
      → error version_conflict
        data: {expected: 9f3ac2…, actual: 4c11d8…,
               delta: "@@ −38,4 +38,9 @@ …"}   ← what changed since you looked
4'. agent inspects delta — usually re-anchors and retries in one step,
    without re-reading the file.
```

## Deliberate v0 exclusions

| Excluded | Why | Where it lands |
|---|---|---|
| `exec` / shell | Agents have ssh; mutation surface stays file-only | never |
| git operations | git-over-ssh works; journal records `git_head` for correlation | never (correlation only) |
| `apply_patch` (unified-diff input) | overlaps `edit` ops; wait for demand | Next, if dogfooding misses it |
| LSP semantics (rename-symbol, references) | heavy lifecycle; tree-sitter first | Later |
| leases / locks | optimistic versioning until real contention observed | Later, if ever |
| MCP resources / subscriptions | tools-only keeps v0 small (matches klams/korg) | Later |
| binary & non-UTF-8 editing | rare need, big complexity | Later |
