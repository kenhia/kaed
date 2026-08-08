# Sprint 013 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. This sprint closes korg #929 — the last slice
> of program 1063 (PD-1) — by answering the four questions it left open and
> deploying the third instance. The inputs it waited for all exist now:
> runtime evidence (006), deny-vs-classify (008), cheap wiring (010).
>
> Like 004, the recon is recorded inline, because the reasoning is
> worthless without it.

---

## The recon (2026-08-08, live — not 004's remembered version)

004's recon is six days old and the host has moved. What is actually there:

```
/datastore/
  grafana/        docker-compose.yml, provisioning/, dashboards/, data/
  korg/           docker-compose.yml, korg.env (ken, 600)
  kwebi/          docker-compose.yml, .env (ken, 600), app/ (rsync target), data/
  mongodb/        docker-compose.yml (root, 600), data/ (uid 999)
  postgresql/     docker-compose.yml (root, 600), data/ (uid 999, mode 700)
  prometheus/     docker-compose.yml, prometheus.yml (root, 644), data/
  redis/          docker-compose.yml (root, 600), data/ (uid 999)
  unpoller/       docker-compose.yml, up.conf (root, 600)
  packages/       the homelab package store: artifacts/, crates/, registry/, simple/
  package-store/  docker-compose.yml, nginx.conf (root, 644)
  registry/       docker-compose.yml (root, 644)
  _retired/       vikunja-final-20260727.sql
  lost+found/
```

Three facts drive everything below:

1. **The layout is uniform**: every service directory is compose + config at
   the top with a `data/` subdirectory holding the live state. "Config yes,
   data no" is not a judgment call per directory — it is one lexical shape.
2. **New since 004**: the homelab package store lives here now
   (`packages/`, served by `package-store/`, plus a Docker `registry/`).
   004's recon never had to think about it; this sprint does (D-2).
3. **Ownership is mixed**: four compose/config files are root-owned mode
   600 (postgresql, mongodb, redis compose; `up.conf`) — invisible to a
   kaed running as ken — and several more are root-owned 644: readable,
   not writable (D-6).

Elsewhere: `~/hvsim` is unchanged (a `compose.yaml` rsync-deployed from
`fun/hv-simulator` on kai); `~/src` still holds exactly
`homelab/apt-temps` and `tools/kpidash`; `~/.kconfidential` (mode 700)
sits outside every root proposed here; and `/gratch`'s top level is, if
anything, a stronger argument than 004 recorded — `bitlocker-keys`,
`backups`, `github-recovery`, `dotfiles-work` and some forty siblings.

What the evidence pass adds (006, `docs/agent-usage-report-2026-08-06.md`):
agents use kaed for docs and config — 81% markdown, the rest compose files,
`.env`-shaped config and scripts, zero data files, zero appetite for
anything `/gratch`-shaped. Small corpus (41 txns), so it justifies the
*shape* of the roots, not their width — which is exactly how it is used
here.

---

## D-1 — Broad means: root at `/datastore`, deny the data by shape

```toml
[[roots]]
name = "datastore"
path = "/datastore"

[security]
deny = [
    "**/secrets",
    "**/*.key",
    "/datastore/*/data",
    "/datastore/_retired",
    "/datastore/packages",
]
```

**`/datastore/*/data` is the whole answer to "what does broad mean".** The
recon's uniform layout makes the config/data boundary lexical, and the
matcher's own semantics make the shape durable: deny globs match absolute
paths with ancestors walked (the 004 lesson — bare form, never `/**`), and
globset's `*` crosses separators, so a `data` directory at *any* depth
under `/datastore` is covered. Both properties are now pinned by
`deny.rs::the_kubsdb_broad_access_shape_holds` — the "test the matcher"
instruction #929 carried, discharged in CI rather than on the host.

The alternative — denying services by name (`/datastore/postgresql`, …) —
was rejected because it rots at the worst moment: a new service following
the layout convention would arrive with its data dir *unprotected* until
someone edits config. Under the shape rule it is covered on arrival.
The over-match (`kwebi/app/**/data`, should one appear) is accepted: it is
the safe direction, and the refusal names the rule.

`_retired/` is denied as what it is — a database export is data wearing a
different name.

**The classify half is why this sprint runs at all**: `korg.env` and
`kwebi/.env` are *not* denied. They classify under 008's defaults
(`**/*.env`, `**/.env`), read redacted, take typed env ops, and a search
value-probe matches nothing by construction. `/datastore/postgresql` next
to `/datastore/korg/korg.env` — #929's central difficulty — resolves as:
the first is denied by shape, the second is editable but redacted. That is
the deny-vs-classify distinction doing the exact job PD-1 sequenced it for.

## D-2 — The package store is denied: it is fleet supply chain, not host config

`/datastore/packages` did not exist in 004's recon and changes the risk
shape of the root that now contains it. It holds the artifacts every host
installs — kaed itself deploys from it (005, store-native) with no
signature check between store and host. A write surface there converts one
agent edit into arbitrary code on every host at its next install: strictly
worse blast radius than the database data dirs beside it, whose damage at
least stays on kubsdb.

