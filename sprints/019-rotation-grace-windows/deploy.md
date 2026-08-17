# Sprint 019 — deploy

Two halves, deliberately separate. The **config** half needs no new binary
(`prev_token_file` has been honoured since sprint 002) and went out during
the sprint. The **binary** half — the startup warning and the rewritten
`kaed-new-token` — ships with the normal fleet deploy at `/sprint-ship`
Phase 7, from merged `main`.

## Config rollout — done 2026-08-17

Every `[auth]` identity on every host now declares `prev_token_file`,
pointing at `<token_file>.prev`. Eight entries changed; kai's `claude`
already had one.

| Host | Identities changed | Route |
|---|---|---|
| kai | `claude-kai`, `claude-kubs0` | local edit (kaed refuses its own config dir by design) |
| kubs0 | `claude`, `claude-kai`, `claude-kubs0` | `base64 -w0 … \| ssh … 'base64 -d > …'`, digest-verified |
| kubsdb | `claude`, `claude-kai`, `claude-kubs0` | same |

`config.toml.019.bak` on each host, beside the earlier `.001/.004/.018`
backups.

The remote writes were verified by `md5sum` on both ends before the restart,
per the rule that a write reporting success and not landing is the named
failure mode of that route. Diffs were reviewed against the live file first
and were the intended change and nothing else.

**Restart, not reload.** `prev_token_file` changes the config *shape*, and
SIGHUP only re-reads token files for identities already known — a reload
would have reported success and changed nothing. Backends first, gateway
last:

```sh
ssh kubs0  'systemctl --user restart kaed'
ssh kubsdb 'systemctl --user restart kaed'
systemctl --user restart kaed          # kai
```

### Verified live

- `kaed check-config` on all three hosts: **9 of 9 identities resolve** after
  the restart — the change did not cost a credential.
- `roots` through the kai gateway: both peers `probe: ok`, `verified: true`,
  all seven roots addressable. This is the part worth having checked — the
  six gateway-consumed credentials cross a restart on both ends, and a
  proxied call is the only thing that exercises them together.
- No `.prev` file exists anywhere. That is correct and is the point: the
  *slot* is declared, the file appears only while a rotation is in flight.
  Declaring the slot is inert until then, which is why it can be rolled out
  ahead of need.

## Binary rollout — done 2026-08-17, `0.1.0-9518bff`

Published from merged `main` (`9518bff`) and installed from the store on all
three hosts — kai first, then kubs0 and kubsdb, each bootstrapping the
installer from the artifact and checking it against `SHA256SUMS`.

| Host | Installed | `kaed --version` | Unit | MCP `serverInfo.version` |
|---|---|---|---|---|
| kai | `0.1.0-9518bff` | match | active | `0.1.0 (9518bff 2026-08-16)` |
| kubs0 | `0.1.0-9518bff` | match | active | `0.1.0 (9518bff 2026-08-16)` |
| kubsdb | `0.1.0-9518bff` | match | active | `0.1.0 (9518bff 2026-08-16)` |

Rollback target: `0.1.0-aeae142` (sprint 018), still in the store and on each
host as `~/.local/bin/kaed.prev`.

### Verified live — the sprint's own behaviour, not just a healthy service

1. **The warning is silent fleet-wide.** `check-config` on all three hosts
   emits zero `HARD CUT` lines, with 3 of 3 identities resolving on each. The
   check is not vacuous: run against kai's pre-fix config during the sprint
   the same code named `["claude-kai", "claude-kubs0"]`, and each backend
   would have named all three. Silence *is* the verification.
2. **`--identity` resolves each host's real config.** The read-only probe
   `kaed-new-token --identity claude-kai --close` fails on every host with

   ```
   ERROR: no /home/ken/.config/kaed/token-claude-kai.prev — no grace window is open
   ```

   naming the identity-specific path. A message naming plain `token.prev`
   would have meant the parser missed the entry and fell back — it does not
   fall back, and this proves it on the live configs rather than on fixtures.
3. **Nothing was rotated**: zero `.prev` files on any host afterwards, which
   is also the state that makes claim 2 a *refusal* rather than a deletion.
4. **The gateway still proxies.** `roots` through kai reports both peers
   `probe: ok`, `verified: true`, all seven roots addressable, every host
   reporting `9518bff`. This is the check that covers the six
   gateway-consumed credentials end to end.

No rotation was performed as a deploy check, deliberately. The live rotation
is krot's scheduled 2026-08-21 test, which this sprint exists to precede.
