<!-- kproject:begin — managed by kprojects; do not edit inside this block -->
## kproject conventions

This project uses the kproject minimal harness
(<https://github.com/kenhia/kprojects>). Keep context small; prefer doing
over ceremony.

### Layout

- `sprints/` — the project's evolution, one record per PR-sized unit of
  work (a "sprint")
  - `planning/` — planning docs; at minimum `roadmap.md` (the general plan)
  - `review/` — more formal reviews as the project matures
  - sprint records: `###-<short-name>.md` for small projects, or a
    `###-<short-name>/` directory of files for larger/more formal ones
  - a sprint record is one informal narrative: goal, decisions, what
    shipped, follow-ups — written during the sprint, not after
  - projects that deploy end the record with a `## Deployed` section:
    what shipped, where, when, and what was verified live — appended
    after the deploy, not predicted before it
- `docs/` — project documentation, architecture, usage
- `.scratch/` — git-ignored scratch space for user or agent ephemera;
  use it instead of /tmp
- `justfile` — dev recipes; default recipe is `@just --list`; `just check`
  runs the CI gates; `just deploy` (or variants) if the project deploys
- `.env` — git-ignored; tokens and environment vars

### Workflow

- One sprint ≈ one PR. Sprint proposals and work items are managed in
  `korg`; durable cross-project knowledge goes in `klams`.
- Mark each work item resolved as its work completes — don't batch the
  resolutions into sprint-ship. A proposal's progress should be readable
  while the sprint is running, which is the only time it is useful.
- If the korg or klams MCP tools are unavailable in your session, say so
  up front — don't silently work around missing infrastructure.
- TDD preferred: write the failing test first when practical.

### Tooling preferences

- Rust managed by `cargo`; format with `cargo fmt`, lint with
  `cargo clippy --all-targets` (test targets included deliberately — a gate
  that skips them is a gate that lies)
- Mirror `rust-toolchain.toml`, `rustfmt.toml` and `clippy.toml` from a
  sibling homelab repo rather than generating them
- License is MIT unless specifically directed otherwise
<!-- kproject:end -->

## Project

kaed — "Ken's Agent Editor": an editor whose only user is an AI agent. No
human UI. A Rust daemon exposes reading/searching/editing files as an HTTP
MCP server so remote agents (primarily Desktop Claude on cleo) get verified
writes, atomic multi-file transactions, staleness detection, and a durable
attributed journal on each host that runs it (today: kai, kubs0, kubsdb).

**Status: v0 live on kai + kubs0 + kubsdb** (sprints 001–025):
`roots`/`stat`/`list`/`read`/`search`/`edit` plus
`journal`/`diff`/`revert`/`feedback` over streamable HTTP with
**identity-only auth** (declared name; no bearer exists since 024),
serving **MCP `2026-07-28`** fleet-wide since 016 (`0.1.0-af51376`) —
see the protocol bullet below. kubsdb joined in 013 after two sprints deferred (korg #929) — see
the 013 bullet below for its access model. The fleet installs a
published bundle from the package store (005) — **no host but kai has a
checkout**, so any instruction to `git pull` on a host is wrong.

**kai is the fleet's gateway since 010** (R10 in the contract;
`sprints/010-gateway-peer-mode/decisions.md`): calls to kai addressing
peer roots (`kubs0:*`, `kubsdb:*`) are proxied *as the caller* — since 023
by **forwarding the caller's declared name**, with no credential held on
the gateway at all (PD-10; the `[peers.<host>.tokens]` table and
`no_peer_credential` are retired, and the only config a gateway adds is
that each backend's `[auth]` must list the identities arriving proxied).
Results and errors pass
through verbatim plus a `root` tag on errors, `roots` probes peers live
(an outage is `status: "unreachable"` + `since`, data not failure; a
backend that *answered* and declined the forwarded name is
`identity_refused`, not unreachable), and
`search` takes root patterns (`*:*`) for fleet-wide search with per-root
`fanout` reporting. Direct per-host URLs remain
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
  instead of an error — and **`grep -m1`**, because the key appears twice,
  so without it you get a two-line variable and every URL built from it is
  malformed; curl reports that as `000`, or as a plausible-looking HTTP
  status, never as a parse error. Cost a round of chasing a nonexistent
  auth failure in 018).
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
- **The protocol revision is stated, never defaulted** — 015 capped kaed at
  `2025-11-25`, 016 lifted the cap to `2026-07-28`
  (`sprints/01{5,6}-*/decisions.md`; korg #1212, #1214). rmcp's defaults
  advertise every revision the *SDK* knows, which is a claim about rmcp; kaed
  serving it as a claim about itself is what left Claude Code ≥2.1.227
  connected with **zero tools** (it validated `tools/list` against
  `2026-07-28`, whose SEP-2549 `ttlMs`/`cacheScope` kaed did not emit).
  015 is history now — `clamp_protocol_middleware` is **gone** (016 D-3), so
  don't go looking for it. Four things not to re-derive: the gap really was
  **two fields on one result**, fixed by hand-writing `list_tools` (the same
  "macro only generates it when absent" trick `call_tool` uses), and rmcp
  already implements the revision's other server-side pieces; those fields are
  emitted **only to peers that negotiated 2026-07-28+** (016 D-1 — a
  `2025-11-25` peer is entitled to `2025-11-25`'s shape, the same reason rmcp
  strips `resultType` for legacy peers); **`fleet::PEER_PROTOCOL_VERSION` is
  pinned BELOW what kaed serves, at `2025-11-25`, and that is not a mistake**
  (016 D-2 — rmcp 3.1.0's *client* cannot drive a 2026-07-28 session, and the
  gateway proxies through one, so an rmcp bump promoting `LATEST` would break
  every proxied call on kai; `an_rmcp_client_cannot_yet_drive_2026_07_28`
  fails when that stops being true, which is the signal to raise it); and
  `get_info` still pins its fallback to `PROTOCOL_VERSION` explicitly (015
  D-3), which is load-bearing on its own, not a leftover of the cap.
  **R10's "passthrough is verbatim" means content, not protocol framing**
  (016 D-4): the two ends of a proxy hop sit on different revisions *by
  design*, so `proxy_to_peer` stamps an absent `resultType` when the serving
  session is 2026-07-28+ — returning a peer envelope unadapted made the
  client reject the result whole, so a refusal and a success were
  indistinguishable. Found only by the live test from cleo, because the
  failing combination — a 2026-07-28 client making a *proxied* call — is
  unreachable from an rmcp client. **The general rule that cost two rounds
  to learn: a green rmcp-client test says nothing about 2026-07-28
  behaviour.** Anything about this revision, or anything the gateway does to
  a response, needs raw JSON-RPC (`tests/gateway.rs` and `tests/http.rs`
  have the exact `_meta` + header combination).
  016 is slice 1 of **korg program 1220**, which spans all three homelab rmcp
  servers — korg-mcp (#1215) and klams-mcp (#1216) escaped only because their
  rmcp still tops out at `2025-11-25`, and D-2 is the trap most likely to bite
  them.
- **The feedback loop has a reader since 017** (`sprints/017-feedback-triage/
  decisions.md`): the `triage-feedback` skill reads `feedback` on every host,
  triages what is new, and **files the WIs itself — Ken is not the gate**
  (D-1; a loop that asks him to rule on each report re-creates the bottleneck
  kaed exists to remove). State is `docs/feedback-triage.md`: per-host
  high-water marks plus one line per report, dispositions only, never the
  verbatim prose (D-2). Read it with `scripts/feedback-dump.py`, not by hand
  — `immutable=1` reports an **empty table** rather than an error, because
  the rows live in the WAL. Three things not to re-derive: **`feedback`
  takes no `root`, so it never proxies** — a report lands on whichever host
  served the connection, which is why every one so far is on kai including
  the ones about kubsdb; the modal disposition is **"already fixed"** (three
  of the first five), so check that before anything else, but verify against
  current `main` regardless, because #1231 was filed in 013 and still
  reproduces at 016; and **`with_feedback_invite()` is attached only in
  `kaed_error_result`**, so the invite fires on errors alone — the
  bug-heavy category mix is a fact about that placement, not a verdict on
  the contract (D-4). Whether that leaves a real gap is #1233, an
  experiment, not a settled finding.
- **kai and kubs0 are kaed clients since 018** (`sprints/
  018-client-wiring-per-host-authors/`; korg #1350). Both register one MCP
  server named `kaed-kai`, and **kubs0's points at kai, not at its own
  kaed** — kubs0 is a plain backend with no peer tokens, so a localhost
  entry would serve only the two roots that host already edits natively.
  It takes cleo's exposure knowingly: kai down means kubs0 has no kaed at
  all, and the fallback is kubs0's own direct URL. **PD-7 is the identity
  rule** — one author per *machine* (`claude-kai`, `claude-kubs0`; cleo
  keeps bare `claude`), never per harness or per human, because the
  credential is a file on a host and that is what gets revoked. Three
  things not to re-derive: **credentials grow as authors × endpoints**
  (PD-4 means the gateway holds a token per *(author, backend)* pair, so
  two authors cost six files; the fleet is at nine with no inventory —
  `krot` doesn't know about kaed yet); **a backend's `[auth]` must list
  authors that never dial it**, since they arrive proxied, and `config.rs`
  refuses to start on the converse; and **adding an identity is a RESTART,
  not a SIGHUP** (D-3 — `AuthState` captures the `[auth]` spec at startup
  and `reload()` only re-reads the token files it already knows, so the
  symptom is a `401` that reads as "wrong token"). The live test found
  nothing and went beyond its gate: `bin/apply kai kaed-service` was run
  for real, leaving the config byte-identical, so korg #1072's preservation
  fix is now observed rather than inferred.
- **019's rotation machinery is GONE, retired by 023** — don't go looking for
  `kaed-new-token`, `prev_token_file`'s grace window, or
  `identities_without_grace_window`. 019 is history, and it was not wasted:
  making nine credentials legible enough to count is what fired PD-4's own
  escape clause.
- **Identity is a declared name since 023** (PD-10 + R13 in the contract;
  `sprints/023-forwarded-identity/decisions.md`; korg #2392, #2393, slice 5
  of program 2440). `[auth]` is an **allow-list**, not a token table: a
  caller sends `X-Homelab-Agent: <name>` and an unknown name is a 401. The
  gateway **forwards the caller's own name** to peers and holds no
  credential, so `[peers.<host>.tokens]` and `no_peer_credential` are gone —
  PD-4's identity fidelity survives, only the proof changed, and the fleet's
  nine (backend × author) credentials became zero. The caller's tailnet node
  is recorded beside the identity on every journaled mutation and secrets
  audit row (`src/whois.rs`, same measured shape as klams sprint 049 —
  `X-Forwarded-For` first, socket peer as fallback, `unknown` for every
  failure mode). Six things not to re-derive:
  **a declared name never falls through to the bearer** (D-1 — an unknown
  name is refused, because authenticating a caller as something it did not
  claim to be is worse than refusing it; whitespace is not a declaration and
  does fall through);
  **the transition window WAS the token rows and is now closed** (D-2 — it
  was never a flag; 024 deleted the rows from the structs, so don't go
  looking for `token_file`, `token_env`, `prev_token_file` or
  `[peers.*.tokens]` — see the 024 bullet);
  **an identity outlives its token** (D-6 — `resolve_identities` used to drop
  an identity whose token would not resolve, which under declared identity
  would have deleted every agent at the moment the cutover removed the files);
  **`[whois]` is top-level, not `[auth.whois]`** (D-7 — kaed's `[auth]` is a
  map of identity *names*, so klams's placement would collide with an
  identity called `whois`); and **node enforcement is opt-in and off** (D-5 —
  there is no proof of origin under declared identity, so a "gateway
  allow-list" not backed by an origin check is decoration; `nodes` +
  `[whois] enforce` is the only real mechanism, and a *proxied* call resolves
  to the **gateway's** node, not the client's).
  The node reaches the journal via `Journal::as_node()`, a per-request
  recorder view — the bare `impl TxnRecorder for Journal` records `unknown`,
  so a call site that forgets to wrap fails **silently**; that is what the
  `the_node_scoped_recorder_stamps_and_the_bare_one_does_not` gate test is
  for. Also here: **KP-5 is settled and superseded by KP-6** in the
  cross-project `karc+` plan (the grain stays the machine; the leg-toolbox
  credential question dissolved rather than being answered), and **PD-11**
  says `/etc/klams` does **not** become a kaed root — measured, kaed runs as
  `ken` and cannot read it at all, so the root would promise a redaction the
  OS refuses first.
- **The bearer path is GONE since 024** (`sprints/024-identity-only-auth/
  decisions.md`; korg #2471, #2490, slice 7.7 of program 2440). A declared
  name is the only credential: `token_eq`, `Identity.token`,
  `AuthEntry.{token_env,token_file,prev_token_file}`, `PeerConfig.tokens`,
  `PeerTokenEntry`, `resolve_peer_tokens`, `fleet::PeerTokens`,
  `Peers::token_for` and `AuthState.{peer_tokens,peers_spec}` are all
  deleted — don't go looking. Four things not to re-derive:
  **a config still naming a retired field will NOT start, and that is the
  design** (D-1 — `deny_unknown_fields` plus `install.sh` never rewriting a
  config means a surviving credential row would otherwise be invisible;
  `config::retired_fields` scans the raw text *before* serde so the refusal
  names the field, the sprint and the fix, and it strips comments and matches
  longest-first because `prev_token_file` contains `token_file` — both
  pinned by test, and `deploy/config.example.toml` passing `check-config` is
  the live proof of the comment case);
  **`Authorization` is still READ, for the 401 diagnostic only** (D-2 — it
  authenticates nothing; an `Authorization` header with no name is a third
  401 case whose body names the header to send and the one to drop, because
  three Copilot configs sat silently 401 for a day and "sent a retired
  credential" was indistinguishable from "sent nothing". The challenge still
  says `Bearer` because a 401 MUST carry `WWW-Authenticate` and no
  registered scheme describes declaring a name);
  **Copilot is `ghcp-<host>` and klams's bare `ghcp` is NOT a conflict**
  (D-3 + the PD-7 addendum — measured: Claude Code declares `claude` to
  klams from every host and `claude-<host>` to kaed, because klams names the
  *application* and kaed names the **machine**. 023's node field does not
  make the per-host name redundant: a proxied call resolves to the
  *gateway's* node, verified live as `author=ghcp-kubs0, node=kai`, so the
  author name is the only thing distinguishing a Copilot edit from kubs0
  from one from cleo. **korg:2470 therefore needs a per (service, host)
  identity value**, not one per host);
  and **SIGHUP survives and now changes nothing** (D-4 — kept because it is
  a documented signal in an *installed* unit file; every `[auth]`,
  `[whois]` and `[peers]` change is a restart, and the test pins that a
  reload neither drops an identity nor invents one).
- **A forwarded call never fans out, since 025** (R10's hop bullet + **PD-12**;
  `sprints/025-hop-guard-and-session-leak/decisions.md`; korg #2587, #2588).
  Every session a gateway opens to a peer carries `X-Kaed-Hop: <forwarding
  host>`, and a call arriving with it answers from local knowledge — `roots`
  probes nobody, a root pattern expands over local roots only, and the peers
  it skipped are still reported with a `detail` naming the hop. Before it, the
  symmetric three-way mesh (every host declares every other) made one `roots`
  call recurse until something timed out, and it filled all three 1024-fd
  tables in under half a minute; the `No CA certificates were loaded` panic
  and every EMFILE were downstream of the full table, not separate bugs.
  **The star topology was rejected, not overlooked** (PD-12) — it buys
  termination with 018's fallback and leaves the property unenforced, so the
  next `[peers]` edit can recreate the loop. Five things not to re-derive:
  **the marker is not a credential and nothing checks it** (D-1 — there is no
  proof of origin under declared identity, and spoofing it narrows your own
  answer; what it cannot do is add a level to a fan-out);
  **a forwarded call may still serve an addressed root of its own** — only the
  fan-out is refused, because routing reads the host prefix so an addressed
  proxy terminates by construction, and refusing to chain outright would break
  cross-host `secret rotate` (011 D-5);
  **descriptor use is bounded by `peers × authors`** (D-2 — `checkout` holds a
  per-key slot lock across the connect, so concurrent misses coalesce; the
  pre-025 race built one session *per call*, which is what actually filled the
  table. `RunningService` carries a `DropGuard`, so a dropped session did
  cancel — 30 seconds late, when the last `Arc` clone dropped — which is why
  the filed "uncancelled eviction" diagnosis was second-order);
  **`sessions_built` is a counter and the map is not the observable** (D-2 — a
  racing insert *replaces* the entry, so asserting on the map's size passes
  with the bug present; that mistake was made and caught here);
  and **kaed builds its own `reqwest::Client`, once, fallibly** (D-3 — rmcp's
  `from_config` `.expect()`s it per transport, which is the whole of the
  panic; neither of #2537's filed options — pinning a TLS root store, or
  fixing the environment — was needed, because the bundle was always loadable
  and the process had no descriptor to open it with). Also here: EMFILE keeps
  code `internal` and gains `reason: "resource_exhausted"` + `retryable` + a
  hint, classified inside `KaedError::internal` because that is the funnel
  every IO failure already passes through (D-4, on 014 D-1's precedent);
  **"call `roots` first" stays in the instructions** (D-5 — the recursion is
  impossible now, and the advice is how an agent learns host-qualified names);
  and the **inbound** half was left open for a measurement (korg #2589),
  which 026 took — **see the 026 bullet: the premise it was filed on turned
  out to be false**, so don't act on 025's framing of it.
- **Inbound sessions do not accumulate, and rmcp always had the reaper 025
  said it lacked** (`sprints/026-inbound-session-measurement/decisions.md`;
  korg #2589, slice 16 of program 3062). Measured fleet-wide on 2026-09-21
  after eight days of live traffic: **23 / 15 / 14 open descriptors** on
  kai / kubs0 / kubsdb against a 65536 limit. Four things not to re-derive:
  **the idle TTL is `SessionConfig::keep_alive`, defaulting to 300s, on the
  SESSION MANAGER — not on `StreamableHttpServerConfig`** (D-1 — #2589 read
  the server config, correctly found no expiry field, and inferred there was
  none anywhere; `LocalSessionManager::default()` has carried a 300s idle
  TTL and a 60s `init_timeout` since sprint 001, because rmcp has been pinned
  at 3.1.0 that whole time. Reading the struct the constructor takes is not
  reading the configuration); **the 339 descriptors of 2026-09-12 were the
  fan-out outrunning that reaper**, not an absent one (D-2 — a held legacy
  SSE stream shows `lastrcv` under 15s because `sse_keep_alive` is 15s, and
  that is the discriminator: today the two *outbound* peer sockets pinned at
  `2025-11-25` show 6.6s and 10.8s, while all eight *inbound* sockets show
  90–251s, so they hold no stream); **neither filed option ships** (D-3 — a
  custom `session_store` is unnecessary because the knob already exists, and
  flipping `legacy_session_mode` would change protocol behaviour for every
  `2025-11-25` client on the fleet, peer clients included, to fix an
  accumulation that is not happening); and **the negotiated revision is read
  off the wire, not out of a log** (D-4 — kaed logs no revision, and
  `initialize` at `2026-07-28` returns no `mcp-session-id` while
  `2025-11-25` does, which is SEP-2567 and is enough). This also answers
  WI 2587's third acceptance bullet.
- No exec/shell tool and no git tool in the MCP surface — by design; see
  "What kaed is not" in `sprints/planning/overview.md`.
- **`search`/`list`: `glob` is matched against ROOT-relative paths and is
  not re-anchored by `path`** (korg #1066). With `path: "ai/kaed"`, a bare
  `glob: "README.md"` matches nothing — use `ai/kaed/README.md` or
  `**/README.md`. Since 007 both tools report `files_searched` /
  `entries_scanned` and a structured `reason`, so a zero of this shape
  explains itself; before that it was indistinguishable from a real
  no-match, and it produced a wrong conclusion in sprint 006.
