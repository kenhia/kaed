# Sprint 007 deploy — the rename, and the first fleet that declares itself

Deployed **2026-08-07** from merged `main` (`2ce8cc0`), published as
`0.1.0-2ce8cc0` and installed on both hosts from the package store. Store
path unchanged from 005; `latest` moved because this was published from
`main`.

| Host | `kaed --version` | unit | MCP `serverInfo.version` | roots | rollback |
|---|---|---|---|---|---|
| kai | `0.1.0 (2ce8cc0 2026-08-06)` ✔ matches `$V` | active | same stamp ✔ | `kai:src`, `kai:scratch` | `kaed.prev` = `0.1.0-b08ce9f`, or `--version 0.1.0-b08ce9f` |
| kubs0 | `0.1.0 (2ce8cc0 2026-08-06)` ✔ matches `$V` | active | same stamp ✔ | `kubs0:src`, `kubs0:k-homelab` | same |

kubsdb: still no instance, still correct — and now it *says so* on both hosts
rather than being an absence you have to already know about.

## The rename needed no config edit, which was the point

`[server] host` was left unset on both hosts, so kaed fell back to the short
system hostname exactly as D-1 intended:

```
INFO kaed::config: serving roots under this host name host="kai" configured=false
```

That is the whole zero-touch upgrade: `install.sh` never overwrites an
existing config, and every root on both hosts became host-qualified on
restart with nobody editing anything. Both hosts log which name they resolved
and whether it came from config, so a wrong prefix would be visible rather
than silently baked into every journal row.

## The `[peers]` block, added by hand — and why that step exists

`install.sh` correctly refused to touch either config and said so:

```
config exists, left untouched: /home/ken/.config/kaed/config.toml
no [peers] in /home/ken/.config/kaed/config.toml: this host declares no fleet, so
  `roots` reports fleet.declared=false. See config.example.toml.
```

That warning is new in this sprint and it did its job on the first real
deploy: a green install that left the sprint's headline feature inert
announced itself instead of looking finished.

The block was then added per host, **kubs0 first** at Ken's direction, with a
backup each. Both configs were written whole via `base64 -w0 | ssh 'base64
-d'` and **verified by md5 against the local copy** before anything was
restarted — a write that reports success and did not land is the failure mode
here, not a hypothetical one.

- Backups: `~/.config/kaed/config.toml.004.bak` on both, following the
  convention 002 set (`.001.bak` = the config as of sprint 001 — the number
  names the sprint the snapshot belongs to, not a counter). Both outgoing
  configs dated to sprint 004.
- File modes survived the whole-file replace: kubs0 stayed `0600`, kai
  `0664`.
- `kaed check-config` was run and had to exit 0 **before** either restart.

**`[peers]` needs a restart, not a reload.** SIGHUP re-reads token files only
— `AuthState::reload` calls `resolve_identities`, nothing else — so roots and
peers are startup-only. A `systemctl --user reload` would have looked like it
worked and changed nothing. Noted in the block's own comment on both hosts so
the next person does not have to rediscover it.

**k-homelab's `kaed-service` recipe leaves it alone.** It asserts `bind`,
`allowed_hosts`, `roots`, `[security] deny` and `journal.retention_days`, and
splices only those spans; `[peers]` and `[server] host` are tables it does not
know about. Checked in `recipes/kaed-service/kaedconf.py` on kubs0 before
shipping rather than discovered by an `apply`. No manifest change was needed
for the rename either, because the recipe keys roots on their *local* name —
which is the second reason D-1's split was right.

## Verified live, against the real URLs

Beyond the deploy skill's four checks, the three things this sprint actually
delivers were exercised over `https://<host>.<tailnet>:4870/mcp`:

**The fleet is discoverable** (`roots` on kai; kubs0 returns the mirror):

```
host: kai | roots: ['kai:src', 'kai:scratch']
fleet.declared: True
  kai      active      self=True  verified=True  ref=-
  kubs0    active      self=False verified=False ref=-
  kubsdb   deferred    self=False verified=False ref=korg:929
```

`verified=False` on both peers is the honest part: kaed declares, it does not
probe. That is #930's remaining half, and it is now visible in the response
rather than assumed away.

**The deferred host, on the error path** — `root: "kubsdb:data"`:

> host "kubsdb" deliberately does not run kaed (korg:929): broad-access design
> not settled; roots would sit next to /datastore/\* and /gratch. Hosts the
> package store, which is not the same as running kaed. — this is a recorded
> decision, not a broken deploy, so do not install one there

with `data.reason: host_deferred`. This is the sentence #930 exists for, and
it arrives on the path the session that filed it was actually on.

**The pre-007 spelling names its replacement** — `root: "src"` →
*"root names are host-qualified since sprint 007: pass "kai:src", not "src""*,
`did_you_mean: "kai:src"`.

**A declared-but-unroutable peer says which** — `root: "kubs0:src"` →
*"in this host's declared fleet, but kai does not proxy to peers yet
(korg:1050) — connect to kubs0's own kaed"*.

**#1066's incident, reproduced verbatim against the real repo** —
`search {root: "kai:src", path: "ai/kaed", glob: "README.md", pattern: "journal"}`:

```
matches: 0  files_searched: 0
reason: glob_matched_no_files
"0 of 62 scanned entries matched: `glob` is matched against ROOT-relative
 paths and is not re-anchored by `path`, so with path "ai/kaed" the glob
 "README.md" could only ever match at the root's top level — try
 "ai/kaed/README.md" or "**/README.md"."
```

The suggested glob then returns **16 matches in 2 files**. In sprint 006 the
first call returned a bare empty list and the conclusion drawn was "no repo
docs mention the journal".

## No client change was needed

Root names are per-call arguments, not client config — cleo's `~/.claude.json`
holds only URLs and tokens under `kaed-kai` / `kaed-kubs0`, so the #931 hazard
was never touched. A connected agent learns the new vocabulary from the
handshake `instructions`, which now lead with it. Any session still holding
`src` gets the `did_you_mean` above rather than a dead end.

## What is still open

- **The observed half of #930.** Peers are declared, never probed. #1050.
- **kubsdb** (#929) — unchanged, and now declared rather than merely absent.
