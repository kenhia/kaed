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

## Binary rollout — pending

`just publish` + `install.sh --from-store` per host at ship time. Two things
to check afterwards, in this order:

1. `kaed check-config` on each host emits **no** grace-window warning. Before
   this sprint's config rollout it would have named two identities on kai and
   three on each backend; silence is the verification.
2. `kaed-new-token --identity claude-kai --close` on each host. With no
   rotation in flight it must fail with

   ```
   ERROR: no /home/ken/.config/kaed/token-claude-kai.prev — no grace window is open
   ```

   which is the read-only probe worth running: it changes nothing, and the
   path in the message proves `--identity` resolved that identity out of the
   host's real `config.toml` rather than guessing a filename. A message
   naming plain `token.prev` would mean the parser missed the entry.

Do **not** rotate anything as a deploy check. The live rotation is krot's
scheduled 2026-08-21 test, which this sprint exists to precede.
