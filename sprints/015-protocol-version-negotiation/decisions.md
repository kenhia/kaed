# Sprint 015 decisions

Per-sprint `D-n`. Cross-sprint decisions live in
`sprints/planning/decisions.md` as `PD-n`.

---

## D-1 — cap at `2025-11-25` rather than implement `2026-07-28`

**Considered:** emitting the SEP-2549 cache metadata (`ttlMs`, `cacheScope`)
on `tools/list` and keeping `2026-07-28`. It is a small change — rmcp already
models both fields, and kaed's tool list is static per build, so `cacheScope:
public` with an hour's TTL is an honest description of it. rmcp 3.1.0 also
implements the rest of that revision's server side (MRTR, the inline
lifecycle, `_meta` validation), so the gap really is those two fields.

**Decided:** cap, and file the implementation separately.

The deciding factor is not effort, it is **verifiability from here**. The
evidence for the two missing fields is one client-side error message. Whether
Claude Code then finds a third thing missing cannot be tested from this
repo — there is no `2026-07-28` client in the test suite that exercises what
the real one does (rmcp's own client does not even send the `_meta` keys its
own server requires at that revision). `2025-11-25`, by contrast, is *known*
to work with this client: korg-mcp and klams-mcp serve it today, and kaed
itself worked with client 2.1.222 at that revision.

kaed is currently unusable from cleo. With that as the starting position, the
fix with the highest confidence of restoring service wins over the fix that is
more modern. Implementing `2026-07-28` is worth doing and is filed as its own
work item — with the cap in place it can be developed calmly and rolled
forward, rather than being the thing standing between Ken and a working
editor.

The reverse case, if it comes up: the cap is a rearguard action, and every
client will move to `2026-07-28` eventually. Nothing here says otherwise. It
says: not tonight, and not without evidence.

## D-2 — the cap is enforced at the HTTP boundary, not only in the handler

**Considered:** overriding `supported_protocol_versions()` and stopping
there. This is what the ticket proposed, and it is what the SDK's own
extension point is for.

**Decided:** override it *and* clamp the requested version in an axum
middleware in front of the MCP service.

Handler-only was tried first and measured. It does not work, and it fails
worse than the bug:

| | before | handler-only | handler + clamp |
|---|---|---|---|
| handshake | ok, at `2026-07-28` | **fails** | ok, at `2025-11-25` |
| session id | none (inline lifecycle) | none | issued |
| `tools/list` | rejected client-side, 0 tools | never reached | 12 tools |

The mechanism: rmcp decides *which lifecycle a request belongs to* from the
version in the request body, before dispatch. `2026-07-28` selects the inline
lifecycle, which issues no session id. The handler's answer of `2025-11-25`
is then correct and useless — the client honours it, sends
`MCP-Protocol-Version: 2025-11-25` on the next call, and rmcp routes that as a
legacy request with no session to belong to. `400 Unexpected message, expect
initialize request`.

So the version has to be clamped early enough that the transport and the
handler agree. Rewriting a client's request is not something to do lightly,
and this one is deliberately the narrowest possible: only an `initialize`,
only a version kaed does not implement, only the one field (plus the
`MCP-Protocol-Version` header when the client sent a matching one, because
rmcp rejects an initialize whose header and body disagree). Everything else
passes through byte-for-byte.

**The client is not misled.** It is told `2025-11-25` in the handshake
response, which is exactly what the spec says a server does with a version it
does not support. The rewrite changes which internal path serves the request,
not what the client is told about it.

The middleware is gated on "POST with no `mcp-session-id`", so it never
buffers a request that belongs to an established session — which is every
tool call, including the large `edit` bodies. When it does buffer, the cap is
the same 4 MiB rmcp's own body reader applies, so nothing is refused here that
would have been accepted downstream.

## D-3 — `get_info` pins the fallback too

`ServerInfo::default()` sets `protocol_version` to rmcp's `LATEST`, which is
the value handed to a client whose requested version kaed does not recognise.
Today `LATEST` is `2025-11-25` and the default is right by coincidence. The
next SDK bump that promotes `2026-07-28` to `LATEST` would reintroduce this
exact bug through a dependency update, silently, with no code change to review.

So the ceiling is stated once, as `PROTOCOL_VERSION`, and used in three
places: the supported list, the `get_info` fallback, and the clamp. A
dependency bump can no longer change what kaed claims.
