# Sprint 002 deploy — kai

Live since 2026-08-02 18:01 UTC: **https://kai.\<tailnet\>.ts.net:4870/mcp**

(`<tailnet>` is deliberately not committed — klams knows it, as does
`tailscale status`.) Everything in
[001's deploy notes](../001-walking-skeleton/deploy.md) still applies
except where noted below.

## What changed on kai

| Thing | Before (001) | Now |
|---|---|---|
| roots | `home` = `/home/ken` | `src` = `/home/ken/src`, `scratch` = `/home/ken/scratch` |
| token | `token_env` from `EnvironmentFile` | `token_file` = `~/.config/kaed/token` (0600) |
| grace | — | `prev_token_file` = `~/.config/kaed/token.prev` (create only during a rotation) |
| deny | — | built-ins + defaults + `**/.config/kaed`, `**/secrets/**`, `**/*.key` |
| retention | `retention_days = 30` (never enforced) | `= 7`, blob content only, actually GC'd |
| unit | — | `ExecReload=/bin/kill -HUP $MAINPID` |

The old config is kept at `~/.config/kaed/config.toml.001.bak`.

**Root names changed.** A client holding `root: "home"` now gets
`not_found`. `roots` is discoverable, so an agent recovers in one call —
but a hardcoded root name anywhere needs updating.

**`~/.config/kaed/env` is now vestigial.** The unit still sources it and
`KAED_TOKEN_CLAUDE` still holds the same value, but nothing reads it —
`token_file` wins. It was left in place rather than deleted so this deploy
had no destructive step; safe to remove along with the unit's
`EnvironmentFile=` line whenever you like.

**No client-side action needed.** The token value is unchanged from the
2026-08-02 rotation, so whatever cleo has configured keeps working. (The
rotation *mechanism* was exercised during verification with a throwaway
token and then reverted through the grace window — see below.)

## Rotating a token, now that it doesn't break anything

```sh
cp  ~/.config/kaed/token ~/.config/kaed/token.prev   # open the grace window
printf '%s' "$NEW" > ~/.config/kaed/token
chmod 600 ~/.config/kaed/token ~/.config/kaed/token.prev
systemctl --user reload kaed                          # SIGHUP; no restart

# … clients pick up $NEW whenever they next restart …

rm ~/.config/kaed/token.prev                          # close the window
systemctl --user reload kaed
```

While the window is open, every request authenticated with the old token
logs:

```
WARN kaed::server: authenticated with the PREVIOUS token; this client has
not picked up the new one yet author="claude"
```

That line is how you know when it is safe to close the window. It is the
only signal available — 401s are rejected before the transaction layer, so
the journal never sees them.

## Verified after deploy (2026-08-02, live over ts.net)

- `check-config` clean; unit active; full MCP loop (roots → search → read →
  edit with diff proof → journal txn) green.
- **#913** — no token → `WWW-Authenticate: Bearer realm="kaed"` (RFC 6750
  §3.1, no error code); wrong token → `error="invalid_token",
  error_description="… kaed tokens do not expire"`, same sentence in the
  body.
- **#908 layer 1** — `../.config/kaed/env` and `../.ssh/id_ed25519` from
  both roots → `outside_root`; absolute paths refused.
- **#908 layer 2** — with `.env` and `.ssh/id_ed25519` planted *inside*
  `scratch`: `read`/`stat` → `denied` with the matching rule; `create`
  over `.env` → `denied`; a **nonexistent** `.env.nonexistent` → the same
  `denied` (no existence oracle); `list` returned only the allowed entries
  with `denied_hidden: 2`; `search` for the planted secret values returned
  **zero matches** with `denied_hidden: 2`, while still finding the
  allowed file that merely mentions `DB_PASSWORD`.
- **#910** — a forced `version_conflict` and the denied `create` both
  appear in `txn_failures` with author, paths, code and
  expected/actual versions. `SELECT author, count(*) … GROUP BY author`
  returns the conflict rate. Zero blobs written for either failure.
- **#909** — `journal.db`, `-wal` and `-shm` all `0600`.
- **#914** — rotated to a throwaway token and reloaded: new token worked,
  old token still worked (grace window, with the warn logged), then
  `rm token.prev` + reload killed the old one. `ExecMainStartTimestamp`
  was **identical before and after** — the process never restarted. The
  original token was then restored through the same grace-window
  procedure.

## Still to do at fleet-deploy time

Fold into the k-homelab recipe: the token-file layout, the `ExecReload`
line, and per-host roots. kubs0/kubsdb will need their own `allowed_hosts`
(the rmcp Host-validation gotcha from 001 is unchanged).
