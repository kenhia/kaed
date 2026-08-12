# Sprint 016 — real MCP `2026-07-28` support

*korg proposal 1217, covering #1214. Slice 1 of program 1220 ("MCP
2026-07-28 across the fleet"). Branch `016-mcp-2026-07-28`. Started
2026-08-12.*

## Goal

Sprint 015 capped kaed's advertised protocol at `2025-11-25` because kaed
did not emit what `2026-07-28` requires, and a client that believed the
echoed version registered zero tools. That cap was explicitly tonight's
answer, not forever's (015 D-1). This sprint emits what the revision
requires, then removes the cap.

## What was actually missing

One thing, on one result: the SEP-2549 cache metadata (`ttlMs`,
`cacheScope`) on paginated results. kaed advertises tools only, so
`tools/list` is the sole affected result type — `CallToolResult` carries no
cache metadata in any revision.

Verified against the vendored crate rather than taken from the ticket:

- `rmcp::model::paginated_result!` already gives every `List*Result` an
  `Option<u64> ttl_ms` and `Option<CacheScope> cache_scope`, with
  `with_ttl_ms` / `with_cache_scope` builders. The
  `#[tool_handler]`-generated `list_tools` sets both to `None`.
- The revision's other server-side pieces are already rmcp's:
  `resultType`/MRTR (`strip_result_type_for_legacy_peer`), the inline
  discover lifecycle, `_meta` required-key validation
  (`missing_required_keys`), and SEP-2164 error-code mapping.
- `DiscoverResult` already carries **required** (non-`Option`) `ttl_ms` and
  `cache_scope`, filled in by rmcp's default `discover`. No gap there.

So: two fields, one hand-written method. Not an rmcp port and not an rmcp
upgrade. The ticket's analysis held up in full.

## The part that was not in the ticket

**rmcp 3.1.0's client cannot drive a `2026-07-28` session.** The handshake
succeeds and the first real call is rejected by rmcp's *own* server for the
per-request `_meta` the revision requires and the client never sends. #1214
noted this as a limit on what the test suite could prove; it is also a live
hazard, because **kaed is a client too** — every gateway proxy hop to
`kubs0:*` / `kubsdb:*` goes out through `fleet.rs` as an rmcp client using
`ClientInfo::default()`, i.e. `ProtocolVersion::LATEST`.

Today `LATEST` is `2025-11-25`, so nothing is broken. The rmcp release that
promotes `2026-07-28` would break every proxied call on kai, silently,
through a dependency bump — sprint 015's D-3 trap pointing the other way.
Pinned as `fleet::PEER_PROTOCOL_VERSION` (D-2), with a test that fails when
rmcp catches up.

## What shipped

**`src/server.rs`**

- `PROTOCOL_VERSION` moves to `2026-07-28`; `SUPPORTED_PROTOCOL_VERSIONS`
  gains it. `get_info` still pins the fallback to the same constant
  explicitly — 015 D-3 stands on its own and is not a consequence of the
  cap.
- Hand-written `list_tools` on `impl ServerHandler for KaedServer`, the same
  trick `call_tool` uses for gateway routing: `#[tool_handler]` generates
  the method only when it is absent. It sets `ttl_ms` and `cache_scope`, and
  **only for peers that negotiated `2026-07-28`+** (D-1).
- `clamp_protocol_middleware` deleted (D-3), along with
  `MAX_REQUEST_BODY_BYTES`, which existed only to bound its buffering.

- `proxy_to_peer` stamps an absent `result_type` with `COMPLETE` when the
  serving session is `2026-07-28`+ (D-4) — the mirror of rmcp's
  `strip_result_type_for_legacy_peer`, and the fix for the one thing the
  live test caught.

**`src/fleet.rs`**

- `PEER_PROTOCOL_VERSION` — the revision the gateway *asks* a peer for,
  pinned at `2025-11-25`, deliberately below what kaed serves (D-2).

**`tests/http.rs`** — five tests, all against the real server over real HTTP:

- `tools_list_carries_cache_metadata_at_2026_07_28` — the sprint's gate. A
  raw probe negotiates `2026-07-28`, then drives a conformant sessionless
  `tools/list` (`_meta` keys, `MCP-Protocol-Version` and the SEP-2243
  `Mcp-Method` header): 12 tools, `ttlMs` a number, `cacheScope` `"public"`.
  Raw because rmcp's client cannot make this request.
- `tools_list_omits_cache_metadata_for_legacy_peers` — the other half of
  D-1, over a real `2025-11-25` session.
- `negotiation_answers_the_revision_kaed_implements` — 015's test, renamed
  and rewritten around the lifted cap: `2026-07-28` now negotiates as
  itself, a garbage version still comes back at the ceiling, and the four
  older revisions still negotiate as themselves *with a session id*, so the
  legacy lifecycle is undisturbed.
