<!-- kproject:begin — managed by kprojects/install.sh; do not edit inside this block -->
## kproject conventions

This project uses the kproject minimal harness
(`~/src/ai-agents/kprojects`). Keep context small; prefer doing over
ceremony.

### Layout

- `sprints/` — the project's evolution, one record per PR-sized unit of
  work (a "sprint")
  - `planning/` — planning docs; at minimum `roadmap.md` (the general plan)
  - `review/` — more formal reviews as the project matures
  - sprint records: `###-<short-name>.md` for small projects, or a
    `###-<short-name>/` directory of files for larger/more formal ones
  - a sprint record is one informal narrative: goal, decisions, what
    shipped, follow-ups — written during the sprint, not after
- `docs/` — project documentation, architecture, usage
- `.scratch/` — git-ignored scratch space for user or agent ephemera;
  use it instead of /tmp
- `justfile` — dev recipes; default recipe is `@just --list`; `just check`
  runs the CI gates; `just deploy` (or variants) if the project deploys
- `.env` — git-ignored; tokens and environment vars

### Workflow

- One sprint ≈ one PR. Sprint proposals and work items are managed in
  `korg`; durable cross-project knowledge goes in `klams`.
- If the korg or klams MCP tools are unavailable in your session, say so
  up front — don't silently work around missing infrastructure.
- TDD preferred: write the failing test first when practical.

### Tooling preferences

- Python managed by `uv`; lint/format with `ruff`; typecheck with `ty`
  (astral toolchain)
- License is MIT unless specifically directed otherwise
<!-- kproject:end -->

## Project

kaed — "Ken's Agent Editor": an editor whose only user is an AI agent. No
human UI. A Rust daemon exposes reading/searching/editing files as an HTTP
MCP server so remote agents (primarily Desktop Claude on cleo) get verified
writes, atomic multi-file transactions, staleness detection, and a durable
attributed journal on each host that runs it (today: kai, kubs0, kubsdb).

**Status: v0 live on kai + kubs0 + kubsdb** (sprints 001–015, 2026-08-12):
`roots`/`stat`/`list`/`read`/`search`/`edit` plus
`journal`/`diff`/`revert`/`feedback` over streamable HTTP with bearer
auth, serving **MCP `2025-11-25`** (015 — see the protocol bullet below). kubsdb joined in 013 after two sprints deferred (korg #929) — see
the 013 bullet below for its access model. The fleet installs a
published bundle from the package store (005) — **no host but kai has a
checkout**, so any instruction to `git pull` on a host is wrong.

**kai is the fleet's gateway since 010** (R10 in the contract;
`sprints/010-gateway-peer-mode/decisions.md`): calls to kai addressing
peer roots (`kubs0:*`, `kubsdb:*`) are proxied *as the caller* —
per-author peer tokens in
`[peers.<host>.tokens]` on kai (an author without one is refused with
`no_peer_credential`, never impersonated; PD-4), results and errors pass
through verbatim plus a `root` tag on errors, `roots` probes peers live
(an outage is `status: "unreachable"` + `since`, data not failure), and
`search` takes root patterns (`*:*`) for fleet-wide search with per-root
`fanout` reporting. The tokens table is host-local: k-homelab's
kaed-service recipe *preserves* it but never writes it (its sprint 023),
so it is configured by hand per gateway host. Direct per-host URLs remain
the documented fallback — the gateway journals no proxied calls (D-7), so
each host's journal is still the only record of its own edits.

**Root names are host-qualified since 007** — `kai:src`, never `src`, and
the unqualified form is refused rather than aliased. `roots` also returns
the declared fleet from `config.toml [peers]`, which is where the kubsdb
answer above now lives on the host itself (PD-5). `config.toml` is
*installed*, not cloned, so `install.sh` will not add `[peers]` to a config
that already exists — it warns instead, and adding the block is a
deliberate post-deploy step per host.

The design lives in `sprints/planning/` — read `summary.md` first, then
`mcp-contract.md` before touching server code. 002's changes are already
applied to the contract; `sprints/00{1,2}-*/contract-notes.md` record the
reasoning behind them. **Cross-sprint decisions are in
`sprints/planning/decisions.md` as `PD-n`** (distinct from the per-sprint
`D-n`); PD-1 sequences the next nine slices as korg program 1063.

For how the tool is actually used in practice — what agents edit, what
fails, and what the journal structurally cannot tell you — see
`docs/agent-usage-report-2026-08-06.md`; re-run it with
`scripts/journal-report.py`.

- Build/test: `just check` (`cargo fmt --check`, `clippy --all-targets
  -D warnings`, `cargo test`) — run and pass it before shipping.
- Read first: `sprints/planning/{summary,overview,mcp-contract,architecture,roadmap}.md`
- **The repo is going public** (sprint 003). `README.md`, `SECURITY.md` and
  `docs/` are written for strangers, not for this homelab — keep them that
  way. Machine names (cleo, kai, kubs0, kubsdb) are fine to publish; the
  tailnet name is not, in files **or commit messages** — `git grep` and
  `git log -S` both miss messages, which is how one nearly shipped.
