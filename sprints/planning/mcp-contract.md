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
- **Tokens do not expire.** A 401 means the presented token matches no
  configured identity — wrong or rotated — and says so in the
  `WWW-Authenticate` challenge (RFC 6750: `error="invalid_token"` plus a
  description). There is no TTL to hunt for. Rotation is non-breaking
  server-side: a previous token can be honoured during a grace window and
  new tokens load on `SIGHUP` without dropping live sessions. Clients
  still learn a new secret only when they restart.
- **Statelessness:** no tool depends on hidden per-session server state.
  Any session, including a brand-new one, can act given only tool results.

## Contract-wide rules

- **R1 — versions everywhere.** Every response that carries file content
  also carries that file's `version`: the first 16 hex chars of the BLAKE3
  hash of the file's bytes. Directory-level results don't need versions.

  A version is a **pure content address, not a lease or a session handle**.
  It is a fact about the bytes, computed on demand, held nowhere. So a
  version never expires and stays valid across client restarts, server
  restarts, transport reconnects, and token rotation — the only thing that
  invalidates it is the file's content changing, which is exactly what it
  is for. An agent resuming after a crash or a context compaction can edit
  straight from a version it recorded long ago: it either succeeds or gets
  a precise `version_conflict` with a delta. No defensive re-read, no
  "refresh the handle first" step. (Verified live 2026-08-02: a version
  taken before a client restart, a server reconnect, *and* a credential
  rotation was still accepted as an edit base afterward.)
- **R2 — mutations declare base versions; transactions are atomic.** Every
  mutating call names the version of each file it touches. All ops in a
  call apply, or none do. `dry_run` is available on every mutation.
- **R3 — budgeted responses, explicit truncation.** Size-capped results set
  `truncated: true` plus enough information (`next_offset`, remaining
  count) to continue. Never silently truncate.
- **R4 — structured errors.** Failures are `isError` results carrying
  `{code, message, data}`. Codes: `not_found`, `outside_root`, `denied`,
  `version_conflict`, `ambiguous_anchor`, `anchor_not_found`,
  `invalid_input`, `too_large`, `is_binary`, `parse_unavailable`,
  `internal`, and `unsupported_capability` (reserved for peer mode:
  a call against a root whose host lacks that capability).
  `data` makes the error actionable: `version_conflict`
  carries `{expected_version, actual_version, delta}` (a compact diff of
  what changed since the agent last looked); `ambiguous_anchor` carries the
  candidate line numbers; `denied` carries `{path, rule}`.
- **R7 — the deny list is absolute, and never silent.** Some paths are
  refused inside the roots: kaed's own config and journal directories
  always, plus configured globs (`.ssh`, `.env`, `*.pem`, … by default).
  Addressing one is `denied` — a permanent refusal, not a path to correct,
  so it should never be retried. Enumeration (`list`, `search`) omits them
  instead of failing, and reports `denied_hidden: N` so a filtered result
  is never mistaken for the whole directory. The check is lexical, applied
  identically to paths that exist and paths that don't, so a `denied` is
  never evidence that a file is there.
- **R5 — three addressing modes, one engine.** Content **anchor** (unique
  match, robust to line drift), **range@version** (line numbers valid only
  against a declared version), **node** (tree-sitter selector). Identical
  transactional semantics underneath.
- **R6 — durable, attributed history.** Every applied transaction gets a
  journal entry: id, author (from token), timestamp, files + diffstat,
  optional agent-supplied `intent` string, git HEAD of the enclosing repo
  if any.

- **R8 — root names are host-qualified.** A root is named `host:root` —
  `kai:src`, never `src`. `root` was always the indirection every tool
  addresses through, so this is a naming change and not a schema change:
  no signature moves, and a client wired directly to one host already
  speaks the vocabulary peer mode (korg:1050) will route on. The
  unqualified form is **not** accepted as an alias — one root, one
  spelling — but a `not_found` on it names the qualified replacement
  rather than shrugging.

  Corollary: root names are also **durable**, and history outlives them.
  A journal row records the root name in force when it was written, so a
  rename leaves rows naming roots that no longer resolve. Those rows are
  true and are neither rewritten nor aliased back into existence: history
  tools mark such a transaction **historical** with a structured reason,
  and `revert` refuses it saying why. (Added sprint 007; see D-6 there.)

Paths are always relative to a configured **workspace root** (see `roots`).
Absolute paths and `..`-escapes are `outside_root` errors. Line numbers are
1-based, ranges inclusive. v0 assumes UTF-8 text files; binary files can be
`stat`ed and listed but not read/edited (`is_binary`).

## Tools

### Discovery & reading

#### `roots`
What this instance serves, and which hosts are supposed to.
- **In:** —
- **Out:**
  ```jsonc
  {
    "host": "kai",
    "roots": [
      {"name": "kai:src", "host": "kai", "path": "/home/ken/src",
       "description": "code repos", "status": "active",
       "capabilities": ["stat", "list", "read", "search", "edit"]}
    ],
    "fleet": {
      "declared": true,
      "hosts": [
        {"host": "kai", "status": "active", "self": true, "verified": true,
         "version": "0.1.0 (…)", "roots": ["kai:src", "kai:scratch"]},
        {"host": "kubs0", "status": "active", "self": false, "verified": false,
         "url": "https://kubs0.<tailnet>:4870/mcp"},
        {"host": "kubsdb", "status": "deferred", "self": false, "verified": false,
         "ref": "korg:929", "note": "broad-access design not settled"}
      ]
    }
  }
  ```
