# Sprint 001 live test — findings

First use of kaed as a real remote editor by a client that was not the
machine running it. Written *through* kaed itself, over the ts.net
endpoint, as transaction 4.

## Who ran the test

| | |
|---|---|
| Agent | Claude Opus 5 (`claude-opus-5`), Claude Code in the Claude Desktop app |
| Client host | **cleo** — Windows 11 Pro 10.0.26200 |
| Target host | **kai** — kaed 0.1.0, systemd user unit, root `home` = /home/ken |
| Endpoint | `https://kai.<tailnet>.ts.net:4870/mcp` (tailscale serve → 127.0.0.1:4870) |
| kaed identity | `claude` (bearer `KAED_TOKEN_CLAUDE`) |
| Date | 2026-08-02 (UTC; evening of 2026-08-01 local on cleo) |
| Testbed | `/home/ken/src/testing/Toons` — an old Flask/Battle.net repo cloned for the purpose |
| Journal | txns 2 and 3 (txn 1 was the deploy-day dogfood on kai itself) |

Client wiring: `kaed` added to `C:\Users\kenhi\.claude.json` under
`mcpServers`, same http + Authorization-header shape as klams. Note that
`claude_desktop_config.json` on cleo has no `mcpServers` block at all —
Desktop's own connectors live in claude.ai settings, so Claude Code's
`.claude.json` is the file that matters for an agent-driven client.
A restart was required; MCP servers are only loaded at session start.

## Method

Every result below was checked **out-of-band over ssh** — `git diff`,
`py_compile`, direct sqlite reads of journal.db — rather than trusting
what the server reported about itself. That distinction is the whole
point of a verified-write editor, so the test refused to assume it.

## What passed

| Check | Result |
|---|---|
| `roots`, `list` (depth 3, gitignore-aware) | correct; `.git` hidden, `.idea` shown |
| `read` whole file + `numbered` | correct, returns `version` |
| `search` regex + glob | 21 hits, each carrying its file's `version` |
| search → edit with no read between | version from `search` matched `read` exactly |
| `dry_run` | `applied: false`, diff byte-identical to the real run |
| `edit`, 2 ops, 1 file | applied, `txn_id: 2` |
| on-disk verification via ssh | `git diff` matches the returned diff; `py_compile` clean |
| `version_conflict` | correct, with `actual_version` **and** a delta diff |
| re-anchor from the conflict with no re-read | worked; the delta alone was sufficient |
| atomicity | create + deliberately bad anchor → `anchor_not_found`, canary file never reached disk |
| journal | txns attributed to `claude`, intents recorded, `git_head`, per-file line counts |

The conflict was forced honestly: an `ssh` process inserted a line into
`battlenet.py` behind kaed's back, and the next edit was submitted
against the now-stale version. kaed refused it and handed back exactly
what had changed.

## Edits made to the testbed

Real fixes, not filler. Both are latent crashes found while reading:

- `toons/battlenet.py:82` did `toon_cache[toon_key].validUntil` — attribute
  access on a dict that line 95 populates as a dict. `AttributeError` on
  every cache hit, so the toon cache could never once have worked.
- `toon_avatar` dereferenced `toon['thumbnail']` without handling the
  `None` that `toon_from_bliz` returns on a failed Blizzard lookup.

The repo was left with exactly those two fixes plus a `dict` annotation.
Nothing from the conflict or atomicity tests survived — confirmed by
`git diff` and `git status --porcelain`.

## Findings

### 1. Root at $HOME exposes kaed's own token and SSH keys — korg WI #908

`read` on `.config/kaed/env` returns `KAED_TOKEN_CLAUDE` in plaintext:
the service hands out the credential that gates it. `.ssh/id_ed25519` is
reachable too (confirmed by `stat` — deliberately not read). Since
`edit` shares the root, a client can also *rewrite* the auth file, which
makes this a lockout path and not only a disclosure one.

Impact today is bounded — one client, one token, Ken's own host — but it
does not survive the multi-client story `[auth]` already anticipates.
Preferred fixes: stop rooting at `$HOME` (declare explicit project roots),
and/or a denylist enforced in the path resolver so it covers every tool
uniformly rather than per-tool.

**Action outstanding: the live `KAED_TOKEN_CLAUDE` value is now in a
Claude Code transcript on cleo and must be rotated.**

### 2. Journal blobs store whole files — korg WI #909

`blobs` keeps complete pre- and post-images of every touched file for
`retention_days` (30). Editing any file containing a credential copies
that credential into journal.db, outliving the edit. The testbed makes
it concrete: `toons/localconfig.py` holds Battle.net client/secret, and
one anchor_replace anywhere in it would snapshot the whole thing.

The journal also ingested version `0dd410aa481de049` — the ssh-injected
line kaed never authored, captured as the pre-image of the next
transaction. So third-party content lands in there too.

This is a deliberate-decision item, not a clear bug: whole content is
what makes diff-as-proof and delta-on-conflict work at all.

### 3. Failed transactions leave no trace — korg WI #910

After deliberately triggering both a `version_conflict` and an
atomicity rollback, `sqlite_sequence` for `txns` still read 3. Conflicts
are exactly the signal wanted when diagnosing two agents contending over
a file, or one looping on a stale base — and conflict-rate-per-author is
the first real health metric this service could emit to kmon.

### 4. `actual_version` is undocumented in the contract — korg WI #911

The server `instructions` string says to inspect `data.delta`,
re-anchor and retry, but never mentions the error also carries
`actual_version`. Following the text literally costs a wasted re-read.

## On the cold-start question

The `instructions` string was **sufficient**. The read → `base` → edit
loop and the conflict recovery were both driven without consulting the
repo, the plan, or the contract notes — only `deploy.md`, and that only
for wiring. Finding 4 is the single gap, and it was found by triggering
a conflict rather than by reading, which is worth noting: the contract
reads as complete right up until you hit the failure path.

## Verdict

The walking skeleton does what it claims. Verified writes, transactional
edits, and honest conflict reporting all hold up against an adversarial
out-of-band mutation, from a different machine, over the tailnet. The
open items are about blast radius and observability, not correctness.