- **Deploying is `deploy/install.sh`, not hand-typed steps** — idempotent,
  re-running it is the upgrade path, and it never overwrites a config or
  touches a token. Since sprint 005 the fleet is **store-native**: `just
  publish` puts a versioned deploy bundle in the homelab package store and
  every host installs *that* with `install.sh --from-store` — no clone and
  no cargo on the target (kubs0 has neither). Never deploy from a branch;
  `/sprint-ship` Phase 7 runs the `deploy-fleet` skill (`.sprint-deploy`)
  from merged `main`. Deploy state: `sprints/005-store-native-deploy/
  deploy.md` is current, 004's has the per-host table and the verification
  battery, 002's covers rotation, 001's is still the reference for
  tailscale serve and the rmcp `Host` gotcha. The tailnet hostname is
  deliberately not committed — placeholder `<tailnet>`; real value in klams
  or `tailscale status` (whose `--json` pretty-prints: grep
  `'"MagicDNSSuffix": *"'`, with the space, or you get an empty string
  instead of an error).
- **Never hand-write another application's config file**, especially from
  PowerShell on cleo: `Set-Content -Encoding UTF8` writes a BOM on PS 5.1,
  and that wiped every MCP server on cleo once (korg #931). Prefer the
  app's own tooling; if you must write, verify at the *byte* level, since
  re-reading through the tool that wrote it will happily strip the BOM and
  tell you everything is fine. See `docs/setup.md` §7.
- Core invariants (don't design tools that violate them): every content
  response carries a `version`; every mutation declares base versions and
  is atomic; truncation is explicit; errors are structured
  `{code, message, data}`.
- **The deny list is enforced in three places, not one.**
  `fsops::resolve_existing`/`resolve_creatable` cover *addressed* paths;
  `list` and `search` walk directories themselves and each need their own
  `filter_entry` check. Any new tool that enumerates rather than addresses
  needs one too — see `src/deny.rs` and R7 in the contract. Since 008 the
  `.kaedignore` layer rides the same three places (`src/policy.rs`), and
  the in-file `kaedignore` marker is checked wherever content is *opened*
  (`load_text`, `search`) — `list` cannot see it by design.
- **History is readable through the contract since 009** (`src/history.rs`;
  `sprints/009-history-and-feedback/decisions.md`). `journal` merges
  transactions, failed attempts and `feedback` into one stream — `root` is
  an optional *filter*, not an address. Three things not to re-derive:
  **reads are still not journaled** (D-2, a deliberate gap disclosed in
  every response's `coverage` block — don't "fix" it without reopening
  #909's retention decision); `revert` deliberately refuses historical
  roots, classified files and creates, each with a named reason; and
  **redaction lives at the materialisation boundary** (`history::
  materialise`) so a legacy plaintext blob is redacted on read. Free text
  bound for the journal — `intent`, error messages, feedback — is redacted
  too, at both ends: 008's model covered file *content* only, and `intent`
  leaked straight through it until the 009 gate test caught it.
- **Secrets are classified, not denied, since 008** (R9 in the contract;
  `sprints/008-secrets-model/decisions.md`). `.env`-shaped files read
  redacted (`⟨kaed:KEY@digest⟩` placeholders — BLAKE3 + entropy floor,
  PD-2, never HMAC) and are edited via typed env ops on the `edit` tool;
  every derived surface (diff, conflict delta, search hits, journal blobs)
  is redacted too, and `search` runs over the redacted rendering, so a
  value probe matches nothing by construction. Destroying a value needs
  `drop_keys`; there is no plaintext shadow — do not add one without
  reopening D-11/#1051.
- **The secret lifecycle is 011** (R11 in the contract;
  `sprints/011-secret-lifecycle-and-handoff/decisions.md`): a `secret`
  tool (describe / generate / rotate / occurrences) that never returns a
  value, over a **closed** shape grammar (`src/shapes.rs` — no
  `passphrase`, deliberately), and `secret_reveal` as its **own tool**
  because harness per-tool permissioning is the gate. Three things not to
  re-derive: **`describe` IS `load_secret`** — the handle is
  `{root, path, key, digest}` persisted in the file itself, never a second
  store (PD-3/D-2); **the 008 measurement came back zero**, which is why
  reveal is minimal (one key, `intent` required, `allow_reveal`
  kill-switch) — widen it only with new evidence; and **cross-host
  `value_from` moves bytes kaed-to-kaed** (source host journals a
  `transport` audit event, gateway journals nothing, agent context never
  holds the value). The audit stream is `journal` kind `"secret"`;
  `destination` on transport rows is the caller's *claim*, and D-6 says
  why that is the honest ceiling.
- **Write-side leak detection is 012** (R12 in the contract;
  `sprints/012-write-side-leak-detection/decisions.md`): writes to
  **unclassified** files are scanned for newly-introduced secrets —
  known-digest / provider-prefix / private-key matches refuse
  (`reason: secret_leak`, override `allow_secrets` naming the exact
  match), the entropy heuristic **warns and applies** (D-3: promote it
  only with `leak_flagged` evidence, or it becomes a tool agents route
  around). Three things not to re-derive: the known-digest index
  (`secret_digests` in journal.db) holds **digests only, above-floor
  only**, fed by redacted reads/blobs/secret-events — it is not a read
  journal (009 D-2 intact) and not a plaintext shadow (008 D-11 intact);
  **only newly-introduced tokens trip** (D-1), so a file already holding
  a token stays editable, including the edit removing it; and the precise
  tier's coverage is honestly "secrets kaed has seen", not "secrets on
  the host" — no walk runs on the write path. Host lever:
  `[secrets] leak_checks = refuse|flag|off`.
- **kubsdb's broad access is 013** (`sprints/013-kubsdb-broad-access/
  decisions.md`; live-tested from cleo, `live-test.md` beside it). Roots
  `datastore`/`hvsim`/`src`; the config/data boundary is **lexical shape,
  not service names** — `/datastore/*/data` denied (pinned by a `deny.rs`
  test; globset's `*` crosses `/`, deliberately), the package store denied
  as fleet supply chain, `/gratch` has no root, and rsync deploy targets
  are *told, not denied* via root `description`s. Three things not to
  re-derive: **nothing kubsdb-shaped went into `DEFAULT_DENY`** (D-7 —
  it is per-host config, and 014 D-7 says the same of its classify globs);
  root-owned files refuse at the OS (D-6, accepted then; made legible by
  014); and the live test's findings were all closed by 014.
- **The OS is a named policy layer since 014** (`sprints/
  014-legible-permissions/decisions.md`), closing 013's whole live-test
  tail. EACCES is `denied` with `not_readable_by_service_identity` /
  `not_writable_by_service_identity` — **not a new error code** (D-1:
  `reason` is the field whose job that is). Four things not to
  re-derive: **writability is a property of the containing DIRECTORY**
  (D-2 — kaed stages a temp file and renames, so a root-owned 0644 file
  in a writable dir *is* editable and `access(file, W_OK)` would refuse a
  write that works); the route to the editable copy is the addressed
  root's own `description`, carried back as `root_advisory` (D-3 — so
  never hardcode a host's layout in this public repo, and editing the
  root description on the host changes both the advisory and the hint);
  `dry_run` **probes for real** and shares the write path's probe (D-4 —
  a dry run that used to pass against an unwritable path now fails, and
  that is the fix); and enumeration counts OS-hidden entries as
  `unreadable_hidden`, a third sibling of `denied_hidden` /
  `classified_hidden`, deliberately not folded into them (D-6). Also
  here: root patterns are always expanded by the instance that was asked
  and never proxied (D-5), and kubsdb's config gained five classify globs
  plus a MANAGED root description (#1093) — the `lost+found` interim deny
  entry is **gone**, replaced by the walker fix. **Live-tested from cleo
  through the kai gateway** (`014-legible-permissions/live-test.md`): every
  013 finding closed, no regressions, and the first live test in this
  program to file nothing. Two things it settled that are easy to
  re-derive wrongly: the five classified kubsdb files refuse
  `not_readable_by_service_identity` rather than `classified_opaque`
  (0600 root:root — the OS refuses before kaed can read bytes to classify,
  so the globs are dormant policy that arms itself if a mode ever
  changes); and **the hidden counters are lower bounds when `truncated`**,
  which is why one root reported 1, 2 and 6 unreadable entries across
  three sessions.
- **kaed serves MCP `2025-11-25`, deliberately, since 015**
  (`sprints/015-protocol-version-negotiation/decisions.md`; korg #1212).
  rmcp's default advertises every revision the *SDK* knows — 3.1.0 includes
  `2026-07-28`, whose `tools/list` requires SEP-2549 `ttlMs`/`cacheScope`
  that kaed does not emit — so Claude Code ≥2.1.227 asked for it, got it
  echoed, failed result validation and registered **zero tools**. Three
  things not to re-derive: narrowing `supported_protocol_versions()` is
  **not sufficient alone** (D-2 — rmcp routes on the version *asked for*
  before dispatch, so a 2026-07-28 body takes the sessionless inline
  lifecycle and the client's next call has no session to belong to; the
  handler-only fix turns "zero tools" into "cannot connect"), which is why
  `clamp_protocol_middleware` rewrites the requested version at the HTTP
  boundary; `get_info` pins the fallback to the same constant so an rmcp
  bump promoting `LATEST` cannot reintroduce it silently (D-3); and the cap
  is **tonight's answer, not forever's** — implementing `2026-07-28` is
  #1221, and D-1 says the blocker is verifiability from this repo, not
  effort.
- No exec/shell tool and no git tool in the MCP surface — by design; see
  "What kaed is not" in `sprints/planning/overview.md`.
- **`search`/`list`: `glob` is matched against ROOT-relative paths and is
  not re-anchored by `path`** (korg #1066). With `path: "ai/kaed"`, a bare
  `glob: "README.md"` matches nothing — use `ai/kaed/README.md` or
  `**/README.md`. Since 007 both tools report `files_searched` /
  `entries_scanned` and a structured `reason`, so a zero of this shape
  explains itself; before that it was indistinguishable from a real
  no-match, and it produced a wrong conclusion in sprint 006.
