# Roadmap

> The general plan for this project. Keep it current; detail lives in the
> sprint records. Design docs: [summary](summary.md) · [overview](overview.md)
> · [mcp-contract](mcp-contract.md) · [architecture](architecture.md) ·
> [decisions](decisions.md).
>
> **Sequencing for the next nine slices is settled** — korg program 1063,
> reasoned out in [decisions.md](decisions.md) PD-1. The `Now` and `Next`
> items below are covered by it; `Later / Ideas` is not.

## Now

- **k-homelab `kaed-service` advisories assume a checkout** (k-homelab
  #1033) — the recipe itself shipped (korg:932 / #926, done): it asserts the
  binary, the unit and the config keys without owning them, using
  `kaed --version` as a freshness floor and `check-config` as a
  post-condition. But every "fix:" hint in it still says `git pull &&
  ./deploy/install.sh in the kaed repo`, and since 005 the fleet installs a
  published bundle — kubs0 has no repo to pull. Nothing is firing today (both
  hosts are above the floor); it misdirects whoever reads it the first time
  one falls behind.
- **kubsdb** (#929) — deferred by 004, and a "not yet" rather than a "no".
  Wants runtime on kai/kubs0 first, then a *broad*-access design: what
  counts as broad when `/datastore/postgresql` and `/datastore/korg/korg.env`
  sit side by side under a lexical deny matcher, and whether editing a host
  copy whose source of truth is a repo elsewhere is kaed's problem at all.
- **Fleet deployed-ness is not discoverable** (#930) — **closed by 007 +
  010 except one observable.** Root names are host-qualified, the fleet is
  declared in `config.toml [peers]` (PD-5), the root-lookup errors name a
  `deferred` host and its reasoning, and since 010 `roots` *probes*
  routable peers live: declared-vs-observed is real (`verified`, `probe`,
  observed `since`, per-peer version). What remains is 005's second
  observable — what the store's `latest` says versus what each host
  reports — which no probe of the host alone can answer.

## Next

- **Structure:** tree-sitter `outline` + `node_replace` + `check` parse
  diagnostics (rust, markdown, toml first).
- **Act on the feedback channel** — 009 built it; the roadmap's actual
  deliverable is the half after: the first contract revision driven by a
  real agent friction report. An unread channel is worse than none,
  because it looks like a channel.
- **A read log** — the gap 009 accepted deliberately (its D-2) rather than
  inherited. Reads are not journaled at all, so the question this design
  most wants answered — do refusals push agents to ssh? — is one the
  journal structurally cannot answer, and `journal`'s `coverage` block
  says so in every response. Reads vastly outnumber writes, so this
  reopens #909's retention decision; that is why it is not free.
- **Remaining edit ops:** `insert`, `delete`, `rename`; `window`-mode and
  `numbered` reads if not already in 001. **`delete` now has a caller
  waiting**: `revert` cannot undo a transaction that created a file
  without it, and refuses with that reason named.
- **Auth-layer metrics:** per-identity 401 and grace-token counters. 002
  logs both at `warn`; the journal will never see them (401s are rejected
  before the transaction layer). Pairs with the conflict rate from #910 as
  the first thing kmon scrapes off this service.
- **Dogfood report:** a written comparison (round-trips, failures, token
  cost) of kaed vs rclone-mount vs base64-over-ssh on real editing tasks
  — the evidence for whether the bet is paying off.

## Later / Ideas

- **Secrets-aware editing** — `.kaedignore`, redact-and-restore for
  `.env`-shaped files, blind generate/rotate an agent never sees. Nothing
  decided; the thinking is in
  [brainstorm-secrets-editing.md](brainstorm-secrets-editing.md), which
  also argues the honest frame: this is blast radius and ergonomics, not
  access control. Blocks nothing except the history tools above.
- `apply_patch` (unified-diff input) if dogfooding misses it.
- Check hooks beyond parse: fmt/lint/`just check` integration in the edit
  response.
- LSP bridge: rename-symbol, find-references, live diagnostics.
- Leases/locks — only if optimistic versioning shows real contention.
- ksandbox deployment.
- Local (cleo) kaed as an opt-in power tool alongside built-in editing
  tools — the "if we got it right" bet; needs the dogfood report first.
- Per-region versioning for conflict-free disjoint edits.
- MCP resources/subscriptions; metrics & observability; binary/non-UTF-8
  handling; journal → korg feedback promotion automation.

## Done

- **Sprint 010 — gateway peer mode** (2026-08-07). Peer routing turned on:
  a `[peers.<host>]` entry with a `url` and per-author tokens makes any
  instance a gateway — calls addressing that peer's roots are proxied
  **as the caller** (PD-4: journal attribution survives the hop; no
  credential → refused, never impersonated), with arguments and results
  passing through verbatim (routing reads only `root`; a
  `version_conflict` delta crosses the hop intact, plus one `root` tag on
  errors). `roots` probes routable peers in parallel under the caller's
  credential: an answering peer is `verified: true` with its observed
  version and its roots merged in; one that stops answering becomes
  `status: "unreachable"` with the observed `since`, last-known roots
  still listed — unreachable-host-as-data, the brainstorm's win #1.
  `search` gained root patterns (`*:*`) for fleet-wide search under one
  budget with per-root truncation reporting and `hosts_unavailable` named
  rather than skipped. `journal` proxies when its root filter names a
  routable peer (the rows live there — the gateway journals no proxied
  calls, D-7). Peer tokens reload on SIGHUP (#914 extended); contract
  gained R10 and the live `unsupported_capability` code. New
  `tests/gateway.rs` drives two real instances over HTTP. Record:
  `sprints/010-gateway-peer-mode/`.
- **Sprint 009 — history tools and the friction-triggered feedback
  channel** (2026-08-07). R6's promise stopped being redeemable only by
  ssh: `journal` merges applied transactions, failed attempts and friction
  reports into one time-ordered stream (#1049); `diff` reconstructs any
  version the blob store still retains; `revert` undoes a transaction *as*
  a transaction, through the same version checks, and refuses — naming the
  reason — where kaed cannot honestly undo. `feedback` re-shaped per
  #1046: one required field, and the invitation rides the errors that are
  plausibly kaed's fault rather than standing around being ignored. Every
  `journal` response states what the history cannot see; **reads stay
  unjournaled by explicit decision** (D-2), disclosed rather than
  inherited. The 008 gate held, and the gate test earned its keep — it
  caught a live leak nobody had reasoned about: agent-supplied `intent`
  was free text that 008's content-only redaction never covered, and 009
  was the sprint that turned it from write-only into served. Record:
  `sprints/009-history-and-feedback/`.
- **Sprint 008 — the secrets model** (2026-08-07). Classification instead
  of denial: `.env`-shaped files read redacted, edit through typed env
  ops, and every derived surface — diff, conflict delta, search hits,
  journal blobs — redacted with them. Record:
  `sprints/008-secrets-model/`.
- **Sprint 007 — host-qualified addressing and the declared fleet**
  (2026-08-07). Roots became `host:root` (R8), the fleet is declared in
  config and reported by `roots`, and `search`/`list` learned to explain
  an empty result. Record: `sprints/007-host-qualified-addressing/`.
- **Sprint 006 — the first journal evidence pass** (2026-08-06). What
  agents had actually done with kaed, measured rather than assumed —
  including the `search` zero that produced a wrong conclusion (#1066).
  Record: `sprints/006-journal-evidence-pass/`;
  `docs/agent-usage-report-2026-08-06.md`.
- **Sprint 005 — the fleet deploy goes store-native** (2026-08-06). `just
  publish` ships a versioned *deploy bundle* to the homelab package store —
  binary, `install.sh`, unit file, config template, `new-token.sh`, one
  `SHA256SUMS` — and `install.sh --from-store` installs it: no clone, no
  cargo, every file checksum-verified, and the binary must report the version
  it was published under before anything is installed (#1015). Ends the
  build-from-clone model 004 shipped with, which stopped being possible when
  kubs0's checkout was deleted. `new-token.sh` now installs as
  `kaed-new-token`, closing a live gap — kubs0's token could not be rotated
  at all. Nine tests drive the real installer against a static store served
  from the test process, so the failure paths are exercised without
  publishing anything irreversible. Also fixed 004's tailnet extraction,
  which silently returned an empty string. Record:
  `sprints/005-store-native-deploy/`.
- **Sprint 004 — fleet deploy** (2026-08-03). A canonical `deploy/` for
  k-homelab to point at — `install.sh` (idempotent, never overwrites a
  config, never touches a token, keeps a `.prev` for rollback), the unit as
  a real file, the config template, and `new-token.sh` with mint /
  `--rotate` / `--close` (#923). A build stamp: `kaed --version`,
  `check-config` and the MCP handshake all report
  `0.1.0 (<commit> <date>)` instead of a `0.1.0` constant since 001 (#924).
  Per-host roots, one `claude` identity everywhere, `kaed-<host>` client
  naming, and the co-located-URL question settled with evidence (#925 →
  `decisions.md` D1–D4). kai + kubs0 live and verified; **kubsdb
  deliberately deferred** (#929). Two findings the deploy paid for:
  `**/secrets/**` never matches the directory it names, so kai had been
  carrying a rule weaker than it read since 002; and the client-wiring step
  corrupted cleo's `~/.claude.json` with a PowerShell BOM and took out every
  MCP server on that machine (#931 — repaired, guarded by
  `deploy/check-client-config.ps1`, and the reason D5 exists). Record:
  `sprints/004-fleet-deploy/`.
- **Sprint 003 — public beta readiness** (2026-08-02). README, `SECURITY.md`,
  `docs/overview.md` and `docs/setup.md` rewritten for strangers rather than
  for this homelab, plus repo hygiene, ahead of going public.
- **Sprint 002 — blast-radius hardening** (2026-08-02). All six
  sprint-001 follow-ups. Three-layer path deny list enforced in the
  resolver *and* both directory walkers (#908, new `denied` code +
  contract R7); RFC 6750 attributes on 401s (#913); contract R1 now states
  versions are durable content addresses (#915); failed transactions
  journaled, conflict-rate-per-author is one query (#910); blob retention
  made real — GC'd, 7 days, metadata kept forever, DB 0600 (#909);
  SIGHUP token reload with a rotation grace window (#914). kai re-deployed
  on narrowed roots and every exit criterion re-verified live. Record:
  `sprints/002-blast-radius-hardening/`; decisions for #909/#914 in its
  `decisions.md`; contract delta in its `contract-notes.md`.
- **Sprint 001 — walking skeleton** (2026-08-02). The six core tools,
  R1–R4 semantics, atomic apply, journal writes, bearer auth over
  streamable HTTP; deployed to kai (systemd user unit + tailscale
  serve) and verified end-to-end from Desktop Claude on cleo. Record:
  `sprints/001-walking-skeleton/`; contract clarifications in its
  `contract-notes.md`; follow-up WIs #908–#911, #913, #914.
