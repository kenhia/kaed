# Sprint 005 deploy — proving the store path before the fleet takes it

The fleet is **still on `0.1.0 (b08ce9f 2026-08-02)`** as this is written.
That is deliberate: a branch commit vanishes from history at squash-merge,
and stamping a host with a SHA that is on no branch is the exact failure
sprint 004 existed to end. The real rollout is `/sprint-ship` Phase 7 →
`deploy-fleet`, from merged `main`.

What follows is the branch build proving every step of that rollout without
touching either live daemon.

## Published from the branch, without moving `latest`

```
$ just publish
publish: not on main — publishing 0.1.0-2c8af14 WITHOUT moving the latest pointer
==> publishing kaed 0.1.0-2c8af14 as kaed 0.1.0 (2c8af14 2026-08-06)
published artifacts/kaed/0.1.0-2c8af14/config.example.toml
published artifacts/kaed/0.1.0-2c8af14/install.sh
published artifacts/kaed/0.1.0-2c8af14/kaed.service
published artifacts/kaed/0.1.0-2c8af14/kaed-x86_64-linux
published artifacts/kaed/0.1.0-2c8af14/new-token.sh
```

`latest` still reads `0.1.0-b08ce9f` afterwards — checked, not assumed. So a
branch build can be exercised on the real store without any host that
resolves `latest` picking it up.

## kai — a real install from the store, into a throwaway HOME

Not a dry run: the binary, `kaed-new-token`, the unit file and a starter
config all actually landed. `HOME` was a temp dir, so the live daemon — and
the real config and token — were never in the blast radius.

```
fetching kaed 0.1.0-2c8af14 (x86_64-linux) from https://kubsdb.<tailnet>:4880
  kaed-x86_64-linux  checksum OK
  kaed.service  checksum OK
  config.example.toml  checksum OK
  new-token.sh  checksum OK
  reports kaed 0.1.0 (2c8af14 2026-08-06)
```

| Landed | Mode |
|---|---|
| `.local/bin/kaed` | `0755` |
| `.local/bin/kaed-new-token` | `0755` |
| `.config/systemd/user/kaed.service` | `0644` |
| `.config/kaed/config.toml` | `0600` |

`kaed --version` on the installed binary: `kaed 0.1.0 (2c8af14 2026-08-06)` —
the same string the store labelled it with, which is what the installer
asserted before installing anything.

## kubs0 — the clone-less bootstrap, exactly as documented

kubs0 has no checkout (deleted in k-homelab sprint 020) and no cargo needed.
The documented bootstrap ran verbatim: fetch `install.sh` out of the artifact
directory, **check it against the published `SHA256SUMS`**, then run it.

```sh
curl -fsS -O "$base/install.sh"
curl -fsS "$base/SHA256SUMS" | grep ' install.sh$' | sha256sum -c -
sh install.sh --from-store --store "$STORE" --version 0.1.0-2c8af14
```

Same four checksums, same four files, same stamp. Afterwards:

```
kaed 0.1.0 (b08ce9f 2026-08-02)     # the live daemon, untouched
active
```

That is the whole point of the sprint demonstrated in one run: a host with no
repo, no toolchain and no ssh-from-kai copy installed a verified build.

## Upgrade and failure paths, on the real store

- **Second install over the first** — rotated the outgoing binary to
  `kaed.prev`, left the existing config untouched (`config exists, left
  untouched`), and printed the rollback line.
- **A version that was never published** (`9.9.9-nope`) — exit 1, naming the
  URL it could not fetch.
- **`0.1.0-b08ce9f`, the pre-bundle artifact** — exit 1 on the first missing
  file:

  ```
    kaed-x86_64-linux  checksum OK
  curl: (22) The requested URL returned error: 404
  ERROR: fetch failed: .../0.1.0-b08ce9f/kaed.service — is 0.1.0-b08ce9f
  published, for x86_64-linux, with a full deploy bundle?
  ```

  **This is the correct answer, and it is also a real limitation:**
  `--from-store --version 0.1.0-b08ce9f` is not a rollback that works,
  because that version predates the bundle. Until a second bundle exists in
  the store, rolling back is `kaed.prev` or `--bin`. From 005's own publish
  onward, every version is a full bundle and `--version <older>` is the
  general path.
- In both failures the already-installed binary was **untouched** — the
  installer fetches and verifies everything before installing anything, so a
  failure partway through leaves the host on the build it was running.

## One thing that was already broken, found here

The sprint-004 `deploy-fleet` skill resolved the tailnet name with

```sh
grep -o '"MagicDNSSuffix":"[^"]*"'
```

`tailscale status --json` pretty-prints with a space after the colon, so on
tailscale 1.98 that matches nothing and `TN` comes back **empty** — and an
empty `TN` builds a URL like `https://kubsdb.:4880`, which fails as a DNS
error rather than as "your extraction is wrong". Fixed to `'"MagicDNSSuffix":
*"[^"]*"'` in the skill.

## Still to do

- **The real fleet rollout** — Phase 7, from merged `main`, per the rewritten
  `deploy-fleet` skill. Both hosts move from `b08ce9f` to the merge commit.
- **k-homelab's kaed advisory text is now wrong for kubs0.** The
  `kaed-service` recipe's `min_build_date` advisory says *"fix: git pull &&
  ./deploy/install.sh in the kaed repo"* — there is no repo on kubs0. Should
  name the store install instead. Different repo; noted for korg:932.
