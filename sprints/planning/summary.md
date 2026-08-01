# kaed — planning summary

*One page. Read this first; the other planning docs expand on it.*

**kaed** ("Ken's Agent Editor") is an editor whose only user is an AI agent.
It has no UI. It runs as a small Rust daemon on each homelab host and exposes
its entire editing surface as an **HTTP MCP server**, so that a remote agent —
primarily Desktop Claude on cleo — can read, search, and edit files on that
host with guarantees the current alternatives (base64-over-ssh, rclone
mounts) cannot give: verified writes, atomic multi-file edits, staleness
detection, and token-shaped reads.

## The bet

An editor for agents is not a stripped-down human editor — it is a different
artifact. Human editors optimize a perception–action loop across a screen:
viewport, cursor, keystrokes, visual feedback. An agent has none of that. It
has a stale snapshot of the file in its context window, a per-token cost on
everything it reads, no eyes to verify a write landed, and a session that can
die or be compacted at any moment. So kaed optimizes for:

1. **Verifiability** — every mutation declares the file version it was
   computed against and returns proof of what changed. Silent failure and
   silent corruption are designed out, not tested out.
2. **Token economy** — reads are shaped (ranges, windows, outlines) and
   budgeted; the agent never re-reads 500 lines to confirm a 2-line edit.
3. **Statelessness + durable history** — the protocol needs no session
   affinity; the journal (who changed what, when) outlives every agent
   session and is queryable by successors.
4. **Structure awareness** — address "the body of `fn apply`" via
   tree-sitter, not just line numbers that rot after every edit.
5. **Feedback-driven evolution** — agents using kaed can file friction
   reports through kaed itself; the contract evolves from real usage.

## Primary use, secondary hope

Primary: deployed on kai, kubs0, kubsdb (possibly ksandbox) for remote
editing from Desktop Claude. Secondary, unproven: if the model is right, a
local kaed becomes a power tool agents reach for when built-in editing tools
fall short (atomic refactors, structural edits) — a possibility, not a goal.

## Documents

| Doc | What it holds |
|---|---|
| [overview.md](overview.md) | Vision; how an agent editor fundamentally differs from a human editor; design principles; non-goals; open questions |
| [mcp-contract.md](mcp-contract.md) | Draft v0 tool surface: names, parameters, results, error model, worked examples |
| [architecture.md](architecture.md) | Rust implementation sketch: crates, modules, storage, deployment, security |
| [roadmap.md](roadmap.md) | Now / Next / Later |

## Status

Planning only (2026-08-01). Repo is a fresh kproject scaffold; `src/` is the
`cargo init` stub. Nothing here is implemented. First sprint is a walking
skeleton — see roadmap.
