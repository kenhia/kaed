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
- **Fleet deployed-ness is not discoverable** (#930) — **mostly closed by
  007, deliberately left open.** Root names are host-qualified, the fleet is
  declared in `config.toml [peers]` (PD-5), and `roots` plus the root-lookup
  errors both name a `deferred` host and its reasoning. What remains is the
  *observed* half: kaed declares but does not probe, so every peer entry
  reports `verified: false`. Probing arrives with peer mode (#1050); until
  then this is declared-vs-nothing, not declared-vs-observed. 005 gave it a
  second observable worth folding in — what the store's `latest` says versus
  what each host reports.

## Next

- **History tools:** `journal`, `diff`, `revert` — including a read path
  for the `txn_failures` 002 started writing.

  > **Decide the secrets model first.** A redacted read surface is worth
  > nothing if `journal.db` holds plaintext and these tools serve it. #909
  > settled blob retention *without* secrets-aware editing in view, so that
  > decision does not cover this. See
  > [brainstorm-secrets-editing.md](brainstorm-secrets-editing.md) — take
  > the call, then build these. Shipping the leak and the guard in the same
  > quarter is the failure mode.
- **Structure:** tree-sitter `outline` + `node_replace` + `check` parse
  diagnostics (rust, markdown, toml first).
- **`feedback` tool** — then act on it: first contract revision driven by
  real agent friction reports.
- **Remaining edit ops:** `insert`, `delete`, `rename`; `window`-mode and
  `numbered` reads if not already in 001.
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
