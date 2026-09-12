# Sprint 023 — forwarded identity replaces the token matrix

*korg proposal 2424, covering #2392 (the server change) and #2393 (the
cutover). Branch `023-forwarded-identity`. Started 2026-09-12. Slice 5 of
korg program 2440, "simplify homelab secrets" — an **overseen** sprint run
as a headless karc leg.*

## Goal

kaed's bearer tokens are name tags, not locks. Replace them with a declared
identity — `X-Homelab-Agent: <name>` against an allow-list — and let the
gateway forward the caller's declared name to peers instead of holding a
token per `(author, backend)` pair. Record the caller's tailnet node beside
the identity, record-only. Then cut every client over and delete the nine
credentials.

**Identity fidelity is the thing that must survive.** PD-4 chose per-(author,
backend) tokens precisely so a proxied call journals on the peer under the
*caller's* name and never a borrowed one. That property is unchanged here;
only the proof of the name changes, from a shared secret to a declaration
inside a tailnet perimeter.

## Why now

PD-4 left this door open in as many words — "allowlisted-peer forwarded
headers … remain open if token sprawl actually starts to hurt. Do not build
them speculatively." It hurt. Measured on the fleet today, before any change:

| where | files | what they are |
|---|---|---|
| kai `~/.config/kaed/` | 3 | inbound identity tokens |
| kubs0 `~/.config/kaed/` | 3 | inbound identity tokens |
| kubsdb `~/.config/kaed/` | 3 | inbound identity tokens |
| kai `~/.config/kaed/peer-tokens/` | 6 | the gateway's *client* copies of kubs0's and kubsdb's |

Nine `(backend × author)` credentials for three authors and three backends,
exactly as PD-4 predicted the shape would grow, plus six copies on the
gateway. 018 already recorded that the fleet was "at nine with no inventory".

## Premise check

Both items' falsifiable claims, checked against the fleet and the source
before any code was written.

- **#2392 — premise holds.** `[auth]` is token-file based (`config.rs`
  `AuthEntry`, live `config.toml` on all three hosts); `prev_token_file`,
  `identities_without_grace_window` and the startup warning are present
  exactly as 019 left them; `deploy/new-token.sh` exists; `[peers.<host>.tokens]`
  is populated on kai for both peers; `fleet.rs` refuses `no_peer_credential`
  when an author has no token for a backend. Nine credentials confirmed on
  disk, counted above.
- **#2393 — premise holds.** `~/.claude.json` on kai and kubs0 both carry a
  single `kaed-kai` server whose only header is `Authorization`; kubs0's
  points at kai's tailnet URL, not its own kaed, exactly as 018 D-1 recorded.
  karc's leg toolbox (`~/.config/karc/karc-legs.mcp.json`) carries an
  `Authorization` header for `kaed-kai` — the "second copy of kai's token"
  KP-5 names. Token files present on all three hosts.

One premise **sharpened** rather than drifted: the proposal asks whether kaed
"sits behind `serve` or terminates TLS itself before trusting
`X-Forwarded-For`". Measured on kai: `tailscale serve` fronts
`https://kai.<tailnet>:4870` and proxies to `http://localhost:4870`, and
kaed's `[server] bind` is `127.0.0.1:4870`. So kaed's deployment is the same
shape klams measured, and klams's `X-Forwarded-For`-primary / socket-peer-
fallback rule transfers without change.

## Cross-project plan

kaed is in the `karc+` cluster for **KP-5 only** — "identity grain: unsettled,
and kaed's to settle", kaed PD-7 vs karc PD-6, kaed WI 1861. This sprint
settles it, so KP-5 is amended in the same ship (the plan's own rule:
*amend in the sprint that invalidates*). The other half of the row is karc
PD-6, and it is already filed as program slice korg:2425, which mirrors this
one — so the amend checklist is satisfied by a filed counterpart, not left
hanging.

## Decisions

