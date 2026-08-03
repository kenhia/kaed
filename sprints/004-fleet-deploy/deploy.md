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

The kubs0 token went kubs0 → kai → cleo without ever being printed, and the
staging copies on both ends were deleted.

**Restart any Claude Code session on cleo** to pick this up; clients load
MCP config only at session start.

### This step broke cleo, and the fix is below (korg #931)

The content of that edit was right. **The encoding was not, and it cost
cleo every MCP server it had.** Written up as D5 in
[decisions.md](decisions.md); the short version:

The write used PowerShell's `Set-Content -Encoding UTF8`, which on Windows
PowerShell 5.1 means UTF-8 **with BOM**, plus CRLF endings. `JSON.parse`
rejects a leading BOM, so Claude Code could not read its own config, moved
it aside to `.claude.json.backup` and regenerated a default — dropping
`klams`, `korg`, `kaed-kai` and `kaed-kubs0` in one go, and two of three
`projects` entries. klams and korg were working before the deploy and dead
after it; the sprint had no business touching either.

The failure was silent in both directions: nothing complained at write time,
and the client presented the result as "no MCP servers configured" rather
than as a broken file. What made it *look* fine at the time was that the
sanity check I ran after writing re-read the file **through PowerShell**,
which strips the BOM transparently — so it parsed, all 31 keys were there,
and every server checked out. Only a byte-level look would have caught it.

**Repaired 2026-08-03.** `.claude.json.backup` was the 17:55 write: correct
content, wrong encoding. Stripping the BOM, normalising to LF and writing
with an explicit `UTF8Encoding($false)` restored all four servers and all
three projects; the one key Claude Code had added since
(`hasResetAutoModeOptInForDefaultOffer`) was carried forward, and the
regenerated file was kept as `.claude.json.pre-repair-*`. Verified at byte
level (no BOM, no CRLF), parse level, and content level, then all four
servers were sent a real MCP `initialize` **using cleo's own stored
credentials** — four `200`s. kaed's reply also confirms the #924 stamp
reaches clients:

```
serverInfo: {name: kaed, version: "0.1.0 (19a8cb4 2026-08-03)"}
```

**So it cannot recur:** [`deploy/check-client-config.ps1`](../../deploy/check-client-config.ps1)
checks a client config for a BOM, CRLF, parseability and the servers you
expect, and exits non-zero otherwise. Demonstrated rather than asserted —
run against the actual corrupted artifact it exits 1 with
`FAIL: UTF-8 BOM present`, against the regenerated default it exits 1 with
`FAIL: no mcpServers key`, and against the repaired file it exits 0.
`docs/setup.md` §7 now carries the trap and the correct write incantation,
and says to prefer `claude mcp add` over hand-writing the file at all.

(`claude mcp add` was the better fix and was tried first — cleo has no
`claude` CLI on PATH, it runs the desktop/extension install, so a direct
write was genuinely unavoidable here.)

## Still to do

- **k-homelab #926** — the `kaed-service` recipe, per-host manifests and the
  `tailscale_serve` entries (kai's has never been declared, #907). Different
  repo; it has its own proposal now, **korg:932** — run `/start-sprint
  korg:932` from the k-homelab checkout on kubs0. Everything the recipe needs
  to assert now exists: a canonical installer to point at, `kaed --version`
  for the freshness floor, and `check-config` as a post-condition.
- **korg #929** — revisit kubsdb once kai and kubs0 have runtime.

---

## Deployed 2026-08-02 — merged `main`, commit `b08ce9f`

Sprint 004 shipped as PR #4, squash-merged to `b08ce9f`, then redeployed to
both hosts from merged `main` via `/sprint-ship` Phase 7 → the `deploy-fleet`
skill. **This redeploy was the point of wiring Phase 7 at all:** both hosts
had been running `19a8cb4`, a feature-branch commit that ceased to exist on
`main` at the squash-merge, so they would have been reporting a SHA no audit
could resolve — on the very sprint that added the stamp to prevent that.

| Host | Commit | `kaed --version` | Unit | MCP `serverInfo.version` |
|---|---|---|---|---|
| kai | `b08ce9f` | `0.1.0 (b08ce9f 2026-08-02)` | active | `0.1.0 (b08ce9f 2026-08-02)` |
| kubs0 | `b08ce9f` | `0.1.0 (b08ce9f 2026-08-02)` | active | `0.1.0 (b08ce9f 2026-08-02)` |

Neither build `-dirty`; the reported hash was asserted to be a real prefix of
`HEAD` rather than eyeballed. Every host built from its own checkout of the
same commit — nothing was copied host-to-host, which is what makes the stamp
worth trusting.

**Rollback target:** `~/.local/bin/kaed.prev` on each host (`mv -f` it back
and `systemctl --user restart kaed`). Both currently hold the `19a8cb4`
build. Roll back only the broken host — the two are independent, with no
schema or protocol coupling.

**Verified live beyond "it's up":**

- The stamp reaches clients over MCP, not just the CLI — the handshake and
  `kaed --version` agree on both hosts, which is what proves the binary on
  disk and the server on the network are the same build.
- The sprint's deny-glob fix, on the deployed build: `stat` on
  `k-homelab/secrets` → `denied` naming rule `**/secrets`, and the root
  listing shows `denied_hidden: 1` with `secrets` absent.
- No schema or data migration in this sprint, but the journals survived the
  restarts intact and still `0600`: kai 7 txns / 2 failures, kubs0 2 txns /
  1 failure.
