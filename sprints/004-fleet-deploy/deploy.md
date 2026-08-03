# Sprint 004 deploy — the fleet

Two instances live as of 2026-08-03, both on build `19a8cb4`:

| Host | URL | Roots | Identity |
|---|---|---|---|
| kai | `https://kai.<tailnet>.ts.net:4870/mcp` | `src` = `~/src`, `scratch` = `~/scratch` | `claude` |
| kubs0 | `https://kubs0.<tailnet>.ts.net:4870/mcp` | `src` = `~/src`, `k-homelab` = `~/k-homelab` | `claude` |

kubsdb has **no instance** — deferred, korg #929, see
[decisions.md](decisions.md) D1.

(`<tailnet>` is deliberately not committed — klams knows it, as does
`tailscale status`.) [001's deploy notes](../001-walking-skeleton/deploy.md)
and [002's](../002-blast-radius-hardening/deploy.md) still apply except
where noted.

## How a host gets installed now

```sh
git clone <the repo> ~/src/ai/kaed && cd ~/src/ai/kaed
./deploy/install.sh                    # binary, unit, starter config
$EDITOR ~/.config/kaed/config.toml     # roots + allowed_hosts
./deploy/new-token.sh
kaed check-config
systemctl --user start kaed
sudo tailscale serve --bg --https=4870 localhost:4870
```

kubs0 was built from a checkout rather than a shipped binary, deliberately:
a tarball build has no `.git` and stamps `0.1.0 (unknown)`, which defeats
the whole point of #924. kubs0 cloned **from kai over ssh**
(`git clone ken@kai:/home/ken/src/ai/kaed`), so nothing had to be pushed to
the public remote to deploy.

Re-running `install.sh` is the upgrade path — it replaces the binary via
`mv` (no `ETXTBSY` against the running process) and restarts a live daemon.
kai was re-installed through it during this sprint, which is how the upgrade
path got exercised on a host that could be verified.

## Verified on kubs0 (2026-08-03, live over ts.net from kai)

- **`check-config`** — both roots resolved, identity resolved, 17 deny rules
  printed, and the build stamp as its first line.
- **Auth** — no token → `WWW-Authenticate: Bearer realm="kaed"`; wrong token
  → `error="invalid_token", error_description="… kaed tokens do not
  expire"`; right token → `406` (authenticated, MCP rejected an empty body).
- **Hairpin** — `406` from kubs0 to its own ts.net URL. See D4.
- **Deny list, all three enforcement points** — `read` on
  `k-homelab/secrets/index.yml` → `denied` naming the rule; `stat` on
  `secrets` → `denied`; `list` of the k-homelab root → `denied_hidden: 1`
  with `secrets` absent from the entries; `search` for `age1` across the
  whole root → **0 matches**, `denied_hidden: 1`.
- **Full edit loop** — `create` → `anchor_replace` with a correct base
  (diff returned) → forced `version_conflict` on a stale base, whose error
  carried `actual_version` and a `delta`.
- **Journal** — two `txns` rows with author `claude` and their intents; the
  conflict in `txn_failures` with both versions; `journal.db`, `-wal` and
  `-shm` all `0600`.

The probe file (`~/src/.kaed-verify-004.md`) was removed afterwards; its
create and edit remain in kubs0's journal, which is correct.

## A finding, fixed mid-deploy: `**/secrets/**` was the wrong pattern

kai has carried `deny = ["**/secrets/**", ...]` since sprint 002, and it was
copied into the new template. Verifying it on kubs0 showed it is **weaker
than intended**:

```
list  k-homelab/          →  `secrets` VISIBLE as a dir, denied_hidden absent
stat  k-homelab/secrets   →  allowed, returns mtime and size
list  k-homelab/secrets   →  entries: [], denied_hidden: 4
```

Contents were hidden, but the directory itself was not denied — because
`**/secrets/**` requires at least one path component *after* `secrets`, so
it never matches the directory node.

The correct pattern is the bare `**/secrets`, which is exactly the idiom
`deny.rs` documents for `**/.ssh`: *"a path is denied if it or any ancestor
matches, so `**/.ssh` covers everything beneath it and operators write the
natural pattern."* Ancestor-walking then covers the contents for free.

Changed on **both hosts** and in `deploy/config.example.toml`, which now
says why. Re-verified above: `stat` on `secrets` is now `denied`, and it is
gone from `list`.

This is the argument for `kaed check-config` printing every rule, and for
actually exercising the deny list on each host rather than trusting that a
config that looks right is right.

## Client wiring on cleo

cleo runs **Claude Code**, not Desktop Claude (no `%APPDATA%\Claude`), so
the config is `~/.claude.json` → `mcpServers`.

Per D3, the bare `kaed` entry was renamed and a second added:

| Before | After |
|---|---|
| `kaed` → kai | `kaed-kai` → kai |
| — | `kaed-kubs0` → kubs0 |

Verified end-to-end **using cleo's own stored credentials**, not a token
pasted in by hand: both entries return `406`. The old file is backed up at
`~/.claude.json.bak-004`; `klams` and `korg` entries are byte-identical
across the rewrite and all 31 top-level keys survived. The file grew from
42 KB to 101 KB purely because PowerShell's `ConvertTo-Json` pretty-prints —
same data, and Claude Code rewrites it in its own format anyway.

**Restart any Claude Code session on cleo** to pick this up; clients load
MCP config only at session start.

The kubs0 token went kubs0 → kai → cleo without ever being printed, and the
staging copies on both ends were deleted.

## Still to do

- **k-homelab #926** — the `kaed-service` recipe, per-host manifests and the
  `tailscale_serve` entries (kai's has never been declared, #907). That is a
  different repo; run `/start-sprint korg:928` from its checkout on kubs0.
  Everything the recipe needs to assert now exists: a canonical installer to
  point at, `kaed --version` for the freshness floor, and `check-config` as
  a post-condition.
- **korg #929** — revisit kubsdb once kai and kubs0 have runtime.
