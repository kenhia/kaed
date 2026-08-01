# kaed

**Ken's Agent Editor** — an editor whose only user is an AI agent. kaed is a
Rust daemon that exposes reading, searching, and editing files as an HTTP MCP
server: versioned reads, atomic multi-file edit transactions, structured
conflicts instead of silent corruption, and a durable attributed journal. It
exists first for remote editing (an agent on one machine editing files on
another, where ssh-piping and network mounts fail in silent ways), with a
long-shot second act as a local power tool.

Status: **planning** — see [`sprints/planning/summary.md`](sprints/planning/summary.md)
for the one-page overview and pointers to the MCP contract, design overview,
architecture sketch, and roadmap.

It lives alongside the other homelab MCP services (klams for memory, korg for
work items): same transport conventions, same per-agent bearer-token auth.

## Development

Uses the kproject minimal harness: `just` for recipes, `just check` for the
CI gates (fmt, clippy, tests), sprint records under `sprints/`.

## License

MIT — see [LICENSE](LICENSE).
