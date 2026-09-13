# Sprint 024 — identity-only auth

**Proposal:** korg:2504 (slice 7.7 of program korg:2440, *simplify homelab
secrets*). Covers **WI 2471** (delete the legacy bearer path) and **WI 2490**
(GitHub Copilot's kaed client is 401 on kai and kubs0).

**Goal.** 023 replaced bearer tokens with declared identities and left the
bearer path in place as a rescue hatch, to be deleted once the cutover was
observed from cleo. That observation arrived (overseer, 2026-09-12 15:15
PDT), so the path goes. The same sprint fixes what 023's checklist missed:
the Copilot client on every host, which needed a roster decision kaed owns.

## Premise checks

Both held, and the second one grew.

- **WI 2471.** The code was all still there. The item's own "watch out" — that
  removing the fields before every host's config is clean would stop those
  hosts starting — was re-verified **on the day** rather than inherited from
  023's handoff: `kaed check-config` on kai, kubs0 and kubsdb each reported
  `identity-only auth: no bearer is accepted on this host`, zero identities
  annotated `transition window OPEN`, and no surviving peer-token rows.
- **WI 2490.** Reproduced from each host by replaying that file's own stored
  headers and reading status codes only. kai and kubs0 as filed — and
  **cleo too**, which the proposal asked me to check and which is broken the
  same way. Fixed here rather than filed: same defect, same file shape, same
  roster, same decision.

## What shipped

### WI 2490 — Copilot has a kaed identity on all three hosts

`ghcp-kai`, `ghcp-kubs0`, `ghcp-cleo`, allow-listed on **all three** hosts
(each must list names that never dial it, since they arrive proxied), config
restarted on each — adding an identity is a restart, not a SIGHUP. The grain
decision and its measured basis are D-3; the short version is that kaed names
the machine while klams names the application, and 023's node field does not
make the per-host name redundant because a proxied call resolves to the
gateway's node.

Each host's `~/.copilot/mcp-config.json` had `Authorization` removed and
`X-Homelab-Agent` added **in one change** — the program's shape-(b) client
convention, since that file keeps a token setting for other servers. The edit
script asserts that nothing outside the `kaed-kai` entry's headers moved, and
prints names only, never a value.

Verified from **each host in turn, not from kai on their behalf**, using each
config exactly as written, with a no-credential and an unknown-name control
every time:

| from | identity | stored | no credential | unknown name | journaled |
|---|---|---|---|---|---|
| kai | `ghcp-kai` | 200 | 401 | 401 | `author=ghcp-kai` |
| kubs0 | `ghcp-kubs0` | 200 | 401 | 401 | `author=ghcp-kubs0, node=kai` (proxied to `kubs0:scratch`) |
| cleo | `ghcp-cleo` | 200 | 401 | 401 | `author=ghcp-cleo, node=cleo` |

The kubs0 row is the interesting one: it is the proxied path, and `node=kai`
is the gateway — which is the whole of D-3's argument, observed rather than
argued. cleo's file was checked at the **byte** level afterwards (no BOM,
LF-only), because a config written from PowerShell is where this project has
been bitten before. Nothing on cleo was restarted, stopped or killed.

### WI 2471 — the bearer path is gone

Deleted: `auth_middleware`'s bearer resolution and `token_eq`;
`Identity.token`; `AuthEntry.{token_env, token_file, prev_token_file}`;
`identities_still_carrying_a_token`; both startup warnings and the
`token_env`/`token_file` both-set bail; `AuthEntry::current_token` and
`read_token`; `PeerConfig.tokens`, `PeerTokenEntry`, `resolve_peer_tokens`,
`Peer.tokens` and the peer-token validation loop with its two warnings;
`fleet::PeerTokens`, `Peers.tokens`, `Peers::token_for`, `CachedSession.token`
and `checkout`'s `token` parameter and `auth_header` call;
`AuthState.{peer_tokens, peers_spec}`.

`Peers::new` and `checkout` lost a parameter each; `resolve_identities` lost
its warning branch and became total — it now reads nothing from disk, so it
cannot partially fail. 023 D-6 (it must never filter) is pinned by its own
test rather than left as a comment.

Two things deliberately **not** deleted, each with a decision: the
`Authorization` header is still read, for the 401 diagnostic only (D-2), and
`SIGHUP` still re-resolves the allow-list even though that now changes
nothing (D-4). And one thing added rather than removed: `retired_fields`, so
the fleet-wide "will not start" this deletion introduces names the field, the
sprint and the fix instead of surfacing as a bare serde error (D-1).

### Docs

`docs/setup.md` lost the transition-window section, the three key-table rows
and the closing-the-window runbook; its reload table now says restart for
everything and says why; its verify section grew the two **controls** it was
missing. The public "handing this to an agent" prompt still told the agent to
mint a token into `~/.config/kaed/token` — rewritten, and it now asks for the
two refusals as well as the success, because a probe that only shows the
accepted case passes against a server that accepts anything.

`deploy/config.example.toml`'s transition block is gone (and the example
still passes `kaed check-config`, which is a live proof that the new guard
ignores comment mentions). R13 in the contract records that the window closed
and what replaced the check-order argument; the contract's 401 bullet gains
the third case. `sprints/planning/architecture.md` had a `token_env` config
sample from sprint 001 — corrected.

## Repaired in passing

- **All three hosts' `[auth]` comment blocks were stale in two ways**, found
  while editing them to add the Copilot rows. They described `claude` as
  "Desktop Claude on cleo" — corrected by 023's post-ship handoff to Claude
  *Code* — and carried a paragraph asserting that "every identity carries
  `prev_token_file`" and that kaed warns for any identity without one, which
  019 made true and 023 retired. Both replaced with an accurate block that
  states the grain, the proxied-name rule and the restart requirement.
- **A stale three-line preamble survived directly above `[auth]` on kai**,
  describing `token_file` / `token_env` / `prev_token_file` as live
  machinery. My first splice started at the `[auth]` line, so it replaced the
  block and left prose contradicting it immediately above. Found by the new
  guard's own positive control: run against a pre-024 config derived from
  kai's, the error named only `token_file` — the field actually in use —
  while ignoring the two in those comments, which is the comment-stripping
  and longest-first logic working on a real file rather than a fixture.
  Removed on all three hosts (kubs0 and kubsdb were already clean); all
  three configs now have zero mentions, in code *or* comments, and all three
  still start and serve six identities.
- **My own leak-detection control was wrong and caught itself.** The first
  version of the Copilot edit script asserted the expected headers as
  `before | {X-Homelab-Agent}`, which keeps `Authorization` — so it fired on
  a correct edit. It fired *before* the write, so nothing was changed; fixed
  and re-run. Worth recording because the assert doing its job is the only
  reason a silently-wrong edit did not land.

## Two things that cost a round and are worth not re-deriving

- **The SSE stream stays open behind `tailscale serve`.** A probe that does
  `response.read()` blocks until its timeout instead of returning, so the
  first cross-host verification hung and had to be read frame-by-frame,
  stopping at the first non-empty `data:`. kai's localhost URL closes the
  stream and returned immediately — which made the local proof look easier
  than the remote one and is exactly the shape of misleading green.
- **An `edit` op carries its own `path`; `EditParams` does not.** A `delete`
  also needs its target's `base` version, so the proof scripts' cleanup step
  failed and the files were removed by hand. Not a defect — worth one line so
  the next raw JSON-RPC script does not re-learn it.
