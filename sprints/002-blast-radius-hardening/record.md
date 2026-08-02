# Sprint 002 — blast-radius hardening

Branch `002-blast-radius-hardening`, from korg proposal 916. Scope: the
complete sprint-001 follow-up set — WIs #908, #909, #910, #913, #914, #915.

The theme is the title. Sprint 001 proved the loop works; the live test from
cleo then showed how much of the host a single compromised or careless client
can reach. Everything here shrinks that radius, or makes what happened inside
it visible. It is deliberately the sprint before adding roots, clients, or
hosts, because each of those multiplies whatever the radius is when they land.

## Goal

Ship the six follow-ups, and — for the two decision-shaped ones — write the
decision down whether or not the code lands with it. See
[decisions.md](decisions.md).

## Decisions taken up front

Both live in [decisions.md](decisions.md) in full. In one line each:

- **#909 journal blobs** — keep whole content (the features depend on it),
  let the deny list do the protecting, and make retention *real* instead of a
  config knob that GCs nothing.
- **#914 token rotation** — reload-without-restart is the load-bearing half
  and needs tokens to come from a *file*, not just an env var; the grace
  window is what makes the reload safe.

## What shipped

*(written as it lands)*

### #908 — root at `$HOME` exposes kaed's own token and SSH keys

Three layers, because they fail differently:

1. **Built-in self-protection**, not configurable: kaed refuses any path
   under its own config directory or its journal directory. No config can
   turn this off, so kaed can never serve or rewrite the credential that
   gates it — regardless of how the roots are drawn.
2. **Config deny globs** (`[security] deny`), matched against absolute
   paths, extending a default list (`.ssh`, `.gnupg`, `.aws`, `.env`,
   `*.pem`, …). `use_default_deny = false` opts out of the defaults for
   someone who genuinely wants kaed editing their dotfiles; the built-ins
   above still stand.
3. **Explicit roots on kai** — `home` = `/home/ken` becomes narrower roots.
   Config/deploy change, no code.

**The correction that mattered:** the proposal called the path resolver "one
choke point" for this. It is not. `resolve_existing`/`resolve_creatable`
cover every *addressed* path — `stat`, `read`, `edit`, and the base
directory of a walk — but `list` and `search` **enumerate** with their own
`ignore::WalkBuilder` and never call the resolver per entry. A deny check
only in the resolver would have left `search` returning the *contents* of
denied files and `list` enumerating them. The check therefore lands in three
places, and the tests assert all three.

A denied path fails with a new `denied` error code rather than reusing
`outside_root`: the remedy differs (fix your path vs. this is permanently
off-limits), and the check is purely lexical, so it fires identically for
paths that exist and paths that don't — no existence oracle. Enumeration
doesn't error; `list` and `search` omit denied entries and report how many
they hid (`denied_hidden`), because silently dropping them would read as
"that's everything" when it isn't.

### #913 — RFC 6750 attributes on 401

A wrong token now answers with

```
WWW-Authenticate: Bearer realm="kaed", error="invalid_token",
  error_description="token matches no configured identity; kaed tokens do not expire"
```

and the same sentence in the body, so a client that surfaces either one
stops inventing a TTL. A request with **no** credential gets a bare
`Bearer realm="kaed"` — RFC 6750 §3.1 says not to report an error code
when there was no token to be invalid, and that case never produced the
confusing message anyway.

Also added: a `warn` log on every 401. Auth failures are rejected before
the transaction layer, so #910 will never make them visible in the journal
— see the split-out note in [decisions.md](decisions.md#d2).

### #915 — contract: versions are content-derived and durable

R1 now says what was always true but never written: a version is a pure
content address, not a lease or a session handle, so it survives client
restarts, server restarts, reconnects and token rotation. The consequence
is the part worth advertising — an agent resuming after a crash or a
compaction can edit straight from a version it recorded long ago and
either succeed or get a precise conflict, with no defensive re-read. The
same claim goes in the server `instructions`, where an agent will actually
read it.

R4 gained `denied`; a new R7 covers the deny list's contract (permanent
refusal, `denied_hidden` on enumeration, no existence oracle); the
transport section now states the no-expiry and rotation semantics.

### #910 — journal failed transactions

New `txn_failures` table: author, intent, root, target paths, error code,
message, and — lifted into their own columns because they are the reason
the table exists — `expected_version` / `actual_version`. No blobs, per
#909: a failed attempt produced no new content, and its pre-image is still
the file on disk.

`txn::apply` is now a thin wrapper around `apply_inner` that records any
`Err`, so every failure path is covered by construction rather than by
remembering to instrument each `return`. Dry runs are excluded — nothing
was attempted.

The metric this exists for is one query, and there's a test that runs it:

```sql
SELECT author, count(*) FROM txn_failures
 WHERE code = 'version_conflict' GROUP BY author;
```

### #909 — blob retention

Decision in [decisions.md](decisions.md#d1). What landed: `gc_blobs()`
drops expired content and leaves every `txns`/`txn_files` row alone;
it runs on open and at most hourly thereafter (off the completion path, so
no timer task). `journal.retention_days` now means *blob* retention and
defaults to **7**, down from 30 — with GC actually running, that number
became a real quantity of retained file content rather than a decoration.
`journal.db` and its `-wal`/`-shm` siblings are forced to 0600.

### #914 — token reload and grace window

`[auth]` entries take `token_file` (re-read on SIGHUP) and
`prev_token_file` (honoured during rotation) alongside the original
`token_env`. `SIGHUP` swaps the identity table in place behind an
`RwLock`; live sessions survive, because it is the process restart — not
the token — that kills them.

Guard rails: exactly one of `token_env`/`token_file`; `prev_token_file`
requires `token_file`, since a grace window on a token that can only be
loaded by restarting is theatre. A reload that resolves *no* identities
keeps the old set and logs an error rather than locking everyone out.

Use of a grace token logs a `warn` naming the identity — the one signal
that answers "has every client picked up the new secret yet?", which is
otherwise unanswerable because 401s never reach the journal.

## Follow-ups

- **Auth-layer counters.** The `warn` logs (401s, grace-token use) are the
  cheap version. A real counter — per-identity 401 rate, grace-token rate —
  is what a fleet rotation actually wants, and the first thing kmon would
  scrape alongside the conflict rate from #910.
- **`journal` read tools should surface `txn_failures`.** The table is
  written but nothing reads it yet; when `journal`/`diff` land, failures
  belong in the same view as successes.
