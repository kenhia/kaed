---
name: deploy-fleet
description: Build and install kaed from committed main onto every host in the fleet, then verify each one is actually running that build. Use when asked to deploy/redeploy/ship kaed, or when sprint-ship reaches Phase 7. Deploys committed code only.
---

# Deploy kaed to the fleet

Installs kaed on each fleet host from **committed `main`**, using the repo's
own `deploy/install.sh`, then proves each host is running the build you think
it is.

There is no artifact registry and nothing is copied host-to-host: every host
builds from its own checkout of the same commit. That is what makes
`kaed --version` trustworthy, which is the entire point of the version stamp
(korg #924).

## The fleet

| Host | Roots | Notes |
|---|---|---|
| `kai` | `src`, `scratch` | the usual build/orchestration host |
| `kubs0` | `src`, `k-homelab` | `secrets/` denied |

**kubsdb is deliberately NOT in the fleet** — deferred, korg #929. If you find
no kaed there, that is correct and expected, not a broken rollout. Do not
"fix" it by installing one; that needs a broad-access decision first, on a
host whose roots would sit next to `/datastore/*` and `/gratch`.

> This table is the third place the fleet is written down (the others are
> klams and `sprints/004-fleet-deploy/deploy.md`). **korg #930** is about
> collapsing those into one declared-vs-observed source; when it lands, this
> section should read from that instead of restating it.

## Deploys are clean-tree, committed-main only

**Refuse to deploy from a dirty tree or a detached/feature branch.** `install.sh`
builds from whatever is checked out, and `build.rs` stamps the binary with
`git describe --always --dirty`. A dirty deploy produces a binary stamped
`-dirty`, which names no reproducible commit — and a host running an
unidentifiable build is exactly the state sprint 004 existed to end.

Equally: **deploy after a merge, not from a feature branch.** A branch commit
disappears from `main`'s history when the branch is squash-merged, leaving
hosts reporting a SHA that is not on `main`.

## The tailnet name is not in this repo

kaed is public; the tailnet name is deliberately uncommitted. Resolve it at
run time and never write it into a file or a commit message:

```sh
TN=$(tailscale status --json | grep -o '"MagicDNSSuffix":"[^"]*"' | head -1 | cut -d'"' -f4)
```

If that comes back empty, get it from `tailscale status` output or klams —
do **not** guess, and do not fall back to hardcoding it.

## Procedure

Run from a checkout on `kai`. `$SHA` below is the full commit being deployed.

### 1. Preflight

```sh
git -C ~/src/ai/kaed status --short          # must be empty
git -C ~/src/ai/kaed rev-parse --abbrev-ref HEAD   # must be main
git -C ~/src/ai/kaed pull --ff-only origin main
SHA=$(git -C ~/src/ai/kaed rev-parse HEAD)
```

Stop and ask if the tree is dirty or the branch is not `main`. Never stash.

### 2. Per host, in order: kai first, then kubs0

kai first — it is the host you can debug fastest, so if the build is broken
you find out somewhere convenient.

For each host, get its checkout onto `$SHA` and install:

```sh
# on the host (locally for kai; over ssh for the others)
cd ~/src/ai/kaed
git fetch origin
git checkout main
git pull --ff-only origin main
git rev-parse HEAD          # must equal $SHA
./deploy/install.sh
```

`install.sh` is idempotent, rebuilds `--release`, keeps the outgoing binary
as `~/.local/bin/kaed.prev` for rollback, and restarts the unit if it was
already running. It never touches config or the token.

**If a host's `git pull` is not a fast-forward, stop and report it.** Someone
has committed on that host's checkout; resolving that silently is how you
lose their work.

### 3. Verify each host — all four, not just the first

The unit being active proves a kaed is running. It does not prove it is
*this* kaed. Check all of these:

```sh
kaed --version                     # 1. must contain $SHA's short hash, and NOT -dirty
kaed check-config                  # 2. must exit 0; read the roots and deny rules
systemctl --user is-active kaed    # 3. active
```

For (1), assert the reported hash is a real prefix of `$SHA` rather than
eyeballing it — `git describe`'s abbreviation length and `rev-parse --short`
need not agree:

```sh
HASH=$(kaed --version | sed 's/.*(\([0-9a-f]*\).*/\1/')
case "$SHA" in "$HASH"*) echo "match" ;; *) echo "MISMATCH: binary is $HASH, expected $SHA" ;; esac
```

4. **One authenticated MCP round trip over the host's real URL**, which is the
   only check that exercises auth, the `Host` allowlist and the transport
   together:

```sh
curl -s -X POST "https://<host>.${TN}:4870/mcp" \
  -H "Authorization: Bearer $(cat ~/.config/kaed/token)" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"deploy-verify","version":"0"}}}'
```

The response's `result.serverInfo.version` must be the same stamp
`kaed --version` printed. That is the check that closes the loop: the binary
on disk and the server answering the network are the same build.

**Never print the token.** Read it in a subshell as above; do not echo it,
and do not pass it anywhere it will be logged.

### 4. Report

A table per host: commit deployed, `kaed --version`, unit state, MCP
`serverInfo.version`. Name anything that did not match.

## Rollback

A failed deploy does not roll back a merge — the code is fine, the rollout
isn't. Per affected host:

```sh
mv -f ~/.local/bin/kaed.prev ~/.local/bin/kaed
systemctl --user restart kaed
kaed --version        # confirm you are back on the previous build
```

`kaed --version` on `kaed.prev` tells you which build you are rolling back
*to* before you commit to it. If `.prev` is missing (first install on that
host), the rollback is `git checkout <previous-sha> && ./deploy/install.sh`.

Roll back the *broken* hosts only. A half-deployed fleet is a normal state
here — the hosts are independent, and there is no schema or protocol
migration coupling them.

## What this skill does not do

- **Config.** Roots, `allowed_hosts` and deny rules are per host and are not
  in this repo. `install.sh` writes a starter config only when none exists.
  Once k-homelab #926 lands, its recipe asserts those; until then they are
  hand-maintained and this skill must not touch them.
- **Tokens.** Never generated, never rotated, never read except to
  authenticate a verification call. Rotation is `deploy/new-token.sh --rotate`
  and is a deliberate, separate act.
- **`tailscale serve`.** Set once per host at first deploy; k-homelab's
  `tailscale-serve` recipe owns it.
- **Client wiring.** Adding a host to a client's MCP config is separate, and
  it is genuinely dangerous — read the warning in `docs/setup.md` §7 before
  touching a client config from a script. It cost this project every MCP
  server on cleo once (korg #931).
