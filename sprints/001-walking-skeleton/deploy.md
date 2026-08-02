# Sprint 001 deploy — kai

Live since 2026-08-02: **https://kai.<tailnet>.ts.net:4870/mcp**

(`<tailnet>` is the real tailnet name, kept out of the repo on purpose —
klams knows it, as does `tailscale status` on any homelab host.)

## What's where on kai

| Thing | Path |
|---|---|
| binary | `~/.local/bin/kaed` (release build from this repo) |
| config | `~/.config/kaed/config.toml` (root `home` = /home/ken) |
| token | `~/.config/kaed/env` — `KAED_TOKEN_CLAUDE`, 0600, never committed |
| unit | `~/.config/systemd/user/kaed.service`, enabled (linger already on) |
| journal | `~/.local/share/kaed/journal.db` |
| serve | `sudo tailscale serve --bg --https=4870 localhost:4870` |

Recorded as k-homelab WI #907 (incoming-change, kai) — the serve entry
needs declaring in `manifests/kai.yml` `tailscale_serve` next k-homelab
session. klams knows the deployment (memory tagged `kaed`).

## Verified after deploy

- `kaed check-config` clean; unit active.
- 401 without/with wrong bearer token, loopback and ts.net both.
- MCP initialize over the ts.net URL from kai itself (hairpin OK — the
  loopback-bind + serve pattern from the 2026-07-17 klams note).
- Full loop through the live service on `~/scratch/kaed-dogfood`:
  search (version in the hit) → `edit` anchor_replace with that base →
  applied, diff proof, journal txn 1 by `claude` with intent, completed.

## Gotcha that will bite again

rmcp's streamable-HTTP server validates the `Host` header against an
allowlist (DNS-rebinding guard, loopback-only by default). Fronted by
tailscale serve the Host is the ts.net name, so `server.allowed_hosts`
in config.toml must carry `kai.<tailnet>.ts.net` (and the `:4870`
form). Symptom otherwise: 4xx before auth even runs. Same will apply on
kubs0/kubsdb at fleet-deploy time.

## 401 semantics (save future-you a hunt)

kaed has **no token expiry**. A 401 means the presented token matched
no configured identity — wrong or rotated, full stop. Clients may
render it as "requires re-authorization (token expired)" (cleo's did);
that's the client's generic 401 story, and there is no TTL to go
looking for. Fix the client's token and restart it — Claude Code loads
MCP servers only at session start. kaed reads its token env file only
at startup too: after editing `~/.config/kaed/env`,
`systemctl --user restart kaed`.

## Wiring a client (Desktop Claude on cleo)

Add a custom connector / MCP server:

- URL: `https://kai.<tailnet>.ts.net:4870/mcp`
- Bearer token: value of `KAED_TOKEN_CLAUDE` in `kai:~/.config/kaed/env`

(If the client offers no auth-header field, it needs a bridge — note it
as friction for the contract revision.)
