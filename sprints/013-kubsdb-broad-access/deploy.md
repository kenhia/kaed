# Sprint 013 deploy — kubsdb joins the fleet

> Planned before ship; executed 2026-08-08 (record at the end). Store-native
> throughout (005): kubsdb gets no checkout and no cargo — which is
> pleasingly circular, since the store it installs from is the very
> `/datastore/packages` that D-2 denies it from editing.

## Target state

### kubsdb `~/.config/kaed/config.toml` (new)

`install.sh` places the example config only if none exists and never adds
`[peers]` to one that does — on a fresh host we write the real config
directly. Placeholder `<tailnet>`: real value via `tailscale status --json`
(grep `'"MagicDNSSuffix": *"'` — with the space).

```toml
[server]
bind = "127.0.0.1:4870"
allowed_hosts = [
    "kubsdb.<tailnet>.ts.net",
    "kubsdb.<tailnet>.ts.net:4870",
]

[[roots]]
name = "datastore"
path = "/datastore"
description = "service compose + config; data/ dirs, _retired/ and the package store are denied. kwebi/app is a DEPLOY TARGET (source of truth: kwebi repo; deploy.sh rsyncs over it)."

[[roots]]
name = "hvsim"
path = "~/hvsim"
description = "DEPLOY TARGET: source of truth is fun/hv-simulator on kai; `just deploy` overwrites compose.yaml. Edit here for live triage; edit the repo for changes that should survive."

[[roots]]
name = "src"
path = "~/src"
description = "code repos (apt-temps, kpidash)"

[peers.kai]
status = "active"
url    = "https://kai.<tailnet>.ts.net:4870/mcp"
note   = "workstation; fleet gateway since 010"

[peers.kubs0]
status = "active"
url    = "https://kubs0.<tailnet>.ts.net:4870/mcp"
note   = "klams/k-homelab host; no checkout, installs from the package store"

[auth]
claude = { token_file = "~/.config/kaed/token" }

[security]
deny = [
    "**/secrets",
    "**/*.key",
    "/datastore/*/data",
    "/datastore/_retired",
    "/datastore/packages",
]
```

No `[peers.*.tokens]` tables: kubsdb serves and is proxied *to*; it is not
a gateway. No `[secrets]` overrides: `leak_checks = "refuse"` and reveal
defaults are correct here.

### kai `~/.config/kaed/config.toml` (edit)

`[peers.kubsdb]` flips `deferred → active` — dropping `ref = "korg:929"`,
which this sprint retires — and gains the routing url plus a per-author
token table (hand-configured; k-homelab's kaed-service recipe preserves
this table, never writes it):

```toml
[peers.kubsdb]
status = "active"
url    = "https://kubsdb.<tailnet>.ts.net:4870/mcp"
note   = "data host; broad /datastore root with data/ + package store denied (kaed sprint 013)"

[peers.kubsdb.tokens]
claude = { token_file = "~/.config/kaed/peer-tokens/kubsdb-claude" }
```

## Steps (deploy-fleet, from merged main)

1. `just publish` from kai — 0.13.x bundle into the package store.
2. On kubsdb: `install.sh --from-store` (binary, service unit).
3. Token: `deploy/new-token.sh` on kubsdb; copy into kai's
   `~/.config/kaed/peer-tokens/kubsdb-claude` (mode 600) — remember 002's
   rotation rules; never through a journaled/pasted surface.
4. Write kubsdb's config.toml above (from kai, base64-over-ssh per the
   global convention; verify with md5sum). `kaed check-config` on kubsdb —
   read the printed deny list, that is the real one.
5. `tailscale serve` on kubsdb per 001's reference (the rmcp `Host` gotcha:
   allowed_hosts must list bare and ported forms).
6. Edit kai's `[peers.kubsdb]` + tokens table; SIGHUP kai's kaed (010 D-2:
   token files reload, no restart).
7. Upgrade kai + kubs0 to the same published version while at it —
   re-running install.sh is the upgrade path.

## Verification battery (extends 004's)

From cleo (or any client of the gateway) unless noted:

- [ ] `roots` on kai: kubsdb appears **active, verified**, three roots
      with descriptions verbatim, deferred entry gone.
- [ ] `read` of `kubsdb:datastore` `korg/docker-compose.yml` via the
      gateway — content + version (proxy path works).
- [ ] `read` of `korg/korg.env` → redacted placeholders, never plaintext;
      `search` for a known value literal in `kubsdb:*` → zero matches,
      `classified_hidden`/fanout reporting intact.
- [ ] `stat`/`read` of `postgresql/data` → `denied` naming
      `/datastore/*/data`; same for `packages/` naming
      `/datastore/packages`.
- [ ] `edit` dry_run on `hvsim` root → response fine; the *description*
      seen in `roots` carries the deploy-target advisory.
- [ ] `journal` on kubsdb (direct URL) shows the verification txns under
      `claude` — journaled on the target, not the gateway (D-7 of 010).
- [ ] Root-owned 644 file write attempt (e.g. `prometheus/prometheus.yml`)
      → io error, not a crash; recorded as the D-6 accepted shape.
- [ ] `kubsdb:src` reachable; `*:src` fleet search fans out across all
      three hosts.

Post-deploy bookkeeping: korg #929 closes with a comment pointing here;
kai's journal keeps the only record of the config edit if made through
kaed (gateway journals nothing it proxies).

---

## Deployed 2026-08-08

**Artifact `0.1.0-0ed27cc`** (squash-merge of PR #14), published to the
store and installed on all three hosts. Rollback target: `0.1.0-af0731b`
via `kaed.prev` on each host, or any published version from the store.

| host | `kaed --version` | unit | MCP `serverInfo.version` |
|---|---|---|---|
| kai | `0.1.0-0ed27cc` ✓ | active | `0.1.0 (0ed27cc)` ✓ (gateway calls) |
| kubs0 | `0.1.0-0ed27cc` ✓ | active | `0.1.0 (0ed27cc)` ✓ |
| kubsdb | `0.1.0-0ed27cc` ✓ | active | `0.1.0 (0ed27cc)` ✓ |

kubsdb bring-up went exactly per the plan above: config written
base64-over-ssh and hash-verified, token minted with `kaed-new-token`
(copied to kai's `peer-tokens/kubsdb-claude`, mode 600, never printed),
`tailscale serve --bg --https=4870` added alongside the host's existing
serve entries, `kaed check-config` printed the three roots and the exact
deny list from D-1, and kai's `[peers.kubsdb]` flipped
`deferred → active` + tokens table, then a unit restart.

**Verified live, through the gateway as `claude`** (the sprint's own
behaviour, not just liveness):

- `roots` on kai probes kubsdb live: three `kubsdb:*` roots, `active`,
  descriptions (including both DEPLOY TARGET advisories) verbatim; the
  `deferred`/`korg:929` entry is gone.
- `read kubsdb:datastore postgresql/data/PG_VERSION` → structured
  `denied` (with 009's feedback invite riding it).
- `read kubsdb:datastore korg/korg.env` → placeholders only
  (`⟨kaed:DATABASE_URL@…⟩`); no value crossed the wire.

Remaining battery items (fleet search fan-out, hvsim dry_run, direct
journal read, root-owned-file io error, value-probe search) are covered
by Ken's live test from cleo, results to land beside this file like 010's
`live-test.md`.

**Known doc staleness for the follow-up chore PR**: the deploy-fleet
skill's fleet table and its "kubsdb is deliberately NOT in the fleet"
warning, CLAUDE.md's fleet status ("kubsdb deliberately has no
instance"), and `docs/setup.md`'s fleet examples all predate this deploy.
