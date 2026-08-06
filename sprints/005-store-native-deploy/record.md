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

## What shipped

*(filled in as the work lands)*

## Follow-ups

*(filled in as the work lands)*
