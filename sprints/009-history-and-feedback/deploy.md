# Sprint 009 deploy — the history tools go live

Deployed **2026-08-07** from merged `main` (`b5b8a80`), published as
`0.1.0-b5b8a80` and installed on both hosts from the package store.

| Host | `kaed --version` | `check-config` | unit | MCP `serverInfo.version` | rollback |
|---|---|---|---|---|---|
| kai | `0.1.0 (b5b8a80 2026-08-07)` ✔ matches `$V` | exit 0 | active | same stamp ✔ | `kaed.prev` = `0.1.0-a9b8ea0`, or `--version 0.1.0-a9b8ea0` |
| kubs0 | `0.1.0 (b5b8a80 2026-08-07)` ✔ matches `$V` | exit 0 | active | same stamp ✔ | same |

`tools/list` over kai's production endpoint returns all ten:
`diff, edit, feedback, journal, list, read, revert, roots, search, stat`.

kubsdb: still no instance, still correct (korg #929).

## No schema change, and the row counts prove it

009 adds a read path, not a migration. Counts captured before and after the
deploy on both hosts, identical:

| Host | txns | txn_files | txn_failures | blobs | feedback |
|---|---|---|---|---|---|
| kai | 41 | 53 | 4 | 70 | 0 |
| kubs0 | 7 | 11 | 1 | 14 | 0 |

Worth doing anyway: a deploy that silently drops rows looks exactly like a
healthy one from the outside, and this sprint touches `journal.rs`.

## What was verified live, beyond "it is up"

**`journal` against real history, both hosts.** Not a synthetic fixture —
the actual accumulated record. Three things landed as designed:

- **`historical` fires on 100% of existing rows.** Every journalled root on
  both hosts is a pre-007 *unqualified* name (`src`, `scratch`, `home` on
  kai; `src`, `k-homelab` on kubs0), so every row comes back labelled
  `unqualified_pre_007`. R8's corollary was written for the `home` case;
  in practice the whole corpus needs it. Nothing was rewritten and no name
  was aliased back into existence.
- **The #910 coverage note fires on kai**, correctly: `txns_from`
  `2026-08-02T05:17:39Z` precedes `failures_from` `2026-08-02T18:02:34Z`, so
  a window reaching back that far now announces that its silence about
  failures is not evidence. That is D-2's whole point, working against the
  data that motivated it.
- **`coverage.notes[0]`** — reads are not journaled — present on every
  response from both hosts.

**`diff` reconstructing a pre-009 blob.** `kai:src`,
`ai/kaed/sprints/006-journal-evidence-pass/README.md`, from a retained
`old_version` written by an older kaed, to `current`: 16 diff lines,
`from_source: journal_blob → to_source: working_tree`, `redacted: false`.
The blob store written by every previous sprint is readable by this one.

**The friction invite, on a real refusal.** A `.pem` under `kai:scratch` →
`code: denied`, `reason: classified_opaque`, and `data.feedback_invite`
present with `required: ["summary"]`. The refusal's own `hint` survived
alongside it. Smoke file removed after the test.

**A refusal that correctly gets *no* invite.** A `..` path escape returns
`outside_root` with no `feedback_invite` — the narrowness in D-5 is real
and not just asserted in unit tests. (First probe of the deploy, chosen
badly on my part; the wrong answer was the right behaviour.)

## Observations

- **kubs0's #910 note fires on a one-second gap** (`txns_from` 00:53:12 vs
  `failures_from` 00:53:13) — that host's first transaction and first
  failure landed in the same second, so the note is *true* but tells nobody
  anything. Left as is deliberately: the alternative is a threshold, and any
  threshold is a magic number that silently suppresses some real gap. Cheap
  to revisit if it turns out to devalue the note that matters on kai.
- `install.sh` left both configs untouched, as designed. No `[peers]` work
  needed — both hosts have had their block since 007.
- Per 008's note, the MCP round trip must use **each host's own token**;
  both probes were run on the host they targeted.
- `feedback` has 0 rows on both hosts. That is the correct state five
  minutes after shipping the channel, and it is also exactly the number the
  roadmap's remaining deliverable is about — an unread channel is worse than
  none.
