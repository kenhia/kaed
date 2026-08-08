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

## Deployed

*(filled in at ship time)*
