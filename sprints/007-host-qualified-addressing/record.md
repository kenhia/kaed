# Sprint 007 — host-qualified addressing and the declared fleet

**Proposal:** korg:1055 · **Branch:** `007-host-qualified-addressing` · started 2026-08-07

Covers korg **#1045** (host-qualify root names, declared fleet in `roots`),
**#930** (a deferred host looks identical to a failed deploy), and **#1066**
(`search` returns a silently empty result for a glob that can match nothing
under `path`).

Slice 2 of program 1063 — step 1 of the gateway design in
`../planning/brainstorm-gateway-mcp.md`, deliberately split from proxying.
Nothing here talks to another host. What ships is the **addressing
vocabulary** and the **declaration**, so that peer mode (#1050) is later a
routing feature rather than a redesign, and so that the secret handle's
location half (PD-3) has a host-qualified namespace to be written against.

## Goal

Three things an agent can read, all through the tool it already calls first:

1. Root names become `host:name` — `kai:src`, not `src`. No tool signature
   changes: `root` was always the indirection.
2. `roots` grows a per-root `host`/`status`/`capabilities` and a `fleet`
   block that names **every declared host, including the ones deliberately
   not running kaed**. That is #930's exit criterion, met at the tool
   surface rather than in a doc.
3. `search` (and `list`) report `files_searched`, so a zero that means "your
   glob selected nothing" stops being byte-identical to a zero that means
   "your pattern is genuinely absent".

## What shipped

**Addressing (#1045).** `[server] host` in `config.toml`, defaulting to the
short system hostname, prefixes every declared root: `name = "src"` is
served, addressed and journaled as `kai:src`. The default is what makes this
a zero-touch upgrade on hosts whose config `install.sh` will never overwrite.
kaed refuses to start if it cannot resolve a host name, and logs the one it
picked and whether it was configured.

**The declared fleet (#930).** `[peers]` in `config.toml`, validated at
startup — `deferred` requires `ref`, `unreachable` requires `since` — and
surfaced two ways:

- `roots` grew `host`, per-root `status` and `capabilities`, and a `fleet`
  block with one entry per declared host. `verified` is true only for the
  instance answering, because this sprint probes nothing; `fleet.declared`
  is false when there is no `[peers]` table at all, so an absence is never
  read as a deferral.
- The **error path**, which is where the session that got #930 filed
  actually was. `root: "kubsdb:data"` no longer returns "unknown root"; it
  returns *"host `kubsdb` deliberately does not run kaed (korg:929): … this
  is a recorded decision, not a broken deploy, so do not install one
  there"*, with `reason`, `ref` and `note` in `data`. Seven distinct
  reasons, one per remedy — see D-2.

`kaed check-config` prints the host and the fleet too, for whoever is on the
box with a shell rather than an MCP client. `install.sh` warns when an
existing config has no `[peers]`, since it will not add one and a silently
undeclared fleet is the original bug.

**The silent zero (#1066).** `search` gained `files_searched` and `list`
gained `entries_scanned` — both always present, including zero — plus a
shared structured `reason` whose `hint` is built from the caller's own
`glob`/`path`. Verified against the sprint-006 incident verbatim:

```
search {root: "kai:src", path: "ai/kaed", glob: "README.md", pattern: "journal"}
  → matches: [], files_searched: 0
    reason: glob_matched_no_files
    hint: "0 of 2 scanned entries matched: `glob` is matched against
           ROOT-relative paths and is not re-anchored by `path` … try
           "ai/kaed/README.md" or "**/README.md"."
```

The suggested glob then returns the match. Before this, step one was a bare
empty list and the agent concluded the string appears nowhere.

**Contract.** `mcp-contract.md` gained R8 (host-qualified, durable root
names, and the historical-row rule from D-6), the new `roots` shape, the
`search`/`list` fields, `unsupported_capability` as reserved, and a worked
example for the zero that used to lie.

**Not done, deliberately:** no proxying, no peer probing, and #1066's fix
option (3) — `glob` relative to `path` — is left alone as a separate
decision. See decisions.md D-7.

## k-homelab's `kaed-service` recipe is unaffected — checked, not assumed

The recipe asserts config on both hosts and **removes a root not declared in
`manifests/<host>.yml`**, so "does host-qualification break it" had to be
answered before shipping rather than discovered by an `apply`. Read on kubs0
(`recipes/kaed-service/kaedconf.py`):

- It keys roots on `r["name"]` from the `[[roots]]` blocks — the **local**
  name, which D-1 deliberately left alone. `src` in the manifest still
  matches `src` in the config; the host prefix never appears in either.
- It manages exactly `bind`, `allowed_hosts`, `roots`, `[security] deny` and
  `journal.retention_days`, and splices only those spans. `[server] host`
  and `[peers]` are tables it does not know about, so they survive a
  converge byte-identically.

So no manifest change is needed for the rename. Declaring the fleet in
`[peers]` is a *kaed* config concern that the recipe will not fight — which
is the outcome PD-5 wanted, since the alternative was k-homelab owning the
declaration and kubs0 having no checkout to read it from.

## Follow-ups

- **Deploy owes a per-host `[peers]` edit.** `install.sh` will not add the
  block to an existing config, so kai and kubs0 come up post-upgrade with
  correct qualified root names but `fleet.declared: false`. Adding
  `[peers.kubs0]`/`[peers.kubsdb]` on kai and the mirror on kubs0 is a
  deliberate step at ship time; the installer warns when it is missing.
- **klams' kaed topology memory goes stale at deploy, not at merge.** It
  names roots `src` / `scratch` / `k-homelab` and says the fleet is declared
  in four places with k-homelab's manifests the strongest candidate for the
  declared side — reasoning PD-5 has since replaced. Supersede it *after*
  the fleet is actually running 007, not before: until then it is correct.
- **`deploy-fleet`'s own fleet table** (`.claude/skills/deploy-fleet/
  SKILL.md`) says it should read from #930's answer once that lands. It now
  has one.
- **#1049 must implement D-6.** History tools need to label a transaction
  whose root no longer resolves as historical, and `revert` must refuse one
  with a structured reason. Every row written before this sprint qualifies,
  plus kai's five pre-existing `home` rows.
- **`unsupported_capability` is reserved, not implemented.** Nothing lacks a
  capability on a single instance; peer mode (#1050) is what makes it real.
