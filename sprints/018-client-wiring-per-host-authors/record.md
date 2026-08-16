# Sprint 018 — client wiring on kai and kubs0, with per-host authors

*korg proposal 1354, covering #1350. Slice 1 of program 1357 ("kaed on kai
and kubs0"). Branch `018-client-wiring-per-host-authors`. Started and
finished 2026-08-16.*

## Goal

Give agents on kai and kubs0 the cross-host editing route cleo has had
since sprint 004. Until today the documented answer for a Claude Code
session on kai that needed to change a file on kubs0 was base64-over-ssh —
the route whose named failure mode is *a write that reports success and did
not land*. kaed has been running on all three hosts since 004/013; only the
client half was missing.

Nothing in this sprint changes the server. No Rust was written. What
changed is credentials, three `config.toml` files, and two client
registrations — plus the decision recorded as PD-7, which is the part worth
keeping.

## What shipped

**Two new authors, `claude-kai` and `claude-kubs0`.** cleo keeps bare
`claude`. Six credentials in total, because each client authenticates to
kai *and* kai proxies to each backend as the caller:

| Where | Identity | Purpose |
|---|---|---|
| kai `[auth]` | `claude-kai`, `claude-kubs0` | both clients dial kai |
| kai `[peers.kubs0.tokens]` | `claude-kai`, `claude-kubs0` | gateway → kubs0, as the caller |
| kai `[peers.kubsdb.tokens]` | `claude-kai`, `claude-kubs0` | gateway → kubsdb, as the caller |
| kubs0 `[auth]` | `claude-kai`, `claude-kubs0` | accept the proxied identities |
| kubsdb `[auth]` | `claude-kai`, `claude-kubs0` | accept the proxied identities |

**Two MCP registrations**, both named `kaed-kai` after the host that
answers, both added with `claude mcp add` rather than by editing
`~/.claude.json`:

- kai → `http://127.0.0.1:4870/mcp` (its own kaed, which is the gateway)
- kubs0 → `https://kai.<tailnet>:4870/mcp` (**kai, not its own kaed**)

kubs0 pointing at kai is the counter-intuitive one and it is deliberate:
kubs0 is a plain backend with no peer tokens, so a localhost entry there
would serve `kubs0:src` and `kubs0:k-homelab` only — files that host
already edits natively. It would deliver nothing. kubs0 wires to the
gateway exactly as cleo does, and accepts the same exposure cleo already
carries: if kai is down, kubs0 loses kaed entirely, including its route to
kubsdb. The documented fallback is kubs0's own direct backend.

## Decisions

`decisions.md` beside this file: D-1 (why the identity is the host, not the
harness), D-2 (why the token layout is six files and not two), D-3 (adding
an identity is a restart, not a SIGHUP), D-4 (why the gate had to be a real
Claude Code session and not an rmcp client). The cross-sprint one is
**PD-7** in `sprints/planning/decisions.md`.

## Gate

`live-test.md` beside this file. All five gate items from #1350 pass, plus
one the WI did not ask for.