Read access is not worth carving out: the store is already HTTP-served
(that is its job), `install.sh --from-store` and version queries go over
that surface, and kaed has no read-only root mechanism to express
"list but never write" anyway.

The *serving* configuration — `package-store/`'s compose and `nginx.conf`,
`registry/`'s compose — stays in the root: that is host config like
grafana's, and today root-ownership makes it read-only through kaed
regardless (D-6).

## D-3 — `/gratch` has no root, and this sprint does not soften that

Unchanged from 004, now with evidence on top of the recon: nothing in five
days of journals suggests an agent reaching for anything shaped like NAS
contents, and the top level still leads with `bitlocker-keys` and
`github-recovery`. Any future argument for a subtree is a new argument to
be made explicitly (a new root with its own description), never an
inherited default. Not carried as an open question — closed until someone
reopens it with a concrete need.

## D-4 — Deploy targets are told, not denied: the advisory is the root description

#929's genuinely interesting question — an agent edits `~/hvsim/compose.yaml`
or `/datastore/kwebi/app/*`, and the next `just deploy`/`deploy.sh`
silently reverts it — gets the third answer that did not exist in 004:
**the `roots` response carries a per-root `description` (007), and the
advisory rides it.**

```toml
[[roots]]
name = "hvsim"
path = "~/hvsim"
description = "DEPLOY TARGET: source of truth is fun/hv-simulator on kai; `just deploy` overwrites compose.yaml. Edit here for live triage; edit the repo for changes that should survive."

[[roots]]
name = "datastore"
path = "/datastore"
description = "service compose + config; data/ dirs, _retired/ and the package store are denied. kwebi/app is a DEPLOY TARGET (source of truth: kwebi repo; deploy.sh rsyncs over it)."
```

Why not deny the targets outright: editing the live copy is sometimes the
correct move — a compose hotfix during an outage is exactly remote-agent
work, and it is journaled, attributed and revertable through kaed where an
ssh edit is none of those. The failure mode #929 worried about is an agent
editing *in ignorance*; the description removes the ignorance in the one
surface every client already reads (PD-5's criterion — the answer lives
where the agent is already looking). kaed's posture is making state
legible, not enforcing policy it cannot see (it has no git tool to know
whether a tree is clean — by design, `overview.md` "What kaed is not").

`kwebi/app`'s advisory rides the `datastore` description because
descriptions attach to roots, not subdirectories — accepted as good
enough; a per-subtree advisory mechanism is not worth inventing for one
case.

## D-5 — Three roots: `datastore`, `hvsim`, `src`. No `$HOME`, ever

`kubsdb:src` is included even though 004 rightly said it buys almost
nothing *as a reason to deploy*: the instance now exists for other
reasons, the marginal cost of the root is zero, and it makes
`apt-temps`/`kpidash` editable the same way every other host's repos are —
plus `search` root patterns (`*:src`) then cover all three hosts
uniformly. Not rooting `$HOME` is 001's founding lesson and needs no
re-argument; `~/.kconfidential` and `~/.ssh` stay outside every root by
construction, not by deny rule.

## D-6 — Unix ownership is a fourth layer, accepted and documented, not built around

kaed runs as ken. Root-owned 600 files (postgresql/mongodb/redis compose,
`up.conf`) are invisible — reads fail at the OS, not at policy. Root-owned
644 files (grafana/prometheus compose, `prometheus.yml`, `nginx.conf`) read
fine and fail at *write* time with an io error rather than a structured
refusal. That is less legible than a `denied` with a hint, and it is
accepted: the alternative is kaed-as-root, which no amount of legibility
justifies. If journals later show agents actually hitting these (a
`leak_flagged`-style evidence bar, per 012 D-3's pattern), a structured
"not writable by the service identity" error is a cheap follow-up —
noted, not built.

## D-7 — Nothing changes in the shipped defaults; the sprint is config + one pin test

The fourth #929 question answered: `DEFAULT_DENY` and `DEFAULT_CLASSIFY`
are untouched. `/datastore/*/data` is a fact about one host's layout, not
about the world; hoisting it would be symmetry for its own sake (004's own
anti-pattern). What ships in the repo is the matcher pin test (D-1), the
example-config note that absolute deny globs are legal, and this record.
Everything else is kubsdb's installed `config.toml` and kai's `[peers]`
flip — per-host state, executed at deploy time (`deploy.md`).

The peers flip itself was designed by 007/PD-5 and needs no new mechanism:
kai's `[peers.kubsdb]` goes `deferred → active` + url, gaining a
`[peers.kubsdb.tokens]` table (hand-configured — k-homelab's kaed-service
recipe preserves it, never writes it), and kubsdb's own config declares
kai and kubs0 as active peers without token tables — it serves and is
proxied to; it is not a gateway.
