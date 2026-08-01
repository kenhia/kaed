# Roadmap

> The general plan for this project. Keep it current; detail lives in the
> sprint records. Design docs: [summary](summary.md) · [overview](overview.md)
> · [mcp-contract](mcp-contract.md) · [architecture](architecture.md).

## Now

- **Sprint 001 — walking skeleton.** Smallest kaed that beats
  base64-over-ssh: `roots`, `stat`, `list`, `read`, `search`, and `edit`
  with `anchor_replace` / `range_replace` / `create` — full R1/R2 version
  semantics, atomic multi-file apply, bearer auth, streamable HTTP.
  Journal writes recorded (schema in place) but no `journal`/`diff`/
  `revert` tools yet. Deploy to kai as a systemd user service behind
  tailscale serve; wire into Desktop Claude; dogfood on a scratch repo.

## Next

- **History tools:** `journal`, `diff`, `revert` + blob retention.
- **Structure:** tree-sitter `outline` + `node_replace` + `check` parse
  diagnostics (rust, markdown, toml first).
- **`feedback` tool** — then act on it: first contract revision driven by
  real agent friction reports.
- **Remaining edit ops:** `insert`, `delete`, `rename`; `window`-mode and
  `numbered` reads if not already in 001.
- **Fleet deploy:** kubs0 + kubsdb; k-homelab recipe for install/config;
  korg project registration and sprint flow from there.
- **Dogfood report:** a written comparison (round-trips, failures, token
  cost) of kaed vs rclone-mount vs base64-over-ssh on real editing tasks
  — the evidence for whether the bet is paying off.

## Later / Ideas

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
