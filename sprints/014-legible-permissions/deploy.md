# Sprint 014 deploy — the fleet, plus kubsdb's config finished (#1093)

> Planned before ship. Store-native throughout (005): `just publish` from
> kai, `install.sh --from-store` on each host. 013's `deploy.md` remains the
> reference for kubsdb bring-up; this only records what *changes*.

## Fleet upgrade

All three hosts (kai, kubs0, kubsdb) take the new build. The code changes
are behaviour-visible on every host, not just kubsdb:

- `search`/`list` no longer die on an unreadable directory anywhere.
- read/write EACCES becomes a structured `denied` with a reason.
- `dry_run` refuses a write it can predict will fail — **this is the one
  change that can make a previously "successful" dry run start failing**,
  and that is the point of #1092. It cannot turn a working write into a
  failing one: the probe asks exactly what the write path needs.
- a single-peer root pattern (`kubsdb:*`) is expanded by the host asked.

Nothing here needs a config change to take effect. The kubsdb items below
do.

## kubsdb `~/.config/kaed/config.toml` (edit)

### 1. Drop the `/datastore/lost+found` interim deny entry

013's live test added it as a stopgap for #1088. The walker now skips and
counts unreadable directories, which is the general fix the interim entry
was standing in for — and leaving it would keep conflating "policy says
no" with "the OS said no" for exactly the path that taught us the
difference.

```diff
 [security]
 deny = [
     "**/secrets",
     "**/*.key",
     "/datastore/*/data",
     "/datastore/_retired",
     "/datastore/packages",
-    # korg #1088 interim (post-live-test): root-owned 700 dir kills the
-    # search walker (EACCES); remove once the walker skips-and-counts
-    "/datastore/lost+found",
 ]
```

The deny list returns to the exact five globs
`deny.rs::the_kubsdb_broad_access_shape_holds` pins — host and repo agree
again.

### 2. Classify the six secret-bearing service files (#1093 part 1)

None is `.env`-shaped, so 008's default classifier does not see them.
Today they are protected by unix mode alone, which is luck rather than
policy — and korg #1085 opened by proposing to change exactly those modes.

```toml
[security]
classify = [
    "/datastore/postgresql/docker-compose.yml",              # POSTGRES_PASSWORD
    "/datastore/mongodb/docker-compose.yml",                 # MONGO_INITDB_ROOT_PASSWORD
    "/datastore/redis/docker-compose.yml",                   # --requirepass
    "/datastore/unpoller/up.conf",                           # UniFi user= / pass=
    "/datastore/grafana/provisioning/datasources/datasources.yml",  # postgres datasource password
]
```

Precise paths, never `**/docker-compose.yml`: the k-homelab-managed
compose files are world-readable, hold no secrets, and reading them is
useful. Pinned in CI by `policy.rs::the_kubsdb_classify_shape_holds`, the
sibling of 013's deny-shape test.

`use_default_classify` stays on — `korg/korg.env` must keep reading
redacted, and 013's live test verified it does.

**The observed answer to the item's open question** (recorded here so the
next reader does not re-derive it): both shapes come back
**`classified_opaque`**, the desired outcome — refused with a reason, no
attempted redaction that could pass a value through. The mechanism is
structural rather than lucky: the strict dotenv grammar requires every
line to be blank, a `#` comment, or `KEY=value` at column 0, and both
files open with a line (`services:`, `[unifi.defaults]`) that is none of
those. Pinned by
`fsops.rs::kubsdbs_secret_bearing_service_files_classify_opaque_not_redacted`,
which also re-runs the 008 D-8 value probe over them.

Note `datasources.yml` is a **sixth** secret file relative to #1085's
original recon: 0640 root:root, readable by grafana as uid 472 gid 0.
That mode is deliberate (k-homelab sprint 016) and must not be disturbed.

### 3. Route the managed set via the root description (#1093 part 2)

The other half of `/datastore`'s root-owned files are rendered artifacts
of k-homelab's `docker-services` recipe: its `check` byte-compares them
against the repo copy and its `apply` re-installs them `-o root -g root`
*and restarts the service*, so an in-place edit would be silently reverted
even if the permissions allowed it. Same class D-4 already solved for
deploy targets, so the same mechanism.

```diff
 [[roots]]
 name = "datastore"
 path = "/datastore"
-description = "service compose + config; data/ dirs, _retired/ and the package store are denied. kwebi/app is a DEPLOY TARGET (source of truth: kwebi repo; deploy.sh rsyncs over it)."
+description = "service compose + config; data/ dirs, _retired/ and the package store are denied. kwebi/app is a DEPLOY TARGET (source of truth: kwebi repo; deploy.sh rsyncs over it). MANAGED: grafana/, prometheus/, package-store/ and registry/ are rendered by k-homelab's docker-services recipe and re-installed root:root on apply — edit the source at kubs0:k-homelab/recipes/docker-services/files/<svc>/, not here."
```

