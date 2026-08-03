# Sprint 004 decisions

The decision-shaped work in the fleet-deploy sprint (korg #925), plus the
two questions the proposal flagged as "worth deciding rather than
discovering". All of them are cheap now and expensive once three hosts are
live and clients are wired.

Every decision below was made against the hosts as they actually are on
2026-08-02, not against how they were remembered. The recon is recorded
inline, because the reasoning is worthless without it.

---

## D1 — per-host roots (korg #925)

### The question

kai's roots (`src` = `/home/ken/src`, `scratch` = `/home/ken/scratch`) suit
a workstation. kubs0 and kubsdb are not workstations, and copying kai's
config to them is the `$HOME` mistake in a new costume — plausible-looking
roots that reach further than intended.

### What is actually on each host

**kubs0** — the memory/intelligence host. `~/src/ai` holds five real git
repos (`klams`, `klams-mind`, `klams-view`, `krag`, `kris`); `~/src` also
carries `ai-agents`, `ai-tools`, `config-src`, `misc`, `opc`, `tools`.
Separately, `~/k-homelab` is a full checkout of the config-management repo,
including a `secrets/` directory (`index.yml`, `recipients.txt`, `store`)
that is committed — i.e. encrypted at rest, but still not something an
editor should be handing around. Four `.env` files live inside `~/src`
repos, all covered by the default deny.

**kubsdb** — the data host. `~/src` contains exactly one git repo
(`tools/kpidash`) and one stray directory (`homelab/apt-temps`). Everything
that makes kubsdb matter is *not* source: `/datastore/{postgresql,mongodb,
redis,grafana,prometheus,korg,kwebi}` are live data directories, and
`/gratch` is the 12 TB NAS mount whose top level includes `bitlocker-keys`
and `github-recovery`.

### Decision

**kubs0 gets two roots. kubsdb gets no kaed instance this sprint.**

```toml
# kubs0
[[roots]]
name = "src"
path = "/home/ken/src"
description = "klams and friends"

[[roots]]
name = "k-homelab"
path = "/home/ken/k-homelab"
description = "homelab config management (secrets/ denied)"
```

**`k-homelab` is in, deliberately, and this is the contentious half.** The
WI put the argument for leaving it out: it is a config-management repo
whose changes apply to every host. Three things answer it.

1. **kaed cannot apply anything.** There is no exec tool and no git tool in
   the MCP surface, by design. Editing a recipe is not running one; the
   apply is a separate deliberate act by something with a shell.
2. **Every edit is journaled and attributed.** A recipe change made through
   kaed is *more* traceable than the same change made over ssh, which is
   the realistic alternative.
3. **This sprint's own other half is a k-homelab manifest change** (#926).
   An agent that can read klams source on kubs0 but must ask a human to
   hand-type a manifest line reintroduces exactly the hand-editing this
   sprint exists to eliminate.

The real risk in that root is `secrets/`, and that is what the deny list is
for — `**/secrets/**` is already in kai's config and goes in the template,
so it is in force on kubs0 from the first start rather than added after
someone notices.

**No `scratch` root on kubs0**: there is no scratch directory there, and
inventing one to match kai would be symmetry for its own sake.

### kubsdb: not this sprint, and the trigger for revisiting

The proposal listed "kubsdb may not warrant an instance at all yet" as a
live option. Taking it, on the evidence:

- There is **one** git repo on the host worth editing, and it can be edited
  from kai like any other repo.
- The things genuinely maintained *on* kubsdb — `~/hvsim/compose.yaml`,
  `/datastore/kwebi/app` — are **rsync/scp deploy targets**, not sources.
  `hv-simulator`'s `just deploy` copies `deploy/compose.yaml` over the live
  file, and kwebi's `deploy.sh` rsyncs its whole tree. Editing those in
  place through kaed would put the host copy *out* of sync with the repo,
  which is worse than not being able to edit them.
- Everything else on the host is either live database state or the NAS
  mount, and neither should ever be an editor's business.

So a kaed instance on kubsdb would be a third network-exposed write surface
on the host that holds the most sensitive data in the homelab, in exchange
for editing one repo that is reachable elsewhere. That is a bad trade
today.

**This is a "not yet", not a "no" — korg #929 tracks the revisit, and it
should be soon.** Ken's framing on 2026-08-03, which changes what the
revisit is actually about: no kaed on kubsdb means remote agents (and
agents are *almost always* remote to kubsdb) have to use other means to do
the same work, and that cost is real and recurring. The plan is runtime on
kai and kubs0 first, then **likely broad access** on kubsdb — a decision
needing more thought than this sprint had room for.

Which reframes the question. The narrow-roots version of a kubsdb instance
is what the recon above argues against, and it buys almost nothing anyway.
The version worth designing is the broad one, and it has to answer harder
things: what "broad" means when `/datastore/postgresql` and
`/datastore/korg/korg.env` sit side by side under a lexical deny matcher;
whether editing a host copy whose source of truth is a repo elsewhere is
kaed's problem or a "don't root there"; and whether any of it belongs in
the deny defaults. By then kai's and kubs0's journals will show what agents
actually edit, which beats another round of speculation.

`/datastore` data directories and `/gratch` stay out regardless.

**Consequence for this sprint:** #927 becomes "deploy to kubs0 and wire
cleo to two instances". The fleet is kai + kubs0.

---

## D2 — one identity or one per host (korg #925)

### Decision

**`claude` on every host.** Tokens stay per-instance regardless — each
daemon reads its own token file, and a token is never shared across hosts.
The decision is only about the author name recorded in each journal entry.

The journal is per-host: `~/.local/share/kaed/journal.db` on kai answers
only for kai. The host is therefore already the discriminator, and encoding
it a second time in the author name would be redundant in every row of
every journal. Keeping one name also means a query written against kai's
journal runs unchanged against kubs0's, which is what makes the two
comparable at all.

Per-host names (`claude-kai`, `claude-kubs0`) buy something only if the
journals are ever merged. If that day comes, the host column comes from
whichever process does the merging, which knows the host — it does not need
to have been baked into every historical row.

---

## D3 — MCP server naming on cleo

### The question

Desktop Claude on cleo will hold entries for multiple kaed instances. Two
tools both called `kaed` and no way to tell which host is which — and every
wrong-machine edit still returns a successful-looking diff.

### Decision

**`kaed-<host>`: `kaed-kai`, `kaed-kubs0`.** No bare `kaed` entry anywhere,
including on hosts that only ever talk to one instance.

The bare name is the trap: it reads as "the kaed" and is the one an agent
reaches for by default, so the moment a second instance exists the default
is wrong roughly half the time. Naming every entry after its host means the
host is in the tool name at every call site, and there is no default to
guess wrong about. The cost is renaming kai's existing entry once, now,
while there is exactly one client to update.

---

## D4 — which URL a co-located agent should use

### The question

Sprint 001 verified that an agent on kai can reach kai's own kaed over the
ts.net URL (hairpin). klams' general homelab note says a host cannot reach
its own `tailscale serve` endpoint. Both cannot be right, and someone will
trip over the disagreement.

### Decision

**The ts.net URL is the one URL, on every host including the serving one.
`http://127.0.0.1:4870/mcp` is a documented fallback, not the default.**

Re-verified on kai on 2026-08-02, from kai itself:

```
POST https://kai.<tailnet>.ts.net:4870/mcp  → 406   (authenticated)
POST http://127.0.0.1:4870/mcp              → 406   (authenticated)
```

A `406` there is success — auth passed and the MCP layer then rejected an
empty body. Both routes work.

**The disagreement is resolved, not split.** klams' general note predates
the tailnet single-URL migration of 2026-07-17, which established the
loopback-bind + `tailscale serve` pattern precisely so that hairpin works;
that migration's own record states one URL working "from every tailnet host
including the serving host". kaed already follows that pattern. The general
note describes hosts that do not.

One URL per instance means the client config on cleo, on kai and on kubs0
are the same three lines apart from the token, and an agent copying a URL
between machines cannot produce a broken one.

---

## What this sprint is knowingly not deciding

**`**/secrets/**` and `**/*.key` stay config-level, not `DEFAULT_DENY`.**
Both are in kai's config today and both go in `deploy/config.example.toml`,
so every host gets them from the template rather than from three separate
hand-edits. Promoting them into the built-in defaults is a change to
published behaviour — README, SECURITY.md and the contract all describe
that list — and it belongs in a sprint that is looking at the contract, not
in one whose job is to stop hand-typing installs. Worth a follow-up: `*.key`
in particular is the same class of thing as `*.pem`, which is already a
default.
