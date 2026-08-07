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
attributed journal on each host that runs it (today: kai and kubs0).

**Status: v0 live on kai + kubs0** (sprints 001–007, 2026-08-07):
`roots`/`stat`/`list`/`read`/`search`/`edit` over streamable HTTP with
bearer auth; journal records successes *and* failures; history read tools
not yet. **kubsdb deliberately has no instance** (korg #929) — if you find
none there, that is correct, not a broken rollout. The fleet installs a
published bundle from the package store (005) — **no host but kai has a
checkout**, so any instruction to `git pull` on a host is wrong.

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
  needs one too — see `src/deny.rs` and R7 in the contract.
- No exec/shell tool and no git tool in the MCP surface — by design; see
  "What kaed is not" in `sprints/planning/overview.md`.
- **`search`/`list`: `glob` is matched against ROOT-relative paths and is
  not re-anchored by `path`** (korg #1066). With `path: "ai/kaed"`, a bare
  `glob: "README.md"` matches nothing — use `ai/kaed/README.md` or
  `**/README.md`. Since 007 both tools report `files_searched` /
  `entries_scanned` and a structured `reason`, so a zero of this shape
  explains itself; before that it was indistinguishable from a real
  no-match, and it produced a wrong conclusion in sprint 006.