**No deny rules for these.** They are world-readable and hold no secrets;
only writing is futile, and since this sprint #1091 makes that legible at
the point of failure — carrying this very `description` back as
`root_advisory` in the refusal (014 D-3). That is what makes the advisory
and the hint the same words by construction rather than by discipline: an
edit here changes both.

## Steps

1. `just publish` from kai — bundle into the package store.
2. `install.sh --from-store` on kai, kubs0, kubsdb.
3. Edit kubsdb's config.toml per §1–3 above (base64-over-ssh per the global
   convention; verify with md5sum). `kaed check-config` on kubsdb — read
   the printed deny **and classify** lists, those are the real ones.
4. Restart kubsdb's unit (config changes are not SIGHUP-reloadable; only
   credentials are).

## Verification battery

The full client-side round is #1094 and lands in `live-test.md` beside
this file. The deploy itself only needs:

- [ ] `kaed --version` matches the published artifact on all three hosts.
- [ ] `kaed check-config` on kubsdb prints five deny globs (no
      `lost+found`) and five classify globs.
- [ ] `roots` on kai: `kubsdb:datastore`'s description carries the MANAGED
      advisory verbatim.

---

## Deployed 2026-08-08

**Artifact `0.1.0-88005bc`** (squash-merge of PR #16), published to the store
and installed on all three hosts. Rollback target: `0.1.0-0ed27cc` via
`kaed.prev` on each host, or any published version from the store.

| host | `kaed --version` | unit | MCP `serverInfo.version` |
|---|---|---|---|
| kai | `0.1.0-88005bc` ✓ | active | `0.1.0 (88005bc)` ✓ |
| kubs0 | `0.1.0-88005bc` ✓ | active | `0.1.0 (88005bc)` ✓ |
| kubsdb | `0.1.0-88005bc` ✓ | active | `0.1.0 (88005bc)` ✓ |

kubsdb's three config edits landed as planned (written base64-over-ssh,
md5-verified, `check-config` clean, unit restarted). Before writing the
classify globs the five paths were re-checked on the host: all five present
at the recorded modes, and a scan of every *other* compose/conf file under
`/datastore` for credential-shaped key names came back empty — so the
recon's set is exactly right, and the 600/640 modes correlate perfectly
with it.

### Verified live — the sprint's own behaviour, not just liveness

| item | probe | result |
|---|---|---|
| #1088 | unscoped `search kubsdb:datastore` — the call that used to die | **works**: 15 files searched, `denied_hidden: 8`, **`unreadable_hidden: 2`** |
| #1091 | `read postgresql/docker-compose.yml` (0600 root:root) | `denied` / `not_readable_by_service_identity`, `owner {uid 0, gid 0, mode 0600}`, `service_identity {uid 1000, user "ken"}`, `root_advisory` verbatim |
| #1091 | `read prometheus/prometheus.yml` (0644) | still reads — 1149 bytes, version `d3f9b2eb4301628c`. Only the write path was ever broken |
| #1092 | `edit dry_run` on `prometheus/prometheus.yml` | **refuses** — `not_writable_by_service_identity`, `directory: "prometheus"`, `owner {0755 root:root}`. No diff |
| #1092 | `edit dry_run` create under `kubsdb:src` | still returns a diff, `applied: false` — the probe refuses nothing that would have worked |
| #1089 | `search kubsdb:*` / `kai:*` / `*:*` through the kai gateway | all three: `hosts_unavailable` **absent**; `kubsdb:*` fans out over kubsdb's three roots only |
| #1093 | `check-config` on kubsdb | deny back to five globs (no `lost+found`); five classify globs present |

D-3 confirmed end to end: the `root_advisory` returned in the refusal is
byte-identical to the `description` written in `config.toml` — one string,
two surfaces.

### One finding worth recording

**The classified files refuse as `not_readable_by_service_identity`, not
`classified_opaque`** — because they are 0600 root:root, so the OS refuses
before kaed can read the bytes it would classify. That ordering is correct
(you cannot classify content you cannot read) and the refusal is the more
informative of the two, but it means the classify entries are **dormant**
on this host today.

That is not a wasted change: #1093 added them precisely because korg #1085
opened by proposing to make these files group-writable. The globs are the
policy that becomes load-bearing the moment a mode changes — the failure
mode they exist to prevent is exactly "someone relaxes the mode and kaed
starts serving a DB superuser password in plaintext." Belt and braces, with
the braces currently doing the work.

Worth knowing for #1094: a probe expecting `classified_opaque` on these
five paths will see `not_readable_by_service_identity` instead, and that is
a pass, not a failure. The repo test
`fsops::kubsdbs_secret_bearing_service_files_classify_opaque_not_redacted`
covers the classify behaviour itself on readable fixtures.
