# kaed overview — an editor whose user is an agent

## What kaed is

kaed is a text/code editor with no human interface. It is a daemon that owns
the mechanics of editing — reading, searching, mutating, journaling files on
the host it runs on — and exposes those mechanics as MCP tools over HTTP.
The "user" holding the other end of the protocol is an AI agent, usually one
running on a different machine.

The name is "Ken's Agent Editor", but the honest description is an **editing
protocol with a reference implementation**: the interesting artifact is the
contract (what operations exist, what they promise, what they return), and
the Rust daemon is the thing that keeps those promises.

## Why now

Remote editing is currently the weakest link in this homelab's agent
workflow. Desktop Claude on cleo edits files on kai/kubs0/kubsdb through one
of two paths, both with documented failure modes:

- **base64-over-ssh** — quote-safe but whole-file replace only; no surgical
  edits; every write needs a manual `md5sum` verification round-trip.
- **rclone mount** — native tools work, but edits strip the executable bit,
  a hard kill corrupts the VFS cache after which *writes fail while the Edit
  tool still reports success*, and ssh-side changes hide behind the dir
  cache. Silent data loss is a lived failure mode, not a hypothetical.

Both are adaptations of human-file-access machinery to agent use. kaed is
the purpose-built replacement: a first-class remote editing surface whose
guarantees are designed for how agents actually fail.

## How an agent editor fundamentally differs from a human editor

A human editor is a perception–action loop across a screen. Every major
feature — viewport, cursor, syntax highlighting, undo, autocomplete — exists
to serve eyes and fingers on a continuous feedback loop measured in
milliseconds. An agent has a different body entirely, and almost every
human-editor concept dissolves or inverts:

| Human editor | Agent reality | kaed's answer |
|---|---|---|
| Viewport — you *look at* the file | No eyes; a stale snapshot in a context window | Shaped reads: ranges, windows-around-a-point, outlines. Reads are queries, not glances |
| Cursor & selection — position is ambient state | No ambient state; every call must say what it means | Explicit addressing in every operation: content anchors, line ranges pinned to a version, syntax nodes |
| Keystrokes — thousands of tiny cheap mutations | Few, expensive, batched mutations per turn | Edits are transactions: multi-hunk, multi-file, atomic, dry-runnable |
| Visual confirmation — you *see* the edit land | Cannot see; must trust or re-read (tokens) | Every edit returns proof: diff of what actually changed + new version. Verification is in the response, not a follow-up read |
| Undo stack — private, per-session, keystroke-grained | Sessions die, compact, resume; several agents interleave | A durable journal: transaction-grained, attributed, queryable, revertable by any successor session |
| Syntax *highlighting* — color for eyes | Color is meaningless; structure is not | Syntax *addressing*: tree-sitter outlines and node-targeted edits ("replace the body of `fn foo`") |
| Single user at one keyboard | Multiple agents + human tools on the same tree | Optimistic concurrency: every mutation declares its base version; conflicts are structured data, never last-write-wins |
| Free reads — glancing costs nothing | Every byte returned is paid for in context | Token budgets: capped responses, explicit truncation, outline-first / expand-on-demand |
| Editor session = a running process with open buffers | Agent session = a conversation that will be summarized | Stateless protocol; all durable state (journal, checkpoints) lives server-side and survives everyone |

Two consequences worth calling out:

**Staleness is the default, not the exception.** A human notices the file
changed under them. An agent's snapshot silently rots — another agent, a git
pull, the human's own editor. kaed treats every agent-held copy as
presumed-stale: reads return a content version, mutations declare the
version they were computed against, and a mismatch fails loudly with a
minimal delta ("here's what changed since you looked") instead of landing a
corrupted edit.

**Verification must be free.** Today an agent that cares about correctness
pays a full re-read after every write. kaed inverts this: the write response
*is* the verification — the applied diff, the new version, diagnostics if
the edit broke the parse. One round-trip instead of three.

## Design principles

These bind every tool in the contract; the contract doc restates them as
rules R1–R6:

1. **Every content response carries a version** (content hash). No agent
   should ever hold text it can't pin to a version.
2. **Every mutation declares its base versions and is atomic** — across all
   hunks and all files, or nothing. Dry-run is always available.
3. **Responses are budgeted, truncation is explicit** — a capped result says
   so and says how to continue. Silent truncation is corruption of the
   agent's world model.
4. **Errors are structured** — `{code, message, data}`, machine-actionable
   (`version_conflict` carries the delta; `ambiguous_anchor` carries the
   candidate locations).
5. **Multiple addressing modes, one edit engine** — anchor (unique content
   match), range@version, syntax node. Agents pick per situation; semantics
   are identical underneath.
6. **The protocol is stateless; the history is durable.** No handle a
   session must keep alive; everything a successor session needs is
   queryable from the journal.

## What kaed is not

- **Not a human editor.** No rendering, no cursor, no keybindings, ever.
  Humans use their own editors; kaed's journal is how their edits and the
  agents' edits stay mutually visible.
- **Not a shell.** No exec tool. Agents have ssh for that; keeping mutation
  surface = file edits keeps the security story tractable.
- **Not git.** The journal is operational memory (what happened on this
  host, by whom, revertable), not version control. Git remains the source
  of truth for project history; kaed must coexist with it gracefully.
- **Not an LSP client — yet.** Tree-sitter (fast, in-process, no server
  lifecycle) is the v0 structure engine. LSP-grade semantics (rename
  symbol, find references, live diagnostics) are a Later-tier ambition.
- **Not a sync tool.** Files live on the host; kaed edits in place.

## Success criteria

- Desktop Claude on cleo reaches for kaed instead of the rclone mount or
  base64-over-ssh for edits on kai — because it is *better*, not because it
  was told to.
- Edit-verify round-trips measurably drop (the write response suffices).
- Zero silent-failure incidents: every failed write surfaces as a
  structured error.
- The feedback loop functions: at least one contract revision in the first
  month traceable to an agent-filed friction report.

## Open questions

- **Journal vs git overlap** — when a repo is git-tracked, is `revert`
  journal-based or should kaed delegate to git? Current lean: journal-based
  (works in non-repos too), with the journal recording git HEAD at
  transaction time for correlation.
- **Version granularity** — whole-file hash is v0. Per-region hashing would
  let two agents edit disjoint parts of one file without conflict; is that
  worth the complexity? Wait for real conflicts before deciding.
- **`apply_patch` in v0?** — unified-diff input is a convenience for agents
  that already think in diffs, but overlaps the op-based edit tool. Lean:
  defer to Next; let dogfooding say whether it's missed.
- **Feedback → korg flow** — feedback lands in kaed's own store first;
  does a periodic job promote it to korg work items, or is that manual?
- **Port/auth conventions** — one port across hosts, bearer token per agent
  identity (mirroring klams/korg); exact values decided at first deploy.
- **ksandbox** — in scope in principle; deploy after the contract settles.