See `decisions.md`. The load-bearing ones: D-1 (header beats bearer, unknown
name never falls through), D-2 (the transition window is the token rows
themselves), D-3 (the gateway forwards, and `no_peer_credential` dies with the
matrix), D-5 (node pinning is the only enforcement, opt-in and default off),
PD-10 (the identity model, superseding PD-4's mechanism) and PD-11
(`/etc/klams` does not become a kaed root, and why).

## What shipped

**The server.** `[auth]` is an allow-list (`src/config.rs`); the middleware
checks `X-Homelab-Agent` first and refuses an unknown name outright
(`src/server.rs`); `src/whois.rs` resolves the caller's tailnet node, cached,
record-only, with per-identity `nodes` pinning behind a default-off
`[whois] enforce`. The 401 now distinguishes three cases, because each needs a
different fix.

**The gateway.** `fleet.rs` forwards the caller's declared name to peers via
rmcp's `custom_headers` and holds no credential. `no_peer_credential` is gone;
`peer_credential_rejected` was re-aimed at the backend's allow-list and now
carries the right remedy (add the name there, and **restart**).

**The journal.** `node` on `txns`, `txn_failures` and `secret_events`, with a
migration for existing databases, surfaced through `journal`'s three entry
kinds. Written via `Journal::as_node()`, a per-request recorder view.

**Retired.** `deploy/new-token.sh` and `tests/new_token.rs` deleted, out of
the published bundle, and `install.sh` now *removes* a stale
`kaed-new-token` from `~/.local/bin`. The grace-window machinery
(`prev_token_file` handling, `identities_without_grace_window`, its startup
warning) is gone; the field still parses and is ignored.

**Docs.** SECURITY.md says plainly that the perimeter is the access-control
boundary and the name is attribution — including why the old tokens were not
the protection they read as. `docs/setup.md` replaces "create the token" with
"allow-list your identities" and "rotating a token" with "revoking an
identity". R13 added to the contract; R10's identity bullet rewritten.
`config.example.toml` ships the new shape.

**No rmcp bump was needed.** 3.1.0 already has `custom_headers`, so 016 D-2
(`PEER_PROTOCOL_VERSION` pinned below what kaed serves) is untouched.

### Repaired in passing

- **A refused identity was being reported as an outage.** With the
  `no_credential` pre-gate removed, a backend that *answered* and declined a
  forwarded name reached the `Err` arm of the fleet-search probe and was
  reported `unreachable` with a `since` — a false claim about the world in the
  one field whose job is stopping an agent forming a wrong picture (korg
  #1089's failure, from the other side). Now `identity_refused`, with the
  author and no `since`. Pinned by the fleet-search gateway test.
- **`check-config` still described the retired model**: it printed "(token
  resolved)" per identity and "proxies for [...]" per peer, both read off the
  token tables. It now prints the allow-list, flags which identities still
  hold a legacy bearer, reports `[whois]`, and says a routable peer "proxies
  as the caller" — naming any surviving peer-token rows as retired rather than
  as configuration.

### Verified live, from kai

Two things the test suite structurally cannot show, both run against the
release binary on a scratch port and config — the live service was not
touched:

- **The new binary parses kai's existing, unmodified pre-023 config** and
  starts, warning by name about the three identities still holding a bearer
  and about `prev_token_file`. This is D-2's whole reason for keeping those
  fields parseable, and `install.sh` never rewriting a config is what makes it
  load-bearing at deploy time.
- **End to end with no token anywhere**: a declared identity initializes
  (`200`), an unknown name is refused with the message that names the actual
  problem, no credential is refused with the remedy — and an `edit` forwarded
  with a real tailnet address journaled `author=claude-kai, node=kai`,
  resolved through a real `tailscale whois`. That is WI 2392's acceptance
  criterion minus the cross-host hop, which needs the fleet deploy.

## Cutover status

WI 2393 (the cutover) **cannot run before the ship.** It needs the new binary
live on all three hosts, and kaed is deployed from the package store from
committed `main` — never from a branch. So the order is: ship → `sprint-ship`
Phase 7 deploys the fleet → then the client cutover and credential deletion,
recorded below under `## Deployed`.

Sequencing the cutover so it does not cut off the session performing it:

1. Deploy all three hosts (transition window open; nothing breaks).
2. Add the header to each client, kai and kubs0 first, **cleo last** — it is
   the historically-missed host and its Claude Desktop config is
   MSIX-packaged, so it is edited from `ssh cleo` with a targeted, non-
   printing edit and verified from ssh.
3. Verify a live edit from each host journals under the right identity.
4. Only then delete the nine token files and the `[auth]` / `[peers.*.tokens]`
   rows, and reload.

Step 4 is last on purpose: this leg's own kaed client holds a copy of kai's
token, and the karc leg toolbox keeps working throughout the window.

## Follow-ups

- **korg #2471** — delete the legacy bearer path. It stays in the shipped
  binary for one release as a rescue hatch for a missed client (D-8);
  removing it needs a second deploy after the window is observed closed
  *from cleo*, which is the decision this sprint cannot make.
- **kaed WI 1861 dissolves** (the karc leg-toolbox credential): there is no
  credential to give its own identity. Recorded on the item and settled in
  the cross-project plan as KP-6.
- **krot WI 2466** gets PD-11's answer: `/etc/klams` does not become a kaed
  root, because kaed runs as `ken` and cannot read it at all.
