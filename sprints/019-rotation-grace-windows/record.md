# Sprint 019 — grace windows for all nine identities

*korg proposal 1382, covering #1375. Slice 9 of program 1374 ("krot
rollout"). Branch `019-rotation-grace-windows`. Started 2026-08-17.*

## Goal

Make rotation the rolling, non-breaking story `docs/setup.md` has described
since sprint 002 — for every credential, not just one.

krot's sprint 002 went and looked at the fleet (krot #1359, 2026-08-16) and
found that of the nine (backend × author) credentials, exactly **one** —
kai's `[auth].claude` — has `prev_token_file` configured. The other eight
have none, so rotating any of them is a hard cut: the old token stops
authenticating the instant kaed reloads, and every live session on it starts
401ing. For the six gateway-consumed credentials there isn't even a safe
update order, because both the backend's `[auth]` and kai's
`[peers.<host>.tokens]` have to change and either order 401s in between.

Reproduced first thing this sprint, unchanged:

| Host | `claude` | `claude-kai` | `claude-kubs0` |
|---|---|---|---|
| kai | `prev_token_file` ✔ | — | — |
| kubs0 | — | — | — |
| kubsdb | — | — | — |

Nothing here rotates anything. This is insurance, landing before the
scheduled 2026-08-21 live rotation test in the krot project so that the test
exercises the rolling path instead of proving the hard cut.

## The shape of the problem

The server side was never broken. `prev_token_file` has worked since 002:
`AuthEntry` carries it, `resolve_identities` reads it on every SIGHUP, and
`auth_middleware` accepts it while logging the `authenticated with the
PREVIOUS token` warning that tells you when the window is safe to close.

What was missing is that nothing ever *said* the window wasn't there. The
config template offered `prev_token_file` as a commented-out suggestion,
`new-token.sh --rotate` printed a reminder after the fact, and kaed itself
was silent. Eight identities were configured by three different sprints
without one, and the only way anyone found out was a human reading three
config files on three hosts.

So the fix is three things, not one: configure the eight, stop the next
identity from being added without a window, and make the absence visible
from the host itself.

## What shipped

**The eight config lines**, which was the ask: every `[auth]` identity on
all three hosts now declares `prev_token_file`. See `deploy.md`.

**`kaed-new-token --identity NAME`.** The script only ever knew about
`~/.config/kaed/token`, so the six per-machine credentials sprint 018 added
had no wrapper at all — rotating one meant longhand `cp`/`head -c 48`, which
is exactly where refuse-if-exists and never-print get lost. It now reads
that identity's `token_file` and `prev_token_file` out of `config.toml`
rather than deriving them by convention (D-6): the original `claude` lives
at plain `token` on every host, so any convention needs a special case, and
a wrong guess mints a real-looking credential at a path nothing references.

**`--rotate` refuses without a grace window**, `--force` overrides (D-4,
D-5). The old script printed a reminder to configure `prev_token_file`
*after* rotating; the fleet's 1-of-9 score is what that reminder was worth.

**A startup warning naming the uncovered identities.** `Config::resolve`
now calls `identities_without_grace_window` and warns once with the list, so
the coverage question is answerable from the host instead of by reading
three files on three machines — which is how this was found in the first
place. Startup only, not on SIGHUP (D-3).

**`deploy/config.example.toml` ships the window on by default**, including
the commented per-machine authors, so a new host starts covered.

Eleven tests in `tests/new_token.rs` drive the real script against a
throwaway `KAED_CONFIG_DIR` — the first coverage `new-token.sh` has ever
had, including the property that matters most, that neither mint nor rotate
ever prints a token value.

### The detector confirmed the finding before the fix went in

Run against kai's live config with a throwaway journal and a spare port:

```
WARN kaed::config: no prev_token_file: rotating these is a HARD CUT …
     identities=["claude-kai", "claude-kubs0"]
```

Exactly the two the audit named, found by the code rather than by a human
reading configs. Testing the `--identity` parser against that same real
config also caught a wrong message — a config that exists but names no entry
for a token path was reporting "there is no config to consult". Those are
different facts and the script now says which one it saw.

## Gate

`just check` green: `cargo fmt --check`, `clippy --all-targets -D warnings`,
and the full test suite including the 11 new `new_token` tests.

The config half of the deploy is already live and verified — `deploy.md` has
the routes, the backups and what was checked. The binary half ships at
`/sprint-ship` Phase 7 from merged `main`.

## Follow-ups

- **krot's `registry/kaed.toml`** still documents the 1-of-9 reality and the
  hard-cut warnings in `kaed-rotate-author`. Both are now stale. The
  proposal folds this into krot's scheduled 2026-08-21 rotation test, which
  this sprint deliberately precedes so the test exercises the rolling path.
- **Nine credentials, no inventory.** Credentials grow as authors ×
  endpoints (PD-4/PD-7) and nothing enumerates them — this gap was found by
  a human reading three files. kaed now warns about its own `[auth]` half,
  but the six `[peers.*.tokens]` entries on kai have no equivalent, and
  `krot` does not know about kaed yet.
- **The rotation is still two-ended for gateway-consumed credentials** and
  the second end is manual: copy the new value into kai's peer-token file.
  Documented in `docs/setup.md`; deliberately not automated (D-7).

## Deployed 2026-08-17 — `0.1.0-9518bff`, merged `main`

Config half went out during the sprint; the binary half shipped after the
squash-merge. Both are on kai, kubs0 and kubsdb, verified: the grace-window
warning is silent fleet-wide (it named two or three identities per host
before), `--identity` resolves each host's real config, no `.prev` file
exists anywhere, and `roots` through the gateway reports every host on
`9518bff` with both peers probing ok. Rollback target `0.1.0-aeae142`.

Full table and the exact probes in `deploy.md`.