- Every other tool takes a `root` (the qualified `name`) + relative `path`.
- **`roots` is the only discovery tool.** No `list_available_targets`: two
  mechanisms answering one question is worse for an agent than either alone.
- `capabilities` is per-root and is the **union** across a fleet, never the
  intersection — an intersection silently hides working features on the
  most-updated host. Calling a root that lacks one gets
  `unsupported_capability` (reserved; nothing lacks one on a single
  instance).
- **`fleet` answers "which hosts should run kaed, and do they"** without
  reading project history. Three states stay distinguishable, because
  collapsing any two is the confusion this was built for:
  - `deferred` — deliberately without an instance. Always carries `ref`.
    **Not a broken deploy; do not install one to "fix" it.**
  - `unreachable` — should be serving, is not. Always carries `since`.
  - never-declared — simply absent from `hosts`.
- `verified` separates **declared** from **observed**: only the instance
  answering the call is verified. A config-declared peer reported as plain
  `active` would assert something nobody checked.
- `declared: false` means the host has no `[peers]` declaration at all. Then
  `hosts` is this instance alone and an absence means *nothing* — it is not
  evidence of a deferral, and not evidence against one.
- The same three states are reachable from the **error** path, which is
  where an agent that assumed rather than asked actually lands: a bad `root`
  returns `not_found` with `data.reason` of `host_deferred` (plus `ref`),
  `host_unreachable` (plus `since`), `peer_routing_unavailable`,
  `host_never_declared`, `fleet_undeclared`, `unqualified_root` (plus
  `did_you_mean`) or `unknown_root`. The payload names both machines
  explicitly — `this_host` (the instance answering) and `target_host` (the
  one the root named) — never a bare `host`.

#### `stat`
- **In:** `{root, path}`
- **Out:** `{kind: "file"|"dir"|"symlink", size, version?, line_count?,
  language?, executable, modified, binary}`
- Cheap staleness probe: one `stat` tells an agent whether its held copy
  (by `version`) is still current.

#### `list`
- **In:** `{root, path?, glob?, depth? (default 1), max? }`
- **Out:** `{entries: [{path, kind, size}], truncated, next_offset?,
  entries_scanned, reason?, denied_hidden?}`
- Respects `.gitignore` by default (`ignored: true` to include).
- `denied_hidden` counts entries the deny list removed (R7); a hidden
  directory counts once, subtree included. Absent when zero.
- `entries_scanned` is what the walk produced *before* `glob` — always
  present, including zero. See `search` for why.
- `reason` (see `search`) when nothing matched at all. Absent for an empty
  page past the end: that is already explained by `next_offset`.

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
  after[]}], truncated, total?, files_searched, reason?, denied_hidden?}`
- Ripgrep-grade server-side search. Each match carries its file's
  `version`, so a hit is directly addressable: search → edit with no read
  in between, safely (a stale hit becomes `version_conflict`, not a wrong
  edit).
- **`files_searched` is always present, including zero.** `0 matches in 41
  files` and `0 matches in 0 files` are different answers, and until they
  were distinguishable a scoping mistake was outcome-identical to a genuine
  no-match — the same class of world-model corruption R3 forbids for
  truncation and R7 for filtering. `denied_hidden` is its sibling.
- `reason: {code, hint}` when the emptiness is more likely the caller's than
  the tree's. `hint` is built from the call's own `glob` and `path`, so it
  names the fix rather than restating the rule:
  - `glob_matched_no_files` — **`glob` matches root-relative paths and is
    not re-anchored by `path`.** With `path: "ai/kaed"`, a bare
    `glob: "README.md"` can only match at the root's top level. This one
    cost a wrong conclusion in sprint 006 before it was a rule.
  - `no_files_under_path` — the walk produced nothing (empty, or all
    gitignored).
  - `all_files_skipped` — files matched but every one was binary, over
    `max_file_bytes`, or unreadable.

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
0. roots
     → kai:src (active), kai:scratch (active)
       fleet: kai active(self) · kubs0 active(declared) · kubsdb deferred korg:929
1. search {pattern: "fn apply_txn", root: "kai:src", path: "ai/kaed"}
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

And the zero that used to lie (R8's sibling, korg #1066):

```text
1''. search {pattern: "journal", root: "kai:src", path: "ai/kaed",
             glob: "README.md"}
       → matches: [], files_searched: 0
         reason: {code: "glob_matched_no_files",
                  hint: "…`glob` is matched against ROOT-relative paths and is
                         not re-anchored by `path` … try "ai/kaed/README.md"
                         or "**/README.md""}
2''. agent retries with the suggested glob. Before this, step 1'' returned a
     bare empty list and the agent concluded the string appears nowhere.
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
