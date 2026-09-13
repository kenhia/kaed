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

## Cross-repo changes made

**cleo's Copilot `klams` entry, fixed here rather than filed — inviting the
ruling.** Found while checking the same file for kaed's entry: it still sent
`Authorization` and no name, so it was dead too (401 stored, 401 no-credential,
401 unknown name). My first read was that this is klams's roster and therefore
klams's decision, and I said so on the proposal thread. Measuring changed the
answer: `X-Homelab-Agent: ghcp` authenticates against klams **from cleo**
(200, both controls 401), so the name already exists, klams's grain is
per-application, and there was no decision left to own — only a config line
klams's own cutover missed on the historically-missed host. That makes it a
repair under the decision-ownership test, not a filing, and "pre-existing" is
not a reason to file.

Applied with the same script and the same structural asserts, verified from
cleo with both controls, byte-checked (no BOM, LF-only). It mints nothing,
creates no artifact and follows the pattern klams itself established on kai.
klams's own slice korg:2503 can treat this as already done.

A consequence worth having: **none of the three Copilot configs now holds a
credential-shaped header at all** (`Authorization` / `X-API-Key` / `Cookie`
swept on kai, kubs0 and cleo — all `NONE`). cleo's copy is mode `0666` on
Windows and was holding a live klams bearer until this change.

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

## Deployed

**`0.1.0-d1cc186`** — published from committed `main` (squash `d1cc186`, PR #29)
to the package store, then installed from that artifact on **kai, kubs0 and
kubsdb** on 2026-09-13, in that order.

### Pre-restart control, per host — D-1's one risk

The overseer's clearance asked for `kaed check-config` on each host immediately
before its restart. Run, and **exit 0 on all three** — but that is not the check
that covers D-1, and it is worth saying so: run with the *old* binary it cannot
detect a retired field, because the old binary accepts them. The control that
does cover it is the same text scan the new guard performs, and it reported
**0 matches in code and 0 in comments on all three hosts**. The positive control
was run beforehand against a pre-024 config derived from kai's, where the guard
refused and named only `token_file`.

### Verified live, after the restarts

| host | installed | `kaed --version` | unit | MCP `serverInfo.version` |
|---|---|---|---|---|
| kai | `0.1.0-d1cc186` | match | active | `0.1.0 (d1cc186 2026-09-12)` |
| kubs0 | `0.1.0-d1cc186` | match | active | `0.1.0 (d1cc186 2026-09-12)` |
| kubsdb | `0.1.0-d1cc186` | match | active | `0.1.0 (d1cc186 2026-09-12)` |

What this sprint actually changed, smoke-tested against the deployed fleet
rather than inferred from the unit being up:

- **The three 401 shapes.** A stale bearer with no name returns 401 and the body
  names the retirement and the header to send; an unknown declared name returns
  401 and is not downgraded; no credential returns 401. D-2 is live.
- **The Copilot identities survive the restart** — `ghcp-kai`, `ghcp-kubs0` and
  `ghcp-cleo` all 200 against the deployed binary.
- **`roots` through kai's gateway reaches all three hosts**, all `active`, under
  a declared name, with an unknown-name control at 401.
- **A proxied write still lands with no credential anywhere** — the check this
  sprint's deletion most deserved, since `checkout` lost its `token` parameter
  and its `auth_header` call. An `edit` to `kubs0:scratch` through kai's gateway
  applied in 1.1s and kubs0's journal recorded `author=claude-kai, node=kai`.

Nothing on cleo was stopped, restarted or killed.

### Found by the post-deploy verification, filed not repaired

**korg #2537** — kaed's peer client panics a tokio worker on every outbound
connection attempt (`No CA certificates were loaded from the system`, inside
rmcp's `from_config`). It retries and succeeds, so routing is correct and every
check above passed; the cost is **100k+ panic lines per host** and a 10× latency
spread on `roots`: **3.0s on kai against 30.0s on kubs0**, measured with the
panic lines emitted during each call counted.

It predates 024 — earliest occurrence 2026-09-12 17:24:52 PDT, inside sprint
023's build window and before either of 024's restarts. The CA bundle is
*present* on all three hosts, so it is not a missing package, which is what
makes it a decision rather than a repair: the fix is either kaed pinning its own
TLS root store (a dependency-surface and security-posture change) or the
systemd user unit's environment (k-homelab's recipe). The item names both
owners, and notes the thing not to assume — that some attempts clearly do get a
working client, or nothing would work at all.
