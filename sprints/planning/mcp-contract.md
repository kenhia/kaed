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
  `internal`, and `unsupported_capability` (live since peer mode, 010:
  a call routed to a root whose host lacks that capability — the
  union-of-capabilities rule paying off honestly at call time).
  `data` makes the error actionable: `version_conflict`
  carries `{expected_version, actual_version, delta}` (a compact diff of
  what changed since the agent last looked); `ambiguous_anchor` carries the
  candidate line numbers; `denied` carries `{path, rule, reason, hint}` —
  `reason` names the refusing policy layer (`server_denylist` |
  `kaedignore` | `in_file_marker` | `classified_opaque` |
  `kaedignore_protected`) and `hint` says what to do instead, because a
  refusal with no alternative is how an agent routes around kaed via ssh
  and the journal loses the edit too. (Peer-mode refusals ride the same
  code with `no_peer_credential` / `peer_credential_rejected` — R10.)

  **The OS is the fifth refusing layer (014).** Unix ownership refuses
  too, and used to do it as a bare `internal` / `Permission denied (os
  error 13)` — the one error shape in the contract carrying no recovery
  data, which is precisely what this posture exists to prevent. It now
  rides the same `denied` code with
  `not_readable_by_service_identity` / `not_writable_by_service_identity`,
  and its `data` adds `service_identity` (the uid kaed runs as), `owner`
  (uid/gid/mode of whatever refused), and `root_advisory` — the addressed
  root's own `description`, verbatim, so a host that records where its
  rendered files are managed from repeats it at the point of failure
  instead of only in `roots`. Two consequences follow from *how* kaed
  writes — a staged temp file plus a rename: **writability is a property
  of the containing directory**, so a root-owned `0644` file inside a
  writable directory really is editable through kaed while a file whose
  directory is root-owned is not, whatever its own mode says; and
  `dry_run` **probes that for real** rather than answering the content
  question and presenting as if it answered the environment one. Same
  probe, same refusal, on the dry and the real path.

  Errors that are plausibly **kaed's** fault also carry
  `data.feedback_invite` — a one-line ask, no round-trip, no standing
  invitation to ignore (see `feedback`). Narrow on purpose: `denied`,
  `too_large`, `internal`, and `version_conflict` only when there was no
  delta to give. A malformed-input error whose message already names the
  valid fields is not friction worth interrupting for, and prompting on
  everything is how a feedback channel dies.
- **R7 — the deny list is absolute, and never silent.** Some paths are
  refused inside the roots: kaed's own config and journal directories
  always, plus configured globs (credential *stores*: `.ssh`, `.aws`, … by
  default), plus any in-repo `.kaedignore` (gitignore-shaped; each file's
  `!` negation can un-deny only what that same file denied — policy layers
  only ever add restriction, and `.kaedignore` itself is readable but
  never writable through kaed), plus an in-file marker: the bare token
  `kaedignore` on a comment line in the first 5 lines. Addressing a denied
  path is `denied` — a permanent refusal, not a path to correct, so it
  should never be retried; `data.reason` says which layer (R4).
  Enumeration (`list`, `search`) omits denied paths instead of failing,
  and reports `denied_hidden: N` so a filtered result is never mistaken
  for the whole directory. `classified_hidden` and — since 014 —
  `unreadable_hidden` are its siblings under the same rule: an entry the
  OS would not let kaed enumerate or open is skipped and **counted**,
  never fatal and never silent. (One `drwx------` directory used to kill
  an entire `search`; kept separate from `denied_hidden` because the
  remedies differ — one needs a human to change config, the other a
  chmod, a different identity, or nothing at all.) The rule is
  **addressed → refuse, enumerated → skip and count**: a path the caller
  *named* gets the structured refusal, and only what the walk *found* is
  counted, because a zero with a count beside it and no cause is the same
  unhelpfulness one level up.

  All three counters describe **what the enumeration actually reached**, so
  when `truncated` is true they are lower bounds: the walk itself runs to
  exhaustion, but anything discovered by *opening* a file (unreadable,
  classified, in-file marker) is only counted as far as the budget allowed.
  The partial result already says it is partial — reading a counter without
  its `truncated` is the caller error, and it is one worth naming, because
  it produced three different counts for one root across three sessions in
  014's live test. Path checks are lexical, applied identically
  to
  paths that exist and paths that don't, so a `denied` is never evidence
  that a file is there (the in-file marker is necessarily content-level:
  it is checked wherever content is opened — read, edit, search — and is
  invisible to `list`, which opens nothing).
