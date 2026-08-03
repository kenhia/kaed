# Sprint 004 — fleet deploy

**Proposal:** korg:928 · **Branch:** `004-fleet-deploy` · started 2026-08-02

Take kaed from one hand-built host to a managed fleet. kai's install was
typed by hand; doing that twice more is how three hosts end up subtly
different from each other and from the docs. This sprint builds the thing
k-homelab can point at, gives the binary a name an audit can check, decides
where the daemon should and should not run, and rolls it out.

Covered work items: kaed #923 (installer), #924 (version stamp), #925
(per-host roots and identities), #927 (rollout + client wiring); k-homelab
#926 (recipe + manifests) — a different repo, picked up separately.

---

## Decisions

All four are written up in [decisions.md](decisions.md); the short forms:

- **D1 — roots.** kubs0 gets `src` = `~/src` and `k-homelab` = `~/k-homelab`.
  **kubsdb gets no instance this sprint** — a "not yet", tracked as korg
  #929 and wanted soon. The proposal listed the deferral as a live option
  and the recon took it: kubsdb has exactly one git repo worth editing, and
  everything else on the host is live database state, a rsync-deploy target,
  or the NAS mount with `bitlocker-keys` in it. Ken's steer is runtime on
  kai and kubs0 first, then likely *broad* access there — which is a
  different and harder design than the narrow roots this recon rejected.
- **D2 — identity.** `claude` on every host. The journal is already
  per-host, so the host is the discriminator; one name keeps journals
  comparable. Tokens stay per-instance regardless.
- **D3 — MCP naming on cleo.** `kaed-kai`, `kaed-kubs0`. No bare `kaed`
  entry anywhere — that is the name an agent reaches for by default, and
  once there are two instances the default is wrong half the time.
- **D4 — co-located URL.** The ts.net URL, on every host including the
  serving one; localhost is a documented fallback. Re-verified on kai.

The `k-homelab` root was the contentious call; **Ken confirmed it on
2026-08-03**, with `secrets/` denied. The case for it: kaed has no exec and
no git tool, so editing a recipe is not running one; every edit is journaled
and attributed, which is more than ssh gives you; and this sprint's own
other half is a k-homelab manifest change.

## What shipped

**#924 — version stamp.** `build.rs` embeds `git describe --always --dirty`
and the commit date; `src/version.rs` exposes them. `kaed --version` now
says `0.1.0 (966367c 2026-08-02)` instead of `0.1.0`, and the same string
goes out in the MCP handshake, so a connected agent and a shell on the host
agree about which build is running. `check-config` prints it as its first
line, so one command answers both "is this config sane" and "which build is
asserting that" — which is what k-homelab's recipe needs to assert a
freshness floor.

Composed in `build.rs` rather than at runtime because clap's `version` wants
a `&'static str`. Degrades to `0.1.0 (unknown)` with no `.git` rather than
failing the build — building from a tarball has to work.

**#923 — `deploy/`.** Four files, following klams' `deploy/` in spirit but
not in shape: klams installs a *system* service with a service account, kaed
is a `--user` unit because it edits your files as you.

| File | What it owns |
|---|---|
| `install.sh` | binary, unit file, and a starter config **only if absent** |
| `kaed.service` | the unit as a real file, `ExecReload` included |
| `config.example.toml` | the template `docs/setup.md` used to inline |
| `new-token.sh` | mint / `--rotate` / `--close` |

The two rules that shaped it: **it never overwrites an existing config**,
and **it never touches the token**. Those are the two things that cannot be
safely regenerated underneath a running host, and an installer that
regenerated either would undo sprint 002. Re-running `install.sh` is the
upgrade path — it restarts a running daemon, and installs the binary via
`mv` into place so an upgrade cannot hit `ETXTBSY`.

A fresh install is `enable`d but deliberately **not started**: the starter
config has placeholder roots, so starting it would only produce a crash
loop. The script prints the four remaining steps instead.

`new-token.sh` refuses to overwrite an existing token — rotation is
`--rotate`, which opens the grace window, and `--close`, which ends it. It
**never prints the token**; sprint 001's rotation happened because one
landed in a transcript.

Verified rather than assumed: dry-run on both the fresh-host and
existing-config paths, and mint → refuse-to-clobber → rotate → close
exercised against a throwaway config dir, checking mode `0600`, that a
refused mint leaves the token byte-identical, and that `token.prev` really
holds the old value.

**Docs.** `docs/setup.md` gets a five-command "short version" up top and
keeps every manual step as the explanation of what those commands do —
including the longhand rotation, so the script and the prose cannot quietly
diverge.

**#927 — the fleet is live.** kai (re-installed through the new script, so
the upgrade path got exercised somewhere verifiable) and kubs0, both on
`19a8cb4`. cleo's Claude Code is wired to `kaed-kai` and `kaed-kubs0`, and
both were verified end-to-end using cleo's own stored credentials rather
than a hand-pasted token. The whole battery — auth, hairpin, deny list
across all three enforcement points, the full edit → conflict loop, journal
rows and file modes — is in [deploy.md](deploy.md).

### The deploy broke cleo (korg #931)

The client-wiring step wrote `~/.claude.json` from PowerShell with
`Set-Content -Encoding UTF8` — UTF-8 **with BOM** on PS 5.1, plus CRLF.
Claude Code could not parse it, set it aside and regenerated a default,
taking **all four** MCP servers with it. klams and korg were working before
the deploy and dead after it, and neither had anything to do with this
sprint.

Repaired and verified, and the recurrence guard
(`deploy/check-client-config.ps1`) is demonstrated against the actual
corrupted file rather than asserted. Full account in
[deploy.md](deploy.md); the decision and what it says about scoping blast
radius is D5 in [decisions.md](decisions.md).

The uncomfortable detail worth carrying forward: I *did* verify the write,
and the verification passed — because it re-read the file through
PowerShell, which strips a BOM on the way in. **Verify in the format the
consumer reads, not the one you wrote with.**

### The finding that made the deploy worth doing carefully

`deny = ["**/secrets/**"]`, carried on kai since sprint 002 and copied into
the new template, is **weaker than it reads**. It hides a secrets
directory's *contents* but not the directory: `stat` on it succeeded and it
appeared in a parent `list` with no `denied_hidden`. The glob needs a path
component after `secrets`, so it never matches the directory node itself.

The right pattern is the bare `**/secrets` — exactly the idiom `deny.rs`
already documents for `**/.ssh`, where ancestor-walking covers everything
beneath. Fixed on both hosts and in the template, which now explains the
trap. Re-verified: `denied` on `stat`, absent from `list`, zero search hits.

Two things this vindicates: `check-config` printing every rule in force, and
exercising the deny list on each host instead of trusting a config that
looks right.

## Follow-ups

- **`**/secrets/**` and `**/*.key` are not in `DEFAULT_DENY`.** Both are in
  every host's config and now in the template, so nobody hand-types them —
  but `*.key` is the same class of thing as `*.pem`, which *is* a default.
  Promoting them touches README, SECURITY.md and the contract, so it belongs
  in a sprint looking at the contract.
- **k-homelab #926** needs `/start-sprint korg:928` run again from that
  repo's checkout on kubs0.
- **korg #929** — revisit kubsdb, with broad access as the shape to design,
  once kai and kubs0 have journal evidence to argue from.
