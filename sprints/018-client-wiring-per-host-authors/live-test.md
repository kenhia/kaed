# Sprint 018 — live test on kai and kubs0

*2026-08-16. All three hosts on kaed `0.1.0 (af51376 2026-08-12)`. Claude
Code `2.1.233` on kai and kubs0 — i.e. ≥ 2.1.227, the version that
validates against `2026-07-28` and so would have hit sprint 015's
zero-tools failure had 016 not shipped.*

## Verdict

**All five gate items from #1350 pass. Nothing filed.** Second live test in
this program to find nothing — 014 was the first.

Two instruments were used deliberately, because they fail differently:
`probe.py` (raw JSON-RPC, precise) and real headless Claude Code sessions
(the client half). See D-4.

## Gate item 1 — `roots` lists 7 roots, all three hosts verified

Run as **both** new identities against kai's gateway:

| Identity | roots | kai | kubs0 | kubsdb |
|---|---|---|---|---|
| `claude-kai` | 7 | `verified` (self) | `probe: ok`, `verified` | `probe: ok`, `verified` |
| `claude-kubs0` | 7 | `verified` (self) | `probe: ok`, `verified` | `probe: ok`, `verified` |

`kai:src, kai:scratch, kubs0:src, kubs0:k-homelab, kubsdb:datastore,
kubsdb:hvsim, kubsdb:src`.

This one call is load-bearing beyond its own gate line: `roots` probes
peers **under the caller's credential**, so a `probe: ok` for `claude-kai`
is direct evidence that kubs0 and kubsdb accept that author's token. The
peer-credential half of the wiring is proven here, before any edit.

## Gate item 2 — an edit lands on each other host

Four edits, each made as the identity that will really make it, all through
kai's gateway:

| Direction | Identity | Path | txn | applied |
|---|---|---|---|---|
| kai → kubs0 (proxied) | `claude-kai` | `kubs0:src/.kaed-018-from-kai.md` | 19 | yes |
| kai → kubsdb (proxied) | `claude-kai` | `kubsdb:src/.kaed-018-from-kai.md` | 7 | yes |
| kubs0 → kai (direct) | `claude-kubs0` | `kai:scratch/.kaed-018-from-kubs0.md` | 76 | yes |
| kubs0 → kubsdb (proxied) | `claude-kubs0` | `kubsdb:src/.kaed-018-from-kubs0.md` | 8 | yes |

All four verified **on disk over ssh**, not just by the tool's own success
report — this whole route exists to replace a mechanism whose failure mode
was a write that reported success and did not land, so taking the tool's
word for it would have been the wrong test.

Note the txn ids: 19 on kubs0, 7 and 8 on kubsdb, 76 on kai. Each backend
numbers its own, which is the first hint that item 3 holds.

## Gate item 3 — attribution, and no double-journaling on kai

| Host's journal | Rows from this test |
|---|---|
| kubs0 | `author=claude-kai txn=19 root=kubs0:src` |
| kubsdb | `author=claude-kai txn=7`, `author=claude-kubs0 txn=8` |
| kai | `author=claude-kubs0 txn=76 root=kai:scratch` — **and nothing else** |

Both halves pass:

- **Attribution**: every row names the per-host author. No bare `claude`.
- **No double-journal**: kai's journal holds only the edit addressed to
  kai's own root. The two edits kai *proxied* appear on the backends and
  nowhere on the gateway — D-7 from sprint 010, confirmed live rather than
  assumed. Each host's journal remains the only record of its own edits.

## Gate item 4 — a classified file reads redacted

| Call | Result |
|---|---|
| `read kai:src tools/kfdc/.env` as `claude-kai` (direct) | ok, 4 sealed placeholders |
| `read kubs0:src ai/klams/.env` as `claude-kai` (proxied) | ok, 5 sealed placeholders |
| `read kubsdb:datastore redis/docker-compose.yml` as `claude-kubs0` (proxied) | `denied`, `reason: not_readable_by_service_identity` |

Placeholders carry digests where the value clears the entropy floor and
omit them where it does not (`⟨kaed:KORG_URL@cd10d0e2f85fa2e5⟩` vs
`⟨kaed:KFDC_STORE_HOST⟩`) — PD-2 behaving as designed, through the proxy as
well as directly.

The third row is not a failure. It is 014's finding holding: those five
kubsdb files are `0600 root:root`, so the OS refuses before kaed can read
bytes to classify them, and the classify globs stay dormant policy that
arms itself if a mode ever changes. Included here because a new identity
was the plausible thing to have changed it, and it did not.

## Gate item 5 — `bin/audit … kaed-service` is `ok`

`bin/audit kai kaed-service` → `ok`. `bin/audit kubs0 kaed-service` → `ok`.
(kai's run also surfaced six pre-existing `github-repos` advisories about
stale clones, unrelated to this work and untouched.)

### Beyond the gate: apply-safety proven, not inferred

The WI listed this item because `kaedconf.py` deleting hand-added
`[peers.<host>.tokens]` was a blocker until korg #1072 fixed it. An `ok`
audit only reports *no drift detected*, which is weaker than the claim
being relied on. So the real thing was run:

```
md5 before apply:  ce2d96a53cdaf8717d654308e166efb2
bin/apply kai kaed-service  ->  ok
md5 after apply:   ce2d96a53cdaf8717d654308e166efb2
```

Byte-identical, all six new entries present, `check-config` still resolves
all three identities. The preservation claim is now an observation.

## The client half — fresh Claude Code sessions

Sprint 015's failure was Claude Code connecting and registering **zero**
tools, which `claude mcp list` reported as healthy. So connection status is
not evidence; the session has to list tools and then use one.

Both run headless (`claude -p --allowedTools`), which is a genuinely fresh
session and therefore genuinely reloads MCP config:

| Host | Entry | `roots` | Edit |
|---|---|---|---|
| kai | `kaed-kai` → `127.0.0.1:4870` | `HOST=kai, ROOTS=7, VERIFIED=kai,kubs0,kubsdb` | created `kubs0:src/.kaed-018-client-kai.md`, txn 20 |
| kubs0 | `kaed-kai` → kai over the tailnet | `HOST=kai, ROOTS=7, VERIFIED=kai,kubs0,kubsdb` | created `kubsdb:src/.kaed-018-client-kubs0.md`, txn 9 |

kubs0's session reporting `HOST=kai` is the confirmation it is on the
gateway and not a local instance.

Journals afterwards: `claude-kai txn=20` on kubs0, `claude-kubs0 txn=9` on
kubsdb. Both files verified on disk. A Claude Code session on kubs0 edited
a file on kubsdb, through kai, attributed to kubs0 — which is the entire
point of the sprint, observed end to end.

## Protocol check (raw, per CLAUDE.md's rule)

`tools/list` at `2026-07-28` as each new identity: **12 tools** (`diff,
edit, feedback, journal, list, read, revert, roots, search, secret,
secret_reveal, stat`), result carrying `ttlMs=3600000`,
`cacheScope=public`, `resultType=complete`. 015's failure mode is not
lurking for the new identities.

## Cleanup

All eight test files removed from kubs0, kubsdb and kai; absence verified.
The journal rows remain, deliberately — they are the evidence.