- **R9 — heuristics classify; only explicit config denies.**
  Secret-bearing *file* heuristics (`.env*`, `*.env`, `*.pem`, `id_*`,
  `credentials*`, `*.kdbx`, plus `[security] classify` globs) mark a path
  **classified** rather than denying it. A classified file that parses as
  strict dotenv (every line blank, `#` comment, or `KEY=value`) is served
  **redacted**: values become sealed placeholders
  `⟨kaed:KEY@digest⟩` — digest = truncated BLAKE3 of the value, the same
  primitive and truncation as `version` (PD-2) — with the digest withheld
  below an entropy floor (`⟨kaed:DEBUG⟩`), and secret-shaped tokens in
  comments redacted too. The redacted rendering is **line-for-line** with
  the raw file, so ranges, windows, anchors and search hits keep their
  meaning; the `version` returned is always the *raw* file's (R1) and is a
  valid edit base. A classified file that is not dotenv-shaped refuses
  with `reason: classified_opaque`. Every derived surface is redacted or
  withheld — the returned diff, the `version_conflict` delta, search hit
  text, and journal blobs — and a write that would destroy a value
  (delete, or overwrite by a new literal) refuses unless the key is named
  in the edit's `drop_keys`: the value may never have been seen and kaed
  keeps no recoverable shadow. To *use* a value, source the file in a
  shell (`set -a; . .env; set +a`); to manage one, R11's lifecycle never
  needs plaintext, and `secret_reveal` (011, shipped minimal after the
  pressure measurement came back empty) is the separately-permissioned
  escape hatch.
