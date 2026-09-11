# Sprint 022 — Close the near-miss gap

**Proposal:** korg:2378 — "Feedback pass 2: close the near-miss gap"
**Covers:** #2373, #2374, #2375, #2376, #2377
**Branch:** `022-near-miss-gap`

## Goal

The second `feedback` triage pass turned up five findings, and three of them
share a shape kaed has never been able to see: **the call that was never
made.** No error, no refusal, nothing in any journal — so
`with_feedback_invite()` could not have caught them by construction, because
there was no response to attach an invite to. They surfaced only because an
end-of-session prompt asked.

Read together the three say one thing: kaed competes with `ssh` at every
decision point, and it loses on round-trip cost whenever the agent cannot see
that it would win. This sprint is about making kaed legible *at the moment an
agent is deciding whether to use it*.

## Premise check (start of sprint)

All five held. Two were worth the minute:

- **#2376** — the reporting agent guessed `.git` was denied and took the ssh
  fallback. Re-verified here with a `dry_run` create at
  `kubs0:src/klams/.git/…` through the kai gateway: clean diff, nothing
  written. The path was writable the whole time, exactly as triage found.
- **#2375** — both kai and kubs0 carry the identical deny set
  (`**/.config/kaed`, `**/secrets`, `**/*.key`), and the live `roots`
  description for `kubs0:k-homelab` still reads "(secrets/ denied)". The
  claim that the tool surface would lie if only the deny changed is correct.

The other three were confirmed by reading the types: `AmbiguousAnchorData` is
`{path, occurrences: Vec<usize>}`; `ReadParams` takes one path and `ListParams`
serves no content; `EditOp`'s eight variants include nothing that removes a
path.

## Cross-project plan

