# Sprint 008 deploy — the secrets model goes live

Deployed **2026-08-07** from merged `main` (`a9b8ea0`), published as
`0.1.0-a9b8ea0` and installed on both hosts from the package store.

| Host | `kaed --version` | unit | MCP `serverInfo.version` | rollback |
|---|---|---|---|---|
| kai | `0.1.0 (a9b8ea0 2026-08-06)` ✔ matches `$V` | active | same stamp ✔ | `kaed.prev` = `0.1.0-2ce8cc0`, or `--version 0.1.0-2ce8cc0` |
| kubs0 | `0.1.0 (a9b8ea0 2026-08-06)` ✔ matches `$V` | active | same stamp ✔ | same |

kubsdb: still no instance, still correct (korg #929) — and this sprint is a
prerequisite of the decision that could change that.

## What changed on upgrade, verified live

**The behavior flip this sprint exists for:** `.env`-shaped files went from
`denied` to redacted-readable on both hosts, with **no config edit** —
classify defaults are on, and `check-config` now prints the classify rules
beside the deny rules on both hosts.

Smoke test against kai's production MCP endpoint (real URL, real auth, a
real `.env` under `kai:scratch`), full session: initialize →
notifications/initialized → `tools/call read`. Asserted on the live
response:

- the plaintext value appears **nowhere** in the response;
- `redacted: true`, with the sealed `⟨kaed:SMOKE_TOKEN@…⟩` placeholder
  (digest disclosed — the value cleared the entropy floor) and
  `⟨kaed:DEBUG⟩` withheld below it;
- the typed `dotenv` view with `meta.shape: "hex"`;
- the `usage_hint` (`set -a; . <file>; set +a`).

The gitignore warning did not fire — correct, `~/scratch` is not a git repo.
The smoke file was removed after the test.

## The journal migration ran on both hosts

Both live `journal.db`s predate `blobs.redacted`. On restart each daemon
logged:

```
INFO kaed::journal: journal migrated: blobs.redacted added (sprint 008)
```

one line per host, and both units came up clean afterward — the ALTER ran
once, on real data, exactly as the migration test predicted. Pre-008 blob
rows read back `redacted = 0`, which is the signal the #1049 history tools
must key redact-on-read from (D-11 corollary).

## Notes

- Token verification for the MCP round trip must use **each host's own
  token** — kai's token got an empty answer from kubs0 (a 401 wearing a
  grep that found nothing), and running the same probe on kubs0 with its
  local token matched immediately. Not a deploy fault; worth remembering.
- `install.sh` left both configs untouched as designed; no `[peers]` work
  was needed (both hosts have their block since 007).