- **R11 — the secret lifecycle never discloses, and every disclosure is
  journaled (011).** The `secret` tool (describe / generate / rotate /
  occurrences) returns placeholders, digests and handles — never a value:
  kaed mints server-side from a **closed** shape grammar (`hex(N)`,
  `base64url(N)`, `uuid4`, `prefixed(tag,inner)`, plus named
  `[secrets] shapes` entries), so an agent can create and rotate a token
  it has never seen. `secret_reveal` is deliberately its **own tool** —
  harness permissioning is per-tool, and that split is the load-bearing
  gate — one key per call, `intent` required, refusable host-wide with
  `[secrets] allow_reveal = false` (`denied` / `reveal_disabled`).

  A **secret handle** is `{root, path, key, digest}` — host-qualified
  location plus the PD-2 BLAKE3 digest, persisted in the file itself
  (never a second store; R1's rule applied to values). Location is what
  you *resolve* (current value); digest is what you *verify* (exact
  value); a digest that no longer matches **fails loudly**
  (`digest_mismatch`) rather than silently resolving to current. A
  handoff carries the handle, never the value.

  `edit` consumes handles via `env_set.value_from`. Across hosts, the
  bytes move kaed-to-kaed under the caller's identity — through gateway
  memory, never through the agent's context (PD-3, Ken's accepted risk) —
  and are zeroed after use. Every generate / rotate / reveal / transport
  lands in the **secrets audit stream** (`journal` kind `"secret"`;
  `coverage.secrets_from` dates its beginning): events, digests and
  *claimed* destinations, never payloads. The gateway journals nothing it
  proxies (010 D-7); the source host journals the value's exit, the
  target its redacted write. Cross-host rotation (`rotate.also` on
  another host's root) is **not atomic** — the response reports
  per-target outcomes.
- **R12 — writes are checked for leaking secrets (012).** kaed is the
  choke point for writes, and the higher-frequency real incident is a
  secret written *into* a README, a fixture, a doc — so every write to an
  **unclassified** file is scanned for newly-introduced secrets
  (classified files are exempt: they are where secrets belong, and
  already redacted, guarded and journaled). Three tiers:
  - **Known digest** — the token's BLAKE3 digest is in the host's
    `secret_digests` index (a secret kaed has *seen*: served redacted,
    journaled redacted, minted, or audited). Refuses, and the refusal
    names the variable to reference instead of the value.
  - **Provider prefix / private-key armor** (`sk-ant-`, `ghp_`, `AKIA`,
    `-----BEGIN … PRIVATE KEY-----`, …) — externally precise. Refuses.
  - **Secret-shaped** (the high-entropy heuristic) — false-positives by
    nature, so it **warns and applies**, never blocks.

  A refusal is `invalid_input` with `data.reason: "secret_leak"`, the
  match list (kind, line, source location — never the token itself), and
  the exact `allow_secrets` override to pass if writing the literal is
  deliberate — the `drop_keys` shape applied to leaks. Only
  newly-introduced tokens trip: a file already containing a token stays
  editable, including the edit that removes it. Every detection lands in
  the secrets audit stream (`leak_refused` / `leak_flagged` /
  `leak_allowed`), dry runs run the checks but journal nothing, and
  `[secrets] leak_checks = "refuse" | "flag" | "off"` is the host-wide
  lever. **The honest limit:** the precise tier covers secrets kaed has
  seen, not secrets on the host — a value that never passed through kaed
  falls to the heuristic. No walk runs on the write path.
- **R5 — three addressing modes, one engine.** Content **anchor** (unique
  match, robust to line drift), **range@version** (line numbers valid only
  against a declared version), **node** (tree-sitter selector). Identical
  transactional semantics underneath.
- **R6 — durable, attributed history.** Every applied transaction gets a
  journal entry: id, author (from token), timestamp, files + diffstat,
  optional agent-supplied `intent` string, git HEAD of the enclosing repo
  if any. Every *failed* attempt gets one too — failures are the
  interesting record — and so does every `feedback` report.

  The history is **readable through the contract** (`journal`, `diff`,
  `revert`), not only by opening SQLite on the host: a promise redeemable
  by one human is not a promise kept to agents.

  What it does **not** cover, deliberately and disclosed in every
  `journal` response: **reads are not journaled**. Only writes, failed
  write attempts and feedback reach the store. A refused read, a
  truncated one, or an agent quietly giving up leaves no row, so an
  absence of read-side friction in the journal is not evidence of its
  absence in the world.

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

- **R10 — any instance can be the fleet's gateway (010).** An instance
  whose `[peers.<host>]` entries carry a `url` proxies calls addressing
  that peer's roots — same tools, same signatures, no new addressing
  vocabulary: the host prefix R8 introduced *is* the routing key. Rules
  that hold on every proxied call:
  - **Identity survives the hop.** The gateway holds per-author tokens per
    backend (`[peers.<host>.tokens]`, PD-4) and proxies as the caller;
    journal attribution on the target is identical to a direct call. No
    token for the calling author → `denied` with
    `reason: no_peer_credential` — never a borrowed identity. A token the
    backend rejects → `denied` / `peer_credential_rejected`. Peer token
    files reload on SIGHUP, like every other credential (#914).
  - **Patterns are expanded by the instance that was asked (014).**
    Routing reads the host prefix, so a pattern naming exactly one peer
    (`kubsdb:*`) used to forward wholesale — and the peer ran the fan-out
    and answered with *its* `hosts_unavailable`, describing a fleet the
    caller is not in. A `root` containing a glob is now always expanded
    locally and only the resulting concrete peer roots are forwarded, so
    `kubsdb:*`, `kai:*` and `*:*` are one code path. A host the pattern
    excludes by name is not probed and therefore never reported
    unavailable; a host it *includes* still reports an honest gap.
  - **Passthrough is verbatim.** Arguments are forwarded as received
    (routing reads only `root`), results return untouched, and error
    objects pass through whole plus a top-level `root` tag —
    `version_conflict` deltas and `ambiguous_anchor` candidates survive
    the hop by construction.
  - **Reachability is data.** A declared peer that does not answer is
    `not_found` / `host_unreachable` with an observed `since` on the call
    path, and `status: "unreachable"` in `roots` — not a transport
    failure. An in-flight call that times out is `internal` /
    `peer_timeout`, which says the call *may have applied* and to check
    that host's journal — deliberately distinct from a connect failure,
    where the peer provably never saw it.
  - **The failure domain is named.** Gateway down = fleet down through it;
    every host's own URL keeps working, and direct connection is the
    documented fallback, not an emergency improvisation.

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
       "capabilities": ["stat", "list", "read", "search", "edit",
                        "secrets", "history", "feedback"]}
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
- `verified` separates **declared** from **observed**. Since 010 the
  instance probes every routable peer in parallel, under the *caller's*
  credential (PD-4): a peer that answers is `verified: true` with its
  observed `version` and its root entries merged into `roots` verbatim
  (per-root `capabilities` — the union, structurally); a probed peer that
  does not answer is `verified: true, status: "unreachable"` with the
  observed `since` — the check happened, that was its result — and its
  *last-known* root entries stay listed, marked `unreachable`, so the
  namespace survives the outage. `probe` says what happened on this call
  (`ok` / `failed` / `skipped` with a detail such as no URL or no
  credential for the calling author); a peer nothing could check keeps
  `verified: false` and its declaration.
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
  language?, executable, modified, binary, classified?}`
- Cheap staleness probe: one `stat` tells an agent whether its held copy
  (by `version`) is still current. `classified: true` (omitted when false)
  says a `read` will be redacted or refused (R9) before anything is opened.

#### `list`
- **In:** `{root, path?, glob?, depth? (default 1), max? }`
- **Out:** `{entries: [{path, kind, size}], truncated, next_offset?,
  entries_scanned, reason?, denied_hidden?, unreadable_hidden?}`
- Respects `.gitignore` by default (`ignored: true` to include).
- `denied_hidden` counts entries the deny list removed (R7); a hidden
  directory counts once, subtree included. Absent when zero.
- `unreadable_hidden` counts entries the OS would not let kaed enumerate
  (014). Same rule, different remedy — see R7. Absent when zero.
- `entries_scanned` is what the walk produced *before* `glob` — always
  present, including zero. See `search` for why.
- `reason` (see `search`) when nothing matched at all. Absent for an empty
  page past the end: that is already explained by `next_offset`.

#### `read`
- **In:** `{root, path, range?: {start, end}, window?: {line | anchor,
  context (default 10)}, numbered? (default false), max_bytes?}`
- **Out:** `{content, version, range: {start, end}, total_lines,
  truncated, next_offset?, redacted?, dotenv?, usage_hint?, warnings?}`
- Modes: whole file (capped), explicit line `range`, or `window` — N
  context lines around a line number or around a unique anchor string.
  `window` + anchor is the cheap "show me where I'm about to edit" read.
- Whatever the mode, `version` is the whole file's version — immediately
  usable as an edit base.
- Classified dotenv files (R9): `redacted: true`, `content` is the
  line-preserving redacted rendering, and a whole-file read also carries
  `dotenv: [{key, placeholder?, comment?, meta: {len, shape}}]` — the
  typed view — plus `usage_hint` (the shell-source convention). `warnings`
  carries e.g. "classified but not gitignored".

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
  after[]}], truncated, total?, files_searched, reason?, denied_hidden?,
  classified_hidden?, unreadable_hidden?}`
