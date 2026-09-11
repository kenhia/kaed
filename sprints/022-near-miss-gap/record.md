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

## Still open: #2375

The change lands in **k-homelab's manifests**, not here, and it carries a
policy call that is Ken's rather than the sprint's. Raised separately with the
evidence assembled.
