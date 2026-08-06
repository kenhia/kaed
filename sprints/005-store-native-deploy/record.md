# Sprint 005 — the fleet deploy goes store-native

**Proposal:** korg:1023 · **Branch:** `005-store-native-deploy` · started 2026-08-06

Sprint 004 gave kaed a canonical installer and a version stamp, and its
deploy model was *every host builds from its own checkout of the same
commit*. That model is exactly the "pinned clone" the homelab package store
was stood up to end (k-homelab sprint 020, `docs/deploying.md`): kaed's
build-from-clone deploy on kai **and** kubs0 is named in that doc as one of
the three motivating failures.

Sprint 020 then proved the store path by hand — published
`artifacts/kaed/0.1.0-b08ce9f/`, deployed both hosts from it, and **deleted
kubs0's clone**. So the repo is now in a state its own installer does not
describe: `deploy/install.sh` builds from a checkout, and one of the two
fleet hosts has no checkout.

This sprint folds the hand-run path into the repo's canonical flow.

Covered work item: kaed **#1015** — *kaed's canonical installer fetches from
the package store*.

Goal, in one line: **`install.sh` can install a host with nothing but curl
and a store URL, and the fleet deploy uses that on every host — including
the one that still has a clone.**

---

## Decisions

### D1 — One installer with a `--from-store` mode, not a second script

klams (sprint 042) publishes a separate `install-from-store.sh` alongside
its binaries. kaed does not copy that, deliberately.

klams' two installers do genuinely different jobs: the store one drops
binaries into `/usr/local/bin` as root and *deliberately* touches neither
unit files nor config nor service state, because klams' hosts diverge on
purpose. kaed's store install has to do **exactly** what its build install
does — same `~/.local/bin`, same `.prev` rotation, same unit file, same
"config only if absent", same restart-if-running.

Two scripts that must agree on all of that is two scripts that will stop
agreeing. And CLAUDE.md's standing rule is that deploying kaed is
`deploy/install.sh`, not hand-typed steps — one script keeps that true.
So: `--from-store` changes *where the assets come from*, and nothing else.

### D2 — The published artifact is a deploy bundle, not just a binary

`just publish` now puts four files under one version:

```
artifacts/kaed/<version>/kaed-x86_64-linux
artifacts/kaed/<version>/install.sh
artifacts/kaed/<version>/kaed.service
artifacts/kaed/<version>/config.example.toml
artifacts/kaed/<version>/SHA256SUMS
```

Necessary, because a clone-less host has no `deploy/` — `install.sh`'s own
pre-flight fails without `kaed.service`. But it is also the better model:
the unit file that shipped with build X is now recoverable *with* build X.
The build-from-clone deploy never guaranteed that; it installed whatever
unit file happened to be checked out.

Cost is three extra small files per version, all covered by the same
`SHA256SUMS`.

### D3 — No default store URL

`--store URL` or `$KAED_STORE_URL`, and a failure that names both if
neither is set. No baked-in default:

- The tailnet hostname is deliberately not committed to this repo, so a
  correct default is not expressible here anyway.