- Ripgrep-grade server-side search. Each match carries its file's
  `version`, so a hit is directly addressable: search → edit with no read
  in between, safely (a stale hit becomes `version_conflict`, not a wrong
  edit).
- Classified dotenv files are searched over their **redacted rendering**
  (R9): keys, comments and placeholders match; a probe for a secret's
  value matches nothing, by construction. Opaque-classified files are
  skipped whole and counted in `classified_hidden` (sibling of
  `denied_hidden`). Hit `version` stays the raw file's.
- Entries the OS refuses — an undescendable directory, an unopenable file
  — are skipped and counted in `unreadable_hidden` (014). One of them used
  to kill the whole call, which made a broad root unsearchable without a
  `path` scope.
- **`files_searched` is always present, including zero.** `0 matches in 41
  files` and `0 matches in 0 files` are different answers, and until they
  were distinguishable a scoping mistake was outcome-identical to a genuine
  no-match — the same class of world-model corruption R3 forbids for
  truncation and R7 for filtering. `denied_hidden` is its sibling.
- **`root` also takes a pattern (010): fleet-wide search in one call.**
  `*:*`, `*:src`, `kai:*` glob over full root names, expanded against this
  instance's roots plus every reachable peer's (live probe, caller's
  credential), each concrete root searched in parallel. The response shape
  changes only for patterns: `fleet: true`, each match tagged with its
  `root`, ONE `max_results` budget across the whole fan-out, and per-root
  reporting in `fanout[]` — each root's own `files_searched` / `truncated`
  / `denied_hidden` / `classified_hidden` / `unreadable_hidden` /
  `reason`, plus `merge_dropped`
  for what the *merge* discarded over budget (top-level `truncated` goes
  true either way). Hosts that could not be searched are named in
  `hosts_unavailable[]` (`unreachable` + `since`, `no_credential`,
  `no_url`, `deferred` + `ref`) — a fleet search is never silently
  narrower than the fleet. A pattern matching no known root gets
  `reason: root_pattern_matched_no_roots` with the known names (#1066's
  rule, fleet edition). Patterns are `invalid_input` on every other tool:
  search is read-only and merge-safe; a pattern `edit` has no honest
  atomicity story.
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
      {"op": "rename", "from": "src/a.rs", "to": "src/b.rs"},
      // dotenv ops (R9) — for strict-dotenv files, classified or not:
      {"op": "env_set", "path": ".env", "key": "KLAMS_TOKEN",
       "value?": "literal or ⟨kaed:KEY@digest⟩ placeholder",
       "comment?": "replaces the attached comment block"},
      {"op": "env_set", "path": "svc/.env", "key": "CLIENT_TOKEN",
       // R11: write by handle — same root resolves in the engine, other
       // roots/hosts resolve at the instance that took the call; `digest`
       // means exact-or-fail-loudly, omitting it means current-value
       "value_from?": {"root?": "kai:src", "path": ".env",
                       "key": "KLAMS_TOKEN", "digest?": "a3f9c2d41b7e5860"}},
      {"op": "env_rename", "path": ".env", "from": "OLD", "to": "NEW"},
      {"op": "env_delete", "path": ".env", "key": "OLD_TOKEN"},
      {"op": "env_reorder", "path": ".env", "keys": ["A", "B", "C"]},
      // regenerate `.env.example`: keys + comments, values stubbed empty;
      // an existing example must be declared in `base` (R11)
      {"op": "env_sync_example", "path": ".env", "example_path?": ".env.example"}
    ],
    "dry_run?": false,
    "return_diff?": true,     // default true
    "check?": false,          // parse-check touched files post-edit
    "intent?": "extract rollback into its own fn",  // journaled
    "drop_keys?": ["OLD_TOKEN"],  // values this edit may destroy (R9)
    "allow_secrets?": ["sk-ant-"] // leak matches this edit may write (R12)
  }
  ```
- **Out:** `{txn_id, files: [{path, old_version, new_version}], diff,
  diagnostics?, applied: true|false /* false = dry run */, warnings?}`
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
  - **Classified dotenv files take only env ops** — text ops over
    redacted content are refused with a hint, because their failure modes
    (an anchor landing mid-placeholder, a truncated placeholder written
    back as a literal) are exactly what the typed ops make
    unrepresentable. A `value` that is exactly a placeholder is
    substituted with the real value it seals (same key keeps it, another
    key's placeholder copies it); a stale digest fails loudly rather than
    silently resolving to the current value. A write that makes a value
    vanish refuses unless its key is in `drop_keys` (R9).
  - **Unclassified files are scanned for leaking secrets** (R12): a
    newly-introduced token matching a known digest, a provider prefix or
    a private-key block refuses with `reason: secret_leak` and the exact
    `allow_secrets` override; merely secret-shaped content applies with a
    warning. `revert` takes `allow_secrets` too — restoring content that
    re-introduces a secret is a re-leak and refuses the same way.

### Secrets (R11)

#### `secret`
The lifecycle — four actions, none of which ever returns a value.
- **In:** `{action: "describe"|"generate"|"rotate"|"occurrences", root,
  path, key, version? (generate/rotate — R2), shape? (generate: required,
  a `[secrets] shapes` name or a spec; rotate: override for undetectable
  values), comment?, also?: [{root?, path, key?, version}], intent?}`
- `describe` → `{key, shape, len, digest?, placeholder?, handle: {root,
  path, key, digest?}, handle_line, rotatable_by_detection}`. This **is**
  `load_secret`: the handle is the durable cross-session reference (PD-3),
  recomputed from the file on every call — nothing to expire. Digest
  withheld below the entropy floor (PD-2), and then equality/staleness are
  deliberately unanswerable.
- `generate` → mints server-side, writes via the ordinary transaction
  engine (journaled, redacted), returns `{txn_id, placeholder, handle, …}`.
  New keys only — replacing a value is `rotate`, so "this destroys a
  value" stays attached to the verb that means it.
- `rotate` → same shape (detected from the current value; undetectable
  refuses without an explicit `shape` — kaed does not guess, and
  provider-issued tokens are deliberately undetectable), new entropy.
  Primary plus same-root `also` targets land in ONE transaction; targets
  on other hosts are proxied writes under the caller's identity,
  reported per-target (`targets[].applied/error`) — not atomic, and it
  says so. Rotation's overwrite needs no `drop_keys`: it *is* the verb.
- `occurrences` → every classified-dotenv entry on this host sealing the
  same value, by digest equality over redacted renderings; feeds
  `rotate.also`. Fleet-wide is deliberately not a second mechanism:
  `search {pattern: <digest>, root: "*:*"}` already answers it (010).

#### `secret_reveal`
The escape hatch, its own tool so the harness prompts for it separately.
- **In:** `{root, path, key, intent (required), expected_digest?,
  transport_destination?}`
- **Out:** `{key, value, digest?, disclosed: true, note}` — the one field
  in the whole surface that carries plaintext, and the response says to
  surface the disclosure to the human.
- Refuses: empty `intent`; unclassified files (read those directly);
  `expected_digest` mismatch (loud, PD-3); the whole tool when
  `[secrets] allow_reveal = false` (`denied` / `reveal_disabled`).
- Always journaled as a `secret` audit event — `transport_destination`
  (set by kaed itself when this reveal feeds a cross-host `value_from`)
  makes it a `transport` row with the claimed destination.

### History

#### `journal`
- **In:** `{root?, path?, author?, since?, kind?: ["txn"|"failure"|
  "feedback"|"secret"], max? (default 20, capped at 200)}`
- **Out:**
  ```jsonc
  {
    "entries": [
      {"kind": "txn", "txn_id": 42, "author": "claude", "time": "…",
       "status": "applied" | "torn", "root": "kai:src", "intent": "…",
       "files": [{"path", "old_version", "new_version",
                  "lines_added", "lines_removed"}],
       "diffstat": [6, 2], "git_head": "…",
       "historical": {"reason": "…", "note": "…"}},
      {"kind": "failure", "failure_id": 7, "author": "claude", "time": "…",
       "root": "kai:src", "paths": ["src/txn.rs"], "code": "version_conflict",
       "message": "…", "expected_version": "…", "actual_version": "…"},
      {"kind": "feedback", "feedback_id": 3, "author": "claude", "time": "…",
       "category": "friction", "summary": "…", "detail": "…", "context": "…"},
      {"kind": "secret", "event_id": 5, "author": "claude", "time": "…",
       "action": "generate" /* | rotate | reveal | transport
                               | leak_refused | leak_flagged | leak_allowed:
                               write-side detections (R12) — path is the
                               write TARGET; key is the source key, or the
                               detector's detail (prefix, armor label,
                               shape) when no key is known */,
       "root": "kai:src", "path": ".env", "key": "KLAMS_TOKEN",
       "old_digest": "…", "new_digest": "…", "disclosed": false,
       "destination": "kubs0:src/svc/.env" /* transport: the CLAIM */,
       "txn_id": 42, "intent": "…"}
    ],
    "truncated": false,
    "records_scanned": 3,
    "coverage": {"txns_from": "…", "failures_from": "…", "feedback_from": "…",
                 "secrets_from": "…", "blob_retention_days": 7, "notes": ["…"]},
    "reason": {"code": "…", "hint": "…"}
  }
  ```
- The cross-session, cross-agent memory: "what happened here recently and
  who did it." A resuming session reads the journal instead of re-deriving
  the world.
- **One merged stream, three record kinds**, newest first. Successes,
  failed attempts and friction reports interleave because the questions
  worth asking span them — "did my last edit fail, and why", "what did
  agents complain about and what were they doing when they did". `kind`
  narrows it; the default is all three.
- **`root` is an optional filter, not an address.** The journal is
  host-wide, and history legitimately names roots that no longer resolve,
  so an unresolvable value filters rather than fails. One exception since
  010, because the records physically live elsewhere: a filter naming a
  *routable peer's* root proxies the whole query to that peer — a proxied
  edit journals only on its target, so that is where the answer is. If
  that peer is not answering, the result is `host_unreachable`, not an
  empty page: an empty result would claim "no matching records" about a
  store that was never consulted.
- **`coverage` states what this history cannot see** — always, not only
  when something is missing. `notes[0]` says reads are not journaled at
  all: only write transactions, failed write *attempts* and feedback reach
  the store, so silence about read-side friction is not evidence.
  `failures_from` is later than `txns_from` on any host that ran kaed
  before failures were recorded, and a window reaching back past it earns
  a second note. Same rule as `files_searched` (R3/#1066) and
  `denied_hidden` (R7), applied to time instead of to files.
- `historical` labels a row whose `root` this host no longer serves (R8's
  corollary), with `reason` `root_no_longer_served` or
  `unqualified_pre_007`. The row is never rewritten and the name is never
  aliased back into existence.
- `reason` explains an empty result whose emptiness is more likely the
  caller's than the store's: `unknown_root_filter`, `no_matching_records`.
- Agent-supplied `intent` and error messages are served through the R9
  redactor: free text reaches the audit trail, and a token pasted into an
  `intent` must not become readable just because history became readable.

#### `diff`
- **In:** `{root, path, from: <version|txn_id|"current">,
  to?: <same, default "current">}`
- **Out:** `{diff, path, from_version, to_version, redacted?,
  from_source, to_source}`
- A **version** is 16 hex chars (R1), which is what makes the selector
  unambiguous with no type tag. A **txn id** names the state that
  transaction *produced* for this path; to see what one transaction did,
  `journal` gives you that file's `old_version` and `new_version`.
- Works for any version the journal still **retains**. Metadata is kept
  forever and content is not (#909), so an old version can be named and
  not rendered: that is a `not_found` carrying
  `reason: blob_expired_or_absent` and the retention window, never a
  silent empty diff.
- `from_source` / `to_source` are `journal_blob` or `working_tree`.
- **Redaction is enforced at the store boundary, not per tool** (R9): a
  blob flagged redacted is served as the rendering it is; a blob *not*
  flagged whose path is classified **today** is redacted on read (pre-008
  journals hold plaintext under paths only classified later); content with
  no redacted rendering is withheld with a marker. The in-file `kaedignore`
  marker (R7) is checked on the journalled content too, so a file that
  opted out and was later deleted has no readable history either.

#### `revert`
- **In:** `{root, txn_id, dry_run?, intent?, allow_secrets?}`
- **Out:** same shape as `edit` (a revert **is** a new journaled
  transaction, never history rewriting) — and it is itself revertible.
- Runs through the same engine and the same versioning contract: `base` is
  the version that transaction produced, so a file touched since fails
  with `version_conflict` and a delta. Never a force-overwrite — a revert
  that bypassed R2 would be a hole in it. The agent resolves via `diff` +
  a fresh `edit`.
- Refuses, with `data.reason`, where kaed cannot honestly undo:
  `root_no_longer_served` / `unqualified_pre_007` (R8's corollary),
  `no_plaintext_history` (the file is classified, so what the journal
  retains is a *rendering*; restoring it would write placeholders into the
  file as literals), `revert_of_create_needs_delete` (undoing a create is
  a delete, and `delete` is not shipped yet), `blob_expired_or_absent`,
  `wrong_root`.

### Meta

#### `feedback`
- **In:** `{summary, category? (default "friction"), detail?, context?}`
- **Out:** `{id, recorded, note}`
- The evolution loop, in-band: an agent that just fought the contract
  files the report *through* the contract, attributed and timestamped.
  Stored beside the transactions and failures it is about, and read back
  with `journal` (`kind: ["feedback"]`); promotion to korg work items is a
  human/agent chore for now (open question in overview.md).
- **One required field.** Anything that costs an agent a thinking step
  loses to finishing the task, so `summary` is the whole obligation and
  `category` defaults.
- **The invitation rides the failure.** Errors that are plausibly kaed's
  fault carry `data.feedback_invite` (R4) rather than relying on an agent
  remembering a tool exists — the report worth having comes from the
  session that hit a wall, which is the session least likely to volunteer
  one. It is still callable unprompted, and should be: the worst incident
  on record was a *successful* call that answered wrong, and no
  error-triggered channel can see that class.
- Free text is redacted (R9) before storage. The likeliest thing pasted
  into a friction report is the error that caused it.

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
| ~~`secret_reveal`~~ | the 008 measurement found zero live pressure, so it shipped **minimal**: one key, required intent, always journaled, config kill-switch (R11) | shipped in 011 |
| git operations | git-over-ssh works; journal records `git_head` for correlation | never (correlation only) |
| `apply_patch` (unified-diff input) | overlaps `edit` ops; wait for demand | Next, if dogfooding misses it |
| LSP semantics (rename-symbol, references) | heavy lifecycle; tree-sitter first | Later |
| leases / locks | optimistic versioning until real contention observed | Later, if ever |
| MCP resources / subscriptions | tools-only keeps v0 small (matches klams/korg) | Later |
| binary & non-UTF-8 editing | rare need, big complexity | Later |