- `an_rmcp_client_negotiates_its_revision_and_gets_the_tools` — the
  gateway's own path, at the pinned revision.
- `an_rmcp_client_cannot_yet_drive_2026_07_28` — a pin on the *dependency's*
  limitation, asserting the exact `-32602`. It fails when rmcp's client
  catches up, which is the signal to raise D-2's pin.

`raw_post` grew a `raw_post_with` variant taking extra headers; `connect`
grew `connect_at` for a specific revision.

**`tests/gateway.rs`** — `a_proxied_result_is_stamped_for_the_revision_it_
is_returned_on`, D-4's regression test, over a raw `2026-07-28` session
against the two-instance fixture: local call and proxied call both carry
`resultType`, proxied error results included. Raw because the failing
combination — a `2026-07-28` client making a *proxied* call — is structurally
unreachable from rmcp's client, which is exactly why the first canary battery
missed it.

**Docs** — `sprints/planning/architecture.md` records the new ceiling, the
rule that survives the move, and the two-constant table.

`just check` green: fmt, clippy `-D warnings`, 305 tests.

## Canary on kai — `0.1.0-57cf254`

Published `--no-latest` (branch build; the justfile enforces that rule) and
installed on kai alone. kubs0 and kubsdb stay on `0.1.0-0743ff0`, which is
also the rollback target — `~/.local/bin/kaed.prev` on kai, or `--version
0.1.0-0743ff0` from the store.

The server-side battery, over kai's real URL:

| Check | Result |
|---|---|
| `initialize` asking `2026-07-28` | → `2026-07-28`, **no session id** (inline lifecycle — correct for this revision) |
| `tools/list` at `2026-07-28` | 12 tools, `ttlMs: 3600000`, `cacheScope: "public"`, `resultType: "complete"` |
| `initialize` asking `2025-11-25` | → itself, session id issued |
| `tools/list` on that session | 12 tools, **no** `ttlMs`/`cacheScope`/`resultType` (D-1, plus rmcp's own legacy strip) |
| `roots` through the gateway | 7 roots; both peers probed `ok` and `verified` |
| `list kubs0:src` through the gateway | 12 entries, no error |

The last two are the D-2 case running for real: kai on the new build
proxying to peers still on the old one, which is also exactly the state a
staged fleet upgrade passes through.

That is everything this side of the wire. **The client half is the gate that
matters** and cannot be run from kai — a *fresh* Claude Code ≥2.1.227 session
on cleo listing `mcp__kaed-kai__*` and completing one real call. Per 015 D-1
the fleet does not move until it passes.

### It didn't pass — and the battery above is why

`live-test.md` has it: tool registration was green (12 tools, so the #1212
gate itself passed), and **every call to a peer root failed at the client**.
The gateway was returning peer envelopes without `resultType` onto a
`2026-07-28` session. D-4 has the mechanism and the fix.

The row `list kubs0:src through the gateway → 12 entries, no error` is the
one to distrust in hindsight. It ran from an rmcp client, which per D-2
cannot drive a `2026-07-28` session — so it silently tested the `2025-11-25`
path, where the passthrough is correct. **The battery was not wrong; it was
answering a different question than it appeared to.** Any check written from
kai carries that caveat, because the client D-2 describes is the only one
that can ask the other one.

Worth noting the hypothesis that did *not* hold: that this was an artifact of
only kai being upgraded. It is not — the pin is on what kai *asks for*, so an
upgraded peer is still asked at `2025-11-25` and still answers without the
field. Confirmed by asking kai's own new build for a `2025-11-25` session and
watching it omit `resultType` too. Deploying further would have spread the
build without closing the hole.

## Second canary on kai — `0.1.0-a7f81ef`

D-4's fix, kai only again. The paths the live test failed on, re-run raw at
`2026-07-28`:

| Call | `resultType` |
|---|---|
| `stat kai:src …` (local control) | `complete` |
| `stat kubs0:k-homelab` | `complete` |
| `list kubs0:k-homelab` | `complete` |
| `stat kubsdb:src` | `complete` |
| `stat kubs0:src` on a missing path (proxied **refusal**) | `complete`, `isError: true` |

And the regression sweep, unchanged from the first canary: `2026-07-28`
`tools/list` still 12 tools with `ttlMs: 3600000` / `cacheScope: "public"`; a
`2025-11-25` session still gets 12 tools with no cache metadata, and a
proxied call on it still has **no** `resultType` — which is correct, and is
the assertion that would catch this fix over-reaching.

Rollback is unchanged: `kaed.prev` on kai, or `--version 0.1.0-0743ff0` from
the store. kubs0 and kubsdb have still never moved.