- Independently of that, a guessed hostname fails later, as a confusing
  `curl` error, instead of immediately naming the variable you forgot.
  (Same reasoning as klams #682/#776.)

### D4 — The store version is derived from the binary, not from git

The old recipe built the version out of `cargo pkgid` and
`git rev-parse --short HEAD`, while the binary stamps *itself* using
`git describe --always --dirty` (`build.rs`). Those agree today by luck.
They stop agreeing the moment kaed has a tag — `describe` prefers it — and
their abbreviation lengths need not match either.

So `just publish` now builds first and reads the version **out of the
binary it just built**: `kaed 0.1.0 (b08ce9f 2026-08-02)` → `0.1.0-b08ce9f`.
The label is then true by construction, which is what lets `install.sh`
assert exact equality after fetching. Borrowing klams' framing: the
checksum proves the transfer, the label check proves the *labelling* — a
binary published under the wrong version installs cleanly and then lies to
`--version`, which is precisely the signal k-homelab's `min_build_date`
floor reads.

### D5 — `publish` refuses a dirty tree; warns off `main`

A published version must name a commit, or the artifact is a rollback
target nobody can reproduce. A `-dirty` stamp additionally trips
k-homelab's floor check for the wrong reason. Hard refusal.

Not being on `main` is a *warning*, not a refusal: `deploy-fleet` already
refuses to deploy anything but merged `main`, and this sprint has to
publish from its own branch to prove the path works before merging it.

### D6 — In store mode, fetch and verify happen even under `--dry-run`

Fetching into a temp dir changes nothing on the host, and it is the part of
a store install most worth rehearsing: "would this deploy work?" should
include "is that version actually published, and does its checksum match?"

This is also what makes the whole store path testable — see below.

### D7 — Tested against a real static store, served locally

`tests/install_from_store.rs` serves a throwaway artifact tree over HTTP
from the test process and runs the real `deploy/install.sh` against it, in
a temp `HOME`. Exercises the happy path plus the failures that matter:
checksum mismatch, missing `SHA256SUMS` entry, unpublished version, a
binary whose `--version` disagrees with its label, and a missing store URL.

Publishing to kubsdb cannot be undone (versions are immutable), so the test
suite must not need it.

---

### D8 — `new-token.sh` installs as `kaed-new-token`

Rotation is ongoing operator work — mint, `--rotate`, `--close` — and until
now it required a checkout. kubs0 has not had one since sprint 020, which
means **kubs0's token could not be rotated at all**. That is a live gap, not
a hypothetical one, and it falls out of "the fleet must not need a clone".

So `install.sh` now also installs `deploy/new-token.sh` onto `PATH` as
`kaed-new-token`, in both modes. The script was already self-contained
(`$KAED_CONFIG_DIR` or `$HOME`), so this is a copy, not a rewrite.

---

## What shipped

**`install.sh --from-store`.** One new mode, and the only thing it changes is
where the assets come from: it resolves a version (the store's `latest`, or
`--version`), fetches the bundle into a temp dir, verifies every file against
the published `SHA256SUMS`, asserts the binary reports the version it was
published under, and then runs the *existing* install unchanged. Everything
downstream reads `$ASSET_DIR` instead of `$SCRIPT_DIR` — that one variable is
what keeps the two modes from drifting.

Fetch-then-verify-then-install, in that order and in separate passes: a
checksum failure on the third file must not leave a new binary already
swapped in. Confirmed against the real store — two different failures, and
the host stayed on the build it was running.

**`just publish` ships a bundle.** Binary + `install.sh` + `kaed.service` +
`config.example.toml` + `new-token.sh`, one version, one `SHA256SUMS`. Plus
the guards from D4/D5: version read out of the binary, dirty tree refused,
`-dirty`/`unknown` stamps refused, and `--no-latest` when off `main`.

**`tests/install_from_store.rs`.** Nine tests driving the real
`deploy/install.sh` against a static store served out of the test process:
the happy path, that store mode installs what it *fetched* rather than what
is in `deploy/`, `--version` bypassing `latest`, and five failures —
corrupted binary, file absent from `SHA256SUMS`, mislabelled binary,
unpublished version, no store URL. Publishing to kubsdb is irreversible, so
none of this needs the real store.

**The `deploy-fleet` skill is now publish → install-published → verify**, and
says why deploying from a branch is forbidden rather than merely discouraged.
Rollback gained a second path: any published version, on any host, checkout
or not.

**Docs.** `docs/setup.md` gets *Installing a published build* — written for a
stranger with their own file server, not for this homelab: the layout the
installer expects, the verified bootstrap for a host with no checkout, and
why there is no default store URL. Rotation now documents `kaed-new-token`.

The whole path is exercised end to end in [deploy.md](deploy.md), including
the clone-less bootstrap on kubs0, without either live daemon being touched.

### The bug this sprint found in the last one

Sprint 004's `deploy-fleet` skill extracted the tailnet name with
`grep -o '"MagicDNSSuffix":"[^"]*"'`. `tailscale status --json` pretty-prints
with a space after the colon, so on tailscale 1.98 that matches nothing —
and the failure is not "your grep is wrong", it is a URL like
`https://kubsdb.:4880` failing to resolve, three steps later. Fixed.

Worth noting because it is the same shape as sprint 004's BOM incident:
**a check that silently produces an empty result is worse than one that
fails.** The store URL handling was written the other way round on purpose
(D3) — no default, and a refusal that names the variable.

## Follow-ups

- **Rollback to `0.1.0-b08ce9f` does not work through the store**, because
  that version predates the bundle and holds only the binary. `--from-store`
  fails on it cleanly (and correctly — mixing a new unit file with an old
  binary would be worse), but until a second bundle exists, rolling back is
  `kaed.prev` or `--bin`. Self-resolving: from this sprint's publish onward
  every version is a full bundle.
- **k-homelab's `kaed-service` advisory text is stale for a clone-less
  host** — its `min_build_date` failure says *"fix: git pull &&
  ./deploy/install.sh in the kaed repo"*, and kubs0 has no repo. Belongs to
  korg:932 (the k-homelab recipe sprint), not here.
- **The fleet table is still written in three places** (this repo's skill,
  klams, `sprints/004-fleet-deploy/deploy.md`) — korg #930.
- **No arch but `x86_64-linux` is published.** The installer asks for
  `$(uname -m)-$(uname -s)` and gets an honest 404 elsewhere, which is the
  right failure, but an aarch64 host in the fleet would need `just publish`
  to grow cross-compilation. Not needed yet.
