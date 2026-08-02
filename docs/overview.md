# kaed: what it is and why it exists

> New here? [`kaed-explained.html`](kaed-explained.html) is the same story as
> one visual page
> ([rendered preview](https://htmlpreview.github.io/?https://github.com/kenhia/kaed/blob/main/docs/kaed-explained.html)).
> This document is the prose version, with the reasoning.

## The problem

An AI agent running on one machine needs to edit files on another. That is it.
That is the whole problem, and it turns out to be badly served.

The two obvious approaches both fail, and they fail *quietly*, which is the
expensive part:

**Piping content over ssh.** Quote-safe if you base64 it, but it is whole-file
replace only — no surgical edits — and every write needs a manual `md5sum`
round-trip to confirm it landed. Agents skip that verification, because it
costs a turn and usually passes.

**Mounting the remote filesystem** (sshfs, rclone, NFS). Native tools work,
which is the appeal. But edits strip the executable bit; a hard kill corrupts
the VFS cache, after which **writes fail while the editing tool still reports
success**; and changes made on the far side hide behind a directory cache.
Silent data loss here is a lived experience, not a hypothetical.

Both are human file-access machinery pressed into agent service. Neither was
designed for a client that cannot look at the result.

## The insight

Almost every feature of a human editor exists to serve eyes and fingers on a
millisecond feedback loop. An agent has a completely different body, and when
you enumerate the differences, nearly every editor concept either dissolves or
inverts:

| Human editor | Agent reality | kaed's answer |
|---|---|---|
| Viewport — you *look at* the file | No eyes; a stale snapshot in a context window | Shaped reads: ranges, windows around a point, outlines. Reads are queries, not glances |
| Cursor & selection — position is ambient | No ambient state; every call must say what it means | Explicit addressing every time: content anchors, line ranges pinned to a version, syntax nodes |
| Keystrokes — thousands of tiny cheap edits | Few, expensive, batched mutations per turn | Edits are transactions: multi-hunk, multi-file, atomic, dry-runnable |
| Visual confirmation — you *see* it land | Cannot see; must trust or re-read (tokens) | Every edit returns proof: the diff that was applied, plus the new version |
| Undo stack — private, per-session | Sessions die, compact, resume; agents interleave | A durable journal: transaction-grained, attributed, queryable |
| Syntax *highlighting* — color for eyes | Color is meaningless; structure is not | Syntax *addressing*: outlines and node-targeted edits |
| One user at one keyboard | Several agents plus human tools on one tree | Optimistic concurrency: mutations declare a base version; conflicts are structured data |
| Free reads — glancing costs nothing | Every byte returned is paid for in context | Budgeted responses, explicit truncation, outline-first |

Two consequences drive the whole design.

**Staleness is the default, not the exception.** A human notices when a file
changes under them. An agent's snapshot rots silently — another agent, a `git
pull`, the human's own editor. kaed treats every agent-held copy as
presumed-stale.

**Verification must be free.** An agent that cares about correctness currently
pays a full re-read after every write. kaed inverts this: the write response
*is* the verification.

## How it works

kaed is a daemon. It owns the mechanics of editing on the host it runs on, and
exposes them as MCP tools over HTTP. Paths are relative to configured
**roots**; nothing outside a root is reachable.

### The loop

```
search or read  →  returns content + version
      ↓
edit, declaring that version as your base
      ↓
response carries the unified diff that was applied
      ↓                                    (and it's journaled)
done — no verification read
```

A **version** is the first 16 hex characters of the BLAKE3 hash of the file's
bytes. That choice does a surprising amount of work:

- It is computed on demand and stored nowhere, so it is **not a lease or a
  session handle**. It never expires. It survives client restarts, server
  restarts, reconnects, and credential rotation. An agent that crashed an hour
  ago can edit from the version it remembers.
- The only thing that invalidates it is the file's content changing — which is
  precisely what you want it to detect.
- Two files with identical content have the same version, which makes
  "did this actually change?" a string comparison.

If the version you declare doesn't match what's on disk, the edit fails with
`version_conflict` — and the error carries a **diff of what changed since you
looked**, so you can re-anchor and retry without re-reading the file.

### The invariants

Every tool in the contract is bound by these. The
[MCP contract](../sprints/planning/mcp-contract.md) states them as rules
R1–R7:

1. **Every content response carries a version.** No agent should ever hold
   text it cannot pin.
2. **Every mutation declares its base versions and is atomic** — all files and
   all hunks, or none. `dry_run` is always available.
3. **Responses are budgeted; truncation is explicit.** A capped result says so
   and says how to continue. Silent truncation corrupts the agent's model of
   the world.
4. **Errors are structured** `{code, message, data}` and machine-actionable.
   `version_conflict` carries the delta; `ambiguous_anchor` carries the
   candidate line numbers.
5. **Several addressing modes, one edit engine** — anchor (unique content
   match), range-at-a-version, and eventually syntax node. Identical
   transactional semantics underneath.
6. **The protocol is stateless; the history is durable.** No handle a session
   must keep alive. Everything a successor session needs is on the server.
7. **The deny list is absolute, and never silent.** Refused paths are refused
   everywhere, and filtered enumerations report how much they hid.

### Safety

Three layers, of deliberately decreasing absoluteness:

1. **Built-in refusals** — kaed's own config and journal directories. Not
   configurable, so kaed can never serve or rewrite the credential that gates
   it. (It could, once. That is why this exists.)
2. **Default deny globs** — `.ssh`, `.gnupg`, `.aws`, `.env`, `*.pem`, `id_*`
   and friends.
3. **Config deny globs** — whatever else you name.

Addressing a denied path returns `denied`, which is permanent — no path
correction makes it work. Enumerations (`list`, `search`) omit denied entries
rather than failing, but report `denied_hidden: N`, because a silently
filtered listing reads as a complete one.

**This is blast-radius reduction, not access control.** Any agent with a shell
can read what kaed refuses. What the deny list buys is that the ordinary,
well-intentioned path is safe. [SECURITY.md](../SECURITY.md) is explicit about
where the line is.

### The journal

SQLite, one database per host. Every applied transaction: who, what files,
diffstat, the optional `intent` note the agent supplied, and the git HEAD of
the enclosing repo if there is one. Every *failed* attempt too — a
`version_conflict` is the single most diagnostic event this service produces,
and for one sprint it was recorded nowhere.

The journal is operational memory, not version control. Git stays the source
of truth for project history; kaed records what happened on this host, by
whom, so a human's edits and several agents' edits stay mutually visible.

## What kaed is not

- **Not a human editor.** No rendering, no cursor, no keybindings, ever.
- **Not a shell.** There is no exec tool and there will not be one. Agents
  that need to run commands already have ssh. Keeping the mutation surface at
  "file edits" is what makes the security story short enough to state.
- **Not git.** The journal is not version control, and there is no git tool.
- **Not a sync tool.** Files live on the host; kaed edits them in place.
- **Not an LSP client — yet.** Tree-sitter is the planned v0 structure engine
  (fast, in-process, no server lifecycle). Rename-symbol and find-references
  are a much later ambition.

## Where it is now

**Early beta.** Six tools — `roots`, `stat`, `list`, `read`, `search`, `edit`
— live on one host, dogfooded daily, with the full verified-write loop working
end to end from a remote agent.

Three sprints in:

- **001 — walking skeleton.** The six tools, the R1–R4 semantics, atomic
  apply, journal writes, bearer auth over streamable HTTP. Deployed and
  verified from a remote agent.
- **002 — blast-radius hardening.** Everything the first live test exposed:
  the deny list, RFC 6750 error attributes on 401s, failed transactions in the
  journal, real blob retention, and non-breaking token rotation.
- **003 — public beta readiness.** These docs.

## Where it is going

Roughly in order, with the reasoning in
[`sprints/planning/roadmap.md`](../sprints/planning/roadmap.md):

- **Fleet deploy** — more than one host, and a repeatable install rather than
  a hand-built one.
- **History tools** — `journal`, `diff`, `revert`. The data is already being
  written; nothing reads it yet.
- **Structure** — tree-sitter `outline`, node-targeted edits, and parse
  diagnostics returned in the edit response (so "did I break the syntax?" is
  answered by the write, like everything else).
- **A `feedback` tool** — and then acting on it. The first contract revision
  driven by a friction report an agent filed itself is the real test of
  whether this design loop works.
- **The dogfood report** — a written comparison of kaed against the
  ssh and mount approaches on real editing tasks: round-trips, failures, token
  cost. The evidence for whether the bet paid off. Until that exists, "kaed is
  better" is a hypothesis.

Further out, and genuinely uncertain: secrets-aware editing (there is a
[brainstorm](../sprints/planning/brainstorm-secrets-editing.md), nothing
decided), per-region versioning so two agents can edit disjoint parts of one
file, and kaed as a *local* tool alongside an agent's built-in editing — the
"if we got this right" bet, which needs the dogfood report first.

## Reading further

The design documents live in
[`sprints/planning/`](../sprints/planning/) and are written for someone doing
the work:

- [`summary.md`](../sprints/planning/summary.md) — the one-page version
- [`overview.md`](../sprints/planning/overview.md) — longer design rationale
- [`mcp-contract.md`](../sprints/planning/mcp-contract.md) — the tool contract
  and rules R1–R7. **Read this before touching server code.**
- [`architecture.md`](../sprints/planning/architecture.md) — module sketch
- [`roadmap.md`](../sprints/planning/roadmap.md) — what's next and why

Each sprint's record — `sprints/NNN-name/` — holds the reasoning as it
happened, including what was rejected.
