# Sprint 015 live test — from cleo, 2026-08-12

The sprint's only real acceptance gate. Everything kaed can assert about
itself passes with the bug present *or* absent from the client's point of
view: the failure mode is a client silently registering zero tools, which no
server-side check can see. So the gate is a **fresh Claude Code ≥2.1.227
session on cleo** enumerating the tools and completing a real call.

## Result: passed

Ken ran it from cleo before the sprint shipped, against the branch canary
`0.1.0-bc87367` on kai, and reported everything working correctly — tools
enumerable, calls completing. That is the failure from #1212 closed at the
layer it actually appeared in.

**The canary and the shipped build are the same code.** `git diff bc87367
0743ff0 -- src/ tests/ Cargo.toml Cargo.lock build.rs` is empty; the only
commit between them changed `CLAUDE.md`. So this result carries to
`0.1.0-0743ff0`, the build now on all three hosts, without a re-run.

Call-by-call detail was not captured — the session that ran it is not the
session writing this. What is recorded here is the verdict and what it was
run against.

## Server-side battery, all three hosts, on the shipped build

Run from kai over each host's real URL (see `record.md` for the version and
unit table):

| probe | kai | kubs0 | kubsdb |
|---|---|---|---|
| `initialize` asking `2026-07-28` | → `2025-11-25` | → `2025-11-25` | → `2025-11-25` |
| session id issued | yes | yes | yes |
| `initialize` asking `2025-11-25` | → itself | → itself | → itself |
| `tools/list` after the full handshake | 12 | 12 | 12 |
| `tools/call roots` | 7 roots | 2 roots | 3 roots |

The full sequence — `initialize` → `notifications/initialized` →
`tools/list` → `tools/call` — was driven end to end, not just `initialize`,
because the session-id regression this sprint's D-2 is about only shows up on
the *second* request. kai's 7 roots are its own two plus both peers', so
gateway proxying survived the change.

## Findings

None. Nothing to file.

Two things worth stating anyway, because they are easy to re-derive wrongly:

- **`2025-06-18` and older still negotiate as themselves.** The cap is a
  ceiling, not a pin; nothing was downgraded that did not need to be.
- **The branch canary `0.1.0-bc87367` is still in the store** and names a
  commit that no longer exists on `main` after the squash. It is a rollback
  target nobody should pick — the real one is `0.1.0-88005bc` (the build with
  the bug) or `~/.local/bin/kaed.prev`.
