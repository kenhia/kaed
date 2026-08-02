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

**Status: v0 walking skeleton shipped and live on kai** (sprint 001,
2026-08-02): `roots`/`stat`/`list`/`read`/`search`/`edit` over streamable
HTTP with bearer auth; journal writes recorded, history tools not yet.
The design lives in `sprints/planning/` — read `summary.md` first, then
`mcp-contract.md` before touching server code; where implementation and
the draft contract diverge, `sprints/001-walking-skeleton/contract-notes.md`
records the decisions and feeds the first contract revision.

- Build/test: `just check` (`cargo fmt --check`, `clippy --all-targets
  -D warnings`, `cargo test`) — run and pass it before shipping.
- Read first: `sprints/planning/{summary,overview,mcp-contract,architecture,roadmap}.md`
- Deploy state (kai service, tailscale serve, 401 semantics, client
  wiring): `sprints/001-walking-skeleton/deploy.md`. The tailnet hostname
  is deliberately not committed — placeholder `<tailnet>`; real value in
  klams or `tailscale status`.
- Core invariants (don't design tools that violate them): every content
  response carries a `version`; every mutation declares base versions and
  is atomic; truncation is explicit; errors are structured
  `{code, message, data}`.
- No exec/shell tool and no git tool in the MCP surface — by design; see
  "What kaed is not" in `sprints/planning/overview.md`.
