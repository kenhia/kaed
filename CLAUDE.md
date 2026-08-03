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
attributed journal on each homelab host (kai, kubs0, kubsdb).

**Status: v0 live on kai + kubs0** (sprints 001–004, 2026-08-03):
`roots`/`stat`/`list`/`read`/`search`/`edit` over streamable HTTP with
bearer auth; journal records successes *and* failures; history read tools
not yet. **kubsdb deliberately has no instance** (korg #929) — if you find
none there, that is correct, not a broken rollout. The design lives in
`sprints/planning/` — read `summary.md` first, then `mcp-contract.md`
before touching server code. 002's changes are already applied to the
contract; `sprints/00{1,2}-*/contract-notes.md` record the reasoning
behind them.

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
  touches a token. `/sprint-ship` Phase 7 runs the `deploy-fleet` skill
  (`.sprint-deploy`). Deploy state:
  `sprints/004-fleet-deploy/deploy.md` is current (per-host table, the
  verification battery); 002's covers rotation, 001's is still the
  reference for tailscale serve and the rmcp `Host` gotcha. The tailnet
  hostname is deliberately not committed — placeholder `<tailnet>`; real
  value in klams or `tailscale status`.
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
