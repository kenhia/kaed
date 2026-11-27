# Sprint 028 — roll out the shell redaction hook; give the leak inventory a home

**Proposal:** korg:5109 · **Covers:** no work items (lines on Ken's catch-up
lists korg:4767 item 5 and korg:4766 item 3) · **Program:** korg:5112 (the
2026-11-27 low-hanging-fruit run) · **Branch:**
`028-redact-hook-rollout-and-leak-inventory`

This sprint ran as a karc leg (`kaed-f06e60`) on kai, under an overseer.

## Goal

Ken ruled on two things at the 2026-11-27 staff meeting:

1. **"Roll out as measured"** (korg:4767 comment 5219). Ship 027's
   `kaed-redact-hook` with the precise tiers only, the matcher
   `Bash|PowerShell`, and honest wording of "about 15%". The
   credential-context tier and the krot-fed digest index stay undecided.
2. **"In kaed"** (korg:4766 comment 5215). Move the values-free leak
   inventory from `kai:~/scratch/leak-inventory` into this repo.

## Decisions

- **D-1: kaed ships the binary. k-homelab owns the switch.** Measured on
  kubs0:k-homelab, every Claude Code hook on a managed host has its own
  recipe that merges only its own `settings.json` entries, matched by a
  marker in the command. Examples are `recipes/ksitrep/settings_merge.py`
  and `recipes/claude-hooks/settings_merge.py`, which manages only
  `kdeskdash-pub` entries. `kaed-service` already says that the binary and
  the unit belong to kaed's `deploy/` and that k-homelab only asserts. So
  the binary goes into kaed's bundle and `install.sh`. The `PostToolUse`
  entry is relayed to k-homelab as exact JSON and is not hand-edited onto
  any host. A sibling leg holds k-homelab in this run, which is why it is a
  relay and not a Branch A edit.
- **D-2: the hook is optional in a bundle; when present, it is verified like
  everything else.** A version published before 028 has no hook, and a
  rollback to it must still install, so absence from `SHA256SUMS` is the
  signal to leave any installed hook alone. A hook that is listed and fails
  its checksum, or reports a different version, refuses the whole install.
  The hook now prints the daemon's stamp (`kaed-redact-hook 0.1.0 (<sha>
  <date>)`), so the same awk holds it to its label.
- **D-3: no `.prev` for the hook.** The off switch for a bad hook is its
  settings entry, and the hook fails open anyway. It does use a temp name
  plus `mv`, like the daemon, because Claude Code may be running it during
  the swap.
- **D-4: `--bin` does not guess a hook.** `--bin PATH` names one binary.
  Installing whatever sits beside it would be a guess, so the hook is left
  as it is and the install says so.
- **D-5: the inventory is an example, as the brief ruled.** It follows the
  precedent of `examples/redact_measure.rs`. `aho-corasick` and `sha2` are
  **dev-dependencies**; aho-corasick was already in the tree. The
  fingerprint stays `sha256[:12]` because that is krot's own
  (`krot-core::sweep::fingerprint`), and joining against it is the
  inventory's reverse check. The scratch tool's `REVERSE_FPS` environment
  variable became `--reverse-fps`.
- **D-6: the never-a-value test runs in `just check`.** The example is
  declared `[[example]] test = true`, so `cargo test` runs its unit test.
  `a_report_never_holds_a_value` builds a three-value corpus covering the
  prefix, known-digest and context tiers. It asserts that no value, and no
  12-character window of one, appears anywhere in the rows or the reverse
  report. It also asserts that every value was found, so a silent
  regression to an empty report fails too.

## What shipped

- `justfile`: `publish` adds `kaed-redact-hook-<arch>` to the bundle, and a
  new `leak-inventory` recipe runs the example.
- `deploy/install.sh` fetches, verifies and labels the hook when the bundle
  lists it, and installs it to `~/.local/bin/kaed-redact-hook`. Its header
  says it never switches the hook on.
- `src/bin/kaed-redact-hook.rs`: `--version` prints the daemon's stamp under
  the hook's own name.
- Tests:
  - `tests/install_from_store.rs` gains 4: the hook installed beside kaed; a
    pre-028 bundle that still installs; a wrong label that refuses; a
    tampered hook that refuses.
  - `tests/redact_hook.rs` gains 2: the version stamp, and an unreadable
    index that fails open to the index-free tiers.
- `examples/leak_inventory.rs` is the scratch crate, restructured so that
  everything written to a file passes through `inventory()`.
- `docs/redact-hook.md` is new and public-facing. It gives the coverage
  honestly, says what is deliberately off, and has the settings stanza and
  the fail-open contract. README and CLAUDE.md point at it.

## Verified on this branch

- `just check` is green.
- **Fail-open** on the release build from this branch, with a freshly
  minted, full-length fake `ghp_` token. The first attempt minted a token
  too short to trip the prefix tier, so "token absent" meant "no output".
  That attempt was discarded and the proof re-run.
  - Garbage input, empty input and an unexpected `tool_response` shape each
    exit 0 with empty stdout.
  - A missing journal, a corrupt journal and the real one each exit 0, with
    the token sealed as `⟨kaed:ghp@…⟩`.
  - 20 calls take about 15 ms in total.
- **Inventory**: one manual run on kai over the kagviz mirror took 3m23s.
  It read 1,939 transcripts against a 27-digest index and found 256
  distinct candidate values:
  - `context unshaped` 159, `context shaped` 65, `known_digest` 9
  - provider prefixes: `sk-ant-` 9, `AIza` 7, `ghp_` 5, `AKIA` 3,
    `github_pat_` 2, `sk-` 2

  Only counts are recorded here. `kai:~/scratch/leak-inventory` is deleted,
  and the klams note `01a1c7a8-…` now points here.

## Not done here, and why

- **The settings entry.** It is k-homelab's (D-1), and it is sequenced
  after the overseer says the hosts are quiet. The exact JSON is in
  `docs/redact-hook.md` and in the wrap-up handoff.
- **cleo.** `cargo build --target x86_64-pc-windows-gnu --bin
  kaed-redact-hook` fails with 22 errors. The hook links the whole kaed
  library, and `perm.rs`, `fsops.rs`, `journal.rs` and `server.rs` are
  unix-only. kaed also has no Windows delivery route. Getting it there
  takes either a `cfg(unix)` pass over the daemon modules or splitting
  `leak`/`secrets`/`hook` into a portable crate, plus a way to put a binary
  on cleo. That is a decision for Ken, not a repair.
- **Other agent hosts** (kbook2, komarchy, kimac, thrall, kit) run no kaed,
  so the bundle does not reach them. kubsdb gets the binary with the
  deploy, because it is in the fleet, but it has no Claude Code sessions
  and no settings entry is proposed for it.
