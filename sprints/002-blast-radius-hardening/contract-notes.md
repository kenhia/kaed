# Contract changes from sprint 002

Unlike [001's notes](../001-walking-skeleton/contract-notes.md), which
recorded gaps to fold in later, these are changes **already applied** to
`sprints/planning/mcp-contract.md`. Kept here so the sprint's contract
delta is reviewable in one place.

## Applied to the contract

- **R1** now states that a version is a pure content address: it never
  expires, and survives client restarts, server restarts, reconnects and
  token rotation. (#915)
- **R4** gained the `denied` code, with `data: {path, rule}`.
- **R7 — the deny list is absolute, and never silent.** New rule. (#908)
- **Transport & auth** now states that tokens do not expire, that a 401
  says so per RFC 6750, and that rotation is non-breaking server-side via
  reload + grace window. (#913, #914)
- `list` and `search` outputs gained `denied_hidden?`.

## Decisions behind them

- **`denied` is a distinct code, not `outside_root`.** The remedies
  differ: `outside_root` means "your path escaped, fix it", `denied` means
  "no path correction will work, stop". An agent that conflates them will
  retry forever.
- **Enumeration filters, addressing errors.** `list`/`search` omit denied
  entries rather than failing the whole call — one refused file shouldn't
  break a directory listing. But the omission is *counted*, because a
  silently filtered result reads as complete.
- **The deny check is lexical.** It runs on the path, never on the
  filesystem, so `denied` is returned identically for paths that exist and
  paths that don't. A `denied` is therefore never evidence a file is
  there.
- **Both the joined path and the resolved target are checked.** A symlink
  with an innocent name pointing into a denied directory is refused; the
  jail already handles escapes *out* of the root, but not walks *into* a
  denied area inside it.

## Where enforcement actually lives

Worth writing down because the sprint proposal got it wrong, and the same
mistake is easy to repeat when a tool is added:

`fsops::resolve_existing` / `resolve_creatable` are the choke point for
**addressed** paths only. `list` and `search` walk directories with their
own `ignore::WalkBuilder` and never call the resolver per entry. Any new
tool that enumerates rather than addresses needs its own deny filter —
`fsops::check_denied` for a single path, a `filter_entry` predicate for a
walk.

## Still open for the first full revision

- `total` on `search` output is in the contract but not implemented.
- `outline` / `node_replace` / `check` remain unimplemented (planned).
- The `feedback` tool is specified but not built, so agent friction still
  has no in-band path back.
