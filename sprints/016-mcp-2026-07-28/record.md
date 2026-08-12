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

**Docs** — `sprints/planning/architecture.md` records the new ceiling, the
rule that survives the move, and the two-constant table.

`just check` green: fmt, clippy `-D warnings`, 305 tests.

## Deploy

Not yet. Per 015 D-1 the cap is not lifted and deployed in one motion: the
gate is a live test from a **fresh** cleo Claude Code ≥2.1.227 session, kai
first with the 015 build as the rollback target.