kaed is in the `cross-project-planning` routing table for **KP-5 only** (the
identity grain karc's leg toolbox answers to). This sprint touches none of it.
KP-1's banner contract governs how the sprint closes, not what it builds.

## What shipped

Four of the five items, all in this repo. The fifth (#2375) lands in
k-homelab's manifests and is waiting on Ken — see below.

### #2373 — `ambiguous_anchor` can be acted on now (D-1, D-2)

The payload carries each match's line content, capped at 20 with explicit
truncation, plus a `hint` that names `search` once the list is long. `read`'s
`window` gained `occurrence`, so the read path can disambiguate the way the
write path always could.

The sharp bit was the contradiction the item led with: `invites_feedback()`
suppressed the friction report on this error, justified by a comment claiming
its `data` "already carries the fix" — which was false, and is why the cost
stayed invisible until an end-of-session prompt surfaced it. The exclusion is
correct now, and the comment says what the episode taught rather than just
restating the rule.

### #2376 — a root says what it refuses (D-3)

`roots` publishes `policy: {deny, deny_prefixes, classify, also_enforced}` per
root. Safe by construction: deny matching is lexical and absolute, so the
patterns disclose *policy*, never filesystem contents — the property 001 built
deliberately, finally cashed in.

`also_enforced` is the honesty clause: `.kaedignore`, the in-file marker and
unix ownership cannot be enumerated up front, and a list that quietly omitted
them would be read as exhaustive.

### #2374 — `read` takes `paths` (D-4)

Option (a) from the item. One round trip, whole files, one shared budget spent
in order, partial success with per-file errors, and every entry carrying its
own `version` — which is the whole reason the shell route lost: the reporting
agent took ssh for the round trip and then had to come back through kaed for
the edit bases anyway.

A test feeds a version straight from a multi-read into an `edit` to pin that.
Another pins that a classified file still comes back redacted here, so the
multi-file shape is not a way around R9.

### #2377 — the `delete` op (D-5 … D-9)

Ships with the blob/threshold design the item had already settled, plus the
four decisions it left open: directories refused, unrecoverable deletes gated
on acknowledgement, `recoverable` reported at the time of the act, and
`revert` distinguishing "never recoverable" from "recoverable until the window
passed".

R7's three-places rule checked rather than assumed: `delete` addresses a path,
so `resolve_creatable` covers it — pinned by a test, because a new op is
exactly where that invariant gets missed.

## Repaired in passing

`revert` of a create works. It used to refuse with
`revert_of_create_needs_delete`, whose message said the op was "a later
slice" — this is that slice. The full round trip (create → revert deletes it →
revert again restores it) is pinned by a test.

## Gate

`just check` green: `cargo fmt --check`, `cargo clippy --all-targets
-D warnings`, and 344 tests (290 lib + 54 integration), up from 331.

### #2375 — the secrets fence, narrowed (landed in k-homelab)

Ken decided it on 2026-09-11: narrow the rule. `**/secrets` →
`**/secrets/store` + `**/*.age` on kai and kubs0, with kubs0's root
description and the `kaed-service` recipe README moved in the same commit —
otherwise the tool surface would keep saying "secrets/ denied" while the deny
list said otherwise.

**Verified equivalent for the values before changing anything.** All nine
`.age` files live under `secrets/store`, and it is the only `secrets`
directory in any root in the fleet. So the narrowing opens exactly three
plaintext files — `index.yml`, `README.md`, `recipients.txt` — and denies not
one byte less than before.

`**/*.age` is the glob doing the real work. It denies encrypted values by
**file shape** wherever they live, rather than trusting a directory name. The
general form, recorded in the manifests because it outlives this instance: a
deny rule written against a directory name denies whatever else that directory
comes to hold, and stops denying what moves out of it.

Applied with `bin/apply` on both hosts — a restart, not a SIGHUP, because deny
config is startup-only — then verified through the tool surface rather than
assumed:

| check | result |
|---|---|
| `secrets/index.yml` read | serves |
| `secrets/index.yml` `dry_run` edit | clean diff |
| `secrets/store/unifi-controller-user.age` | `denied`, rule `**/*.age` |
| `dry_run` putting a real `sk-ant-` token in the index | `secret_leak`, refused |
| `roots` description | corrected |

The fourth row is the one that matters: the whole case for narrowing rests on
012's write-side leak detection being the real fence, and it is now confirmed
live rather than inferred.

## Caught by the post-deploy smoke test

The `revert` tool's MCP **description** still told agents that undoing a
create "needs a delete op kaed does not have yet" — written truthfully in 009,
and shipped unchanged in the very release that added `delete`. The behaviour
was right and tested; the sentence an agent reads before deciding whether to
try was wrong.

That is this sprint's own failure class, one layer up: #2376 was a root whose
policy an agent could not see, and this was a tool whose description said it
could not do something it now could. An agent reading it would not have
attempted the call, and nothing would have been recorded — the near-miss
again.

Fixed on a follow-up branch, with a **gate** so the surface cannot drift
silently again: `lists_the_tool_surface` now asserts that `edit` advertises
`delete`, that `revert` claims no missing capability, and that `read` and
`roots` name the fields 022 gave them. Verified by re-introducing the old
sentence and watching the test fail, then restoring it.

The general rule, which is why the gate is worth more than the one-line fix:
**a tool description is part of the contract, and nothing was checking it
against the behaviour.** Every other contract surface here has a test.

## Deploy

The fleet still runs `0.1.0 (fdd8647)`, which predates this sprint — confirmed
incidentally when a verification call using the new `window.occurrence` field
came back `unknown field`. The k-homelab half above is live now because it is
config; the code half ships at Phase 7, and will carry 021's non-code changes
along with it as the proposal predicted.

## Deployed

**2026-09-11 — `0.1.0-d0c3cd1` on kai, kubs0 and kubsdb.**

Published from merged `main` to the homelab package store; every host
installed that artifact with `install.sh --from-store`, kai included. The
fleet had been on `0.1.0 (fdd8647 2026-08-19)` since sprint 020, so this
deploy carries 021's non-code changes along with it, exactly as the proposal
predicted — the lag was deliberate, not a missed deploy.

Two publishes: `0.1.0-75525e2` (the sprint merge) went out first, and the
post-deploy smoke test found the stale `revert` description above. The
corrected `0.1.0-d0c3cd1` replaced it fleet-wide within the same session.

| host | binary | check-config | unit | MCP `serverInfo` |
|---|---|---|---|---|
| kai | match | exit 0 | active | `0.1.0 (d0c3cd1 2026-09-11)` |
| kubs0 | match | exit 0 | active | `0.1.0 (d0c3cd1 2026-09-11)` |
| kubsdb | match | exit 0 | active | `0.1.0 (d0c3cd1 2026-09-11)` |

Binary on disk and the server answering the network agree on every host.

### Verified live, not inferred

Each of the sprint's four code changes was exercised against the deployed
fleet rather than trusted from the test suite:

- **#2373** — an ambiguous anchor on a real file returned
  `occurrences: [{line, text}]` with `total` and the hint; `occurrence: 3`
  then returned the third match directly.
- **#2374** — a three-path `read` returned two files with their own versions
  and the missing one reporting `not_found` in its place
  (`requested: 3, returned: 2`). Over raw JSON-RPC, because a client holding
  the pre-deploy schema stringifies the array.
- **#2376** — `roots` carries `policy` per root, **and peer roots carry
  theirs through the gateway verbatim**: kubs0's and kubsdb's deny/classify
  lists arrived with no gateway work, which is 010 D-3 paying off exactly as
  predicted. kubsdb's five 014 classify globs are visible too.
- **#2377** — deleted a file (`recoverable: true`, diff to nothing), then
  reverted the transaction and got the file back byte-identical.
- **#2375** (config, applied earlier) — the narrowed fence is live: kai and
  kubs0 show `**/secrets/store` + `**/*.age`, kubsdb keeps its own
  `**/secrets` untouched, as the item specified.
- The corrected `revert` description was confirmed on the live server by
  `tools/list`, not just in the source.