The one it did not ask for is worth naming: the WI's apply-safety section
said to run `bin/audit kai kaed-service` and confirm `ok`, because
`kaedconf.py` deleting hand-added `[peers.<host>.tokens]` used to be a
blocker (korg #1072 fixed it). An `ok` audit only says *no drift was
detected*. I ran the real `bin/apply kai kaed-service` instead and compared
the config's md5 either side: byte-identical, all six entries intact. That
turns an inference into an observation, which is the difference the audit
alone could not give.

## Two things learned that cost time

**`tailscale status --json | grep '"MagicDNSSuffix": *"'` matches more than
one line.** The suffix appears twice in the JSON, so the documented recipe
yields a two-line variable and every URL built from it becomes malformed.
curl reports this as `000`, or — worse — as a plausible-looking HTTP status
when the mangled host happens to resolve to something. It cost a round of
chasing a nonexistent auth failure. `grep -m1` fixes it, and CLAUDE.md's
tip has been amended.

**Driving `2026-07-28` by hand needs four things, not one.** The revision
requires, per request after `initialize`: an `Mcp-Method` header, an
`Mcp-Name` header on `tools/call` specifically, and `_meta` carrying
`io.modelcontextprotocol/protocolVersion` and `.../clientCapabilities`.
Each one is a separate `400` that names only itself, so the recipe is found
one refusal at a time. `sprints/018-*/probe.py` is the working version,
kept because CLAUDE.md's standing rule is that a green rmcp-client test
says nothing about `2026-07-28` behaviour — so the next person testing this
revision by hand should not have to rediscover the handshake.

## Follow-ups

- Slices 2 and 3 of program 1357 — agent-skills #1351 (per-host CLAUDE.md
  sections telling agents the route exists) and k-homelab #1352 — follow
  deliberately, in that order. Instructions for a route that is not yet
  live are how an agent ends up trusting a tool that is not there; the
  route is live now, so they are unblocked.
- GitHub Copilot CLI on both hosts is still unwired, deferred on purpose:
  its MCP servers stay untrusted until re-added through the CLI's own
  `/mcp add`, so it is a manual pass per host rather than a config copy.
- Nothing manages `~/.claude.json`, and this sprint did not change that.
  k-homelab #1353 holds the question.
- **The token count is now 9 across the fleet and there is no inventory.**
  Six were minted today. `krot` is the project that should hold them; it
  does not know about kaed yet. Not filed — flagging it here rather than
  inventing scope.

## Deployed

**2026-08-16. `0.1.0-aeae142` on kai, kubs0 and kubsdb.** Published from
merged `main` (`aeae142`) with `just publish`; every host installed that
artifact with `install.sh --from-store`, kai included.

Worth being honest about what this deploy was: sprint 018 changed **no
server code**, so `aeae142`'s binary is functionally identical to the
`af51376` it replaced. It was deployed anyway so the fleet's version stamp
names a commit on `main` — this project treats "which build is running" as
answerable from the tool surface, and leaving three hosts stamped with a
commit two sprints back quietly costs that.

Rollback target: `0.1.0-af51376`, either from `~/.local/bin/kaed.prev` on
any host or `install.sh --from-store --version 0.1.0-af51376`.

### Verified live

| Host | `kaed --version` | `check-config` | unit | MCP `serverInfo.version` |
|---|---|---|---|---|
| kai | `0.1.0 (aeae142 2026-08-16)` | exit 0 | active | `0.1.0 (aeae142 2026-08-16)` |
| kubs0 | `0.1.0 (aeae142 2026-08-16)` | exit 0 | active | `0.1.0 (aeae142 2026-08-16)` |
| kubsdb | `0.1.0 (aeae142 2026-08-16)` | exit 0 | active | `0.1.0 (aeae142 2026-08-16)` |

All three exact matches against the published version, and the MCP column
closes the loop: the binary on disk and the server answering the network are
the same build.

### Re-verified this sprint's own work, because the deploy restarted it

A deploy restarts every daemon, which drops exactly the wiring this sprint
installed — so the health check above proves the service is up, not that 018
is still live. Re-checked afterwards:

- `install.sh` reported **`config exists, left untouched`** on kai, and all
  three hosts still resolve `claude`, `claude-kai` and `claude-kubs0`. The
  six new credentials survived, which is the claim `install.sh` makes about
  itself and the one worth checking on the deploy that first depends on it.
- A **fresh Claude Code session on kai**: 7 roots, all three hosts verified,
  every fleet entry reporting `aeae142`.
- A **fresh Claude Code session on kubs0**: 7 roots, all verified, and an
  edit to `kubsdb:src` applied as txn 10 — journaled on kubsdb under
  `claude-kubs0`, verified on disk, then removed.

So the end-to-end path this sprint built — kubs0's agent editing kubsdb
through kai's gateway under its own identity — was exercised again *after*
the redeploy, not just before it.
