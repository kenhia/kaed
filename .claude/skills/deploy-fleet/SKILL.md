---
name: deploy-fleet
description: Publish kaed from committed main to the homelab package store, install that published version on every host in the fleet, then verify each one is actually running that build. Use when asked to deploy/redeploy/ship kaed, or when sprint-ship reaches Phase 7. Deploys committed code only.
---

# Deploy kaed to the fleet

**Build once, publish it, install that.** `just publish` puts a versioned
deploy bundle in the homelab package store; every host then installs *that
artifact* with `deploy/install.sh --from-store`. No host needs a checkout or
a Rust toolchain, and the thing you verified on kai is byte-for-byte the
thing kubs0 runs.

This replaced build-from-clone in sprint 005 (korg #1015). The old model —
every host builds its own copy of the same commit — is the "pinned clone"
k-homelab `docs/deploying.md` exists to end, and it stopped being possible
anyway when kubs0's clone was deleted.

## The fleet

| Host | Roots | Notes |
|---|---|---|
| `kai` | `src`, `scratch` | the build/publish host — the only clone |
| `kubs0` | `src`, `k-homelab` | `secrets/` denied; **no checkout** |

**kubsdb is deliberately NOT in the fleet** — deferred, korg #929. If you find
no kaed there, that is correct and expected, not a broken rollout. Do not
"fix" it by installing one; that needs a broad-access decision first, on a
host whose roots would sit next to `/datastore/*` and `/gratch`. (kubsdb
*hosts* the store; that is a different thing from running kaed.)

> This table is the third place the fleet is written down (the others are
> klams and `sprints/004-fleet-deploy/deploy.md`). **korg #930** is about
> collapsing those into one declared-vs-observed source; when it lands, this
> section should read from that instead of restating it.

## Publish from clean, committed `main` — never a branch

`just publish` **refuses a dirty tree**, and refuses a binary stamped
`-dirty` or `unknown`: a published version must name a commit, or the
artifact is a rollback target nobody can reproduce.

It also **will not move `latest`** when you are not on `main`. A branch
commit disappears from `main`'s history at squash-merge, leaving hosts
reporting a SHA that is on no branch — which is the state sprint 004 existed
to end. So: **deploy after the merge, not from the feature branch.**

## The tailnet name is not in this repo

kaed is public; the tailnet name is deliberately uncommitted. Resolve it at
run time and never write it into a file or a commit message:

```sh
TN=$(tailscale status --json | grep -o '"MagicDNSSuffix":"[^"]*"' | head -1 | cut -d'"' -f4)
STORE="https://kubsdb.${TN}:4880"
```

If `TN` comes back empty, get it from `tailscale status` output or klams —
do **not** guess, and do not fall back to hardcoding it.

## Procedure

Run from the checkout on `kai`.

### 1. Publish

```sh
git -C ~/src/ai/kaed status --short                 # must be empty
git -C ~/src/ai/kaed rev-parse --abbrev-ref HEAD    # must be main
git -C ~/src/ai/kaed pull --ff-only origin main
cd ~/src/ai/kaed && just publish
```

Stop and ask if the tree is dirty or the branch is not `main`. Never stash.

`publish` prints the version it published — capture it, and pin every host
to it explicitly rather than letting each resolve `latest` independently:

```sh
V=$(curl -fsS "$STORE/artifacts/kaed/latest")       # or the printed version
```

The bundle is the binary plus `install.sh`, `kaed.service`,
`config.example.toml` and `new-token.sh`, under one `SHA256SUMS`.

### 2. Install on each host, kai first, then kubs0

kai first — it is the host you can debug fastest, so if something is wrong
you find out somewhere convenient.

**kai** (has the checkout, but still installs from the store — the doctrine
is *every* install pulls from the store, local ones included):

```sh
cd ~/src/ai/kaed
./deploy/install.sh --from-store --store "$STORE" --version "$V"
```

**kubs0** (no checkout — bootstrap the installer from the artifact itself,
verified; do not pipe curl into sh):

```sh
ssh kubs0 "set -eu
  cd \$(mktemp -d)
  base='$STORE/artifacts/kaed/$V'
  curl -fsS -O \"\$base/install.sh\"
  curl -fsS \"\$base/SHA256SUMS\" | grep ' install.sh\$' | sha256sum -c -
  sh install.sh --from-store --store '$STORE' --version '$V'"
```

`install.sh` is idempotent, verifies every fetched file against the
published `SHA256SUMS`, asserts the binary reports the version it was
published under, keeps the outgoing binary as `~/.local/bin/kaed.prev`, and
restarts the unit if it was already running. It never touches config or the
token.

Add `--dry-run` to rehearse: in store mode it still fetches and verifies, so
a dry run answers "is that version published and intact" without installing.

### 3. Verify each host — all four, not just the first

The unit being active proves a kaed is running. It does not prove it is
*this* kaed. Check all of these:

```sh
kaed --version                     # 1. must match $V, and NOT be -dirty
kaed check-config                  # 2. must exit 0; read the roots and deny rules
systemctl --user is-active kaed    # 3. active
```

For (1), assert rather than eyeball. The published version *is* the stamp,
rearranged (`0.1.0 (b08ce9f 2026-08-02)` → `0.1.0-b08ce9f`), so this is an
exact comparison:

```sh
GOT=$(kaed --version | awk '{ gsub(/[()]/, "", $3); print $2 "-" $3 }')
[ "$GOT" = "$V" ] && echo "match" || echo "MISMATCH: binary is $GOT, expected $V"
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

A table per host: version installed, `kaed --version`, unit state, MCP
`serverInfo.version`. Name anything that did not match.

## Rollback

A failed deploy does not roll back a merge — the code is fine, the rollout
isn't. Two paths, and the second is why the store is worth having:

```sh
# fast: the binary this deploy replaced, already on the host
mv -f ~/.local/bin/kaed.prev ~/.local/bin/kaed && systemctl --user restart kaed

# general: any published version, on any host, checkout or not
./deploy/install.sh --from-store --store "$STORE" --version <older>
```

`kaed --version` on `kaed.prev` tells you which build you are rolling back
*to* before you commit to it. `curl -fsS "$STORE/artifacts/kaed/"` (or
`ssh kubsdb kpkg list`) shows what versions exist — **the store's history is
the rollback path**, and it survives a host losing `.prev`.

Roll back the *broken* hosts only. A half-deployed fleet is a normal state
here — the hosts are independent, and there is no schema or protocol
migration coupling them.

## What this skill does not do

- **Config.** Roots, `allowed_hosts` and deny rules are per host and are not
  in this repo. `install.sh` writes a starter config only when none exists.
  Once k-homelab #926 lands, its recipe asserts those; until then they are
  hand-maintained and this skill must not touch them.
- **Tokens.** Never generated, never rotated, never read except to
  authenticate a verification call. Rotation is `kaed-new-token --rotate`
  (installed on every host since sprint 005) and is a deliberate, separate
  act.
- **`tailscale serve`.** Set once per host at first deploy; k-homelab's
  `tailscale-serve` recipe owns it.
- **Client wiring.** Adding a host to a client's MCP config is separate, and
  it is genuinely dangerous — read the warning in `docs/setup.md` §7 before
  touching a client config from a script. It cost this project every MCP
  server on cleo once (korg #931).
