# Sprint 015 — the protocol revision kaed actually implements

*korg proposal 1213, covering #1212. Branch `015-protocol-version-negotiation`.
Started 2026-08-12.*

## Goal

kaed was unusable from cleo. Claude Code 2.1.227 connected, received the
instructions, and registered **zero tools** — no disconnect, no error the
user could see. One bug, one evening.

## What was actually wrong

Claude Code ≥2.1.227 requests protocol version `2026-07-28`. kaed echoed it
back, because rmcp's `ServerHandler::supported_protocol_versions()` defaults
to `ProtocolVersion::KNOWN_VERSIONS` — every revision the **SDK** knows,
which in rmcp 3.1.0 includes `2026-07-28`. That default is a claim about the
SDK; kaed was serving it as a claim about itself.

The client took the echo at its word and validated `tools/list` against that
revision's schema, which requires the SEP-2549 cache metadata on paginated
results:

```
Invalid result for tools/list:
  ttlMs: expected number, received undefined
  cacheScope: expected one of "public"|"private"
```

Three validation attempts, then it gave up and kept the session with an empty
tool list. korg-mcp and klams-mcp negotiate `2025-11-25` with the same client
and work — that was the reference behaviour, and the target.

## The part that was not in the ticket

Narrowing `supported_protocol_versions()` is the obvious fix and it is **not
sufficient on its own**. Measured, not reasoned: a real MCP client asking for
`2026-07-28` against a server with only the handler narrowed cannot finish the
handshake at all —

```
Transport error: Transport channel closed, when send initialized notification
```

rmcp routes a request on the version the client *asked for*, before any
handler runs. A body naming `2026-07-28` selects the inline lifecycle, which
issues **no session id**. The handler then answers `2025-11-25` — correctly —
and the client's next call, now carrying `MCP-Protocol-Version: 2025-11-25`
and no session id, is a legacy-shaped request with no session to belong to:
`400 Unexpected message, expect initialize request`.

That is a worse failure than the one we set out to fix, and it would have
shipped looking like a fix. See D-2.

## What shipped

**`src/server.rs`**

- `SUPPORTED_PROTOCOL_VERSIONS` / `PROTOCOL_VERSION` — kaed's own declared
  list, `2024-11-05` through `2025-11-25`, overriding rmcp's default via
  `supported_protocol_versions()`. `get_info` pins the fallback to the same
  ceiling instead of inheriting rmcp's `LATEST`, so a future SDK bump cannot
  move it silently (D-3).
- `clamp_protocol_middleware` — rewrites an `initialize` whose requested
  version kaed does not implement down to the ceiling *before* the MCP
  service sees it, so the transport and the handler agree on what is being
  served and the session is created exactly as it was for pre-2.1.227
  clients. Gated on "POST with no `mcp-session-id`", so no tool call —
  including the large ones — is buffered by it.

**`tests/http.rs`** — two tests, both driving the real server over real HTTP:

- `negotiation_caps_at_the_revision_kaed_implements` — a raw JSON-RPC probe
  per version (the rmcp client cannot ask for a version it does not itself
  intend to speak). `2026-07-28` and a garbage `9999-12-31` both come back
  `2025-11-25`; the four supported revisions come back as themselves. It also
  asserts a session id is issued, which is the assertion that fails on the
  handler-only fix.
- `a_client_asking_for_2026_07_28_still_gets_the_tools` — a real MCP client
  configured the way Claude Code configures itself: handshake completes,
  negotiates `2025-11-25`, all twelve tools enumerate.

**`sprints/planning/architecture.md`** — the served revision is now written
down, with the rule for moving it.

## Follow-up

Implementing `2026-07-28` properly is filed separately rather than folded in
here — see D-1 for why the cap is the right answer *tonight* and not the right
answer *forever*.

It turned out to be bigger than kaed. Ken had independently filed the same
follow-on from another session, and it is now **korg program 1220** — "MCP
2026-07-28 across the fleet" — spanning all three homelab rmcp servers, since
korg-mcp and klams-mcp escaped only because their rmcp still tops out at
`2025-11-25` and the same trap is armed for them on any bump. kaed is slice 1
(proposal 1217 / WI **#1214**), which removes this sprint's cap; korg-mcp
(#1215) and klams-mcp (#1216) follow. This sprint's session filed #1221 for
the same thing; it was closed as a duplicate after its repo-level analysis was
folded into #1214 and the program notes.

## Deployed 2026-08-12 — `0.1.0-0743ff0`

Store-native, whole fleet. Rollback target: `0.1.0-88005bc` (the build that
had the bug) via `--version`, or `~/.local/bin/kaed.prev` on each host.

A branch canary (`0.1.0-bc87367`) went to kai first, published `--no-latest`
per the justfile's branch rule, and was superseded by the merged build. It is
still in the store as a build that names no commit on `main` — a rollback
target nobody should pick.

| Host | `kaed --version` | check-config | unit | `serverInfo.version` |
|---|---|---|---|---|
| kai | `0.1.0 (0743ff0 …)` | ok | active | matches |
| kubs0 | `0.1.0 (0743ff0 …)` | ok | active | matches |
| kubsdb | `0.1.0 (0743ff0 …)` | ok | active | matches |

**The sprint-specific check, on all three over their real URLs:** `initialize`
asking for `2026-07-28` answers `2025-11-25` **and issues a session id**, then
the full client sequence — `notifications/initialized` → `tools/list` →
`tools/call roots` — completes: 12 tools everywhere, 7 roots on kai (its own
two plus both peers', so gateway proxying survived the change), 2 on kubs0, 3
on kubsdb. Asking for `2025-11-25` still negotiates as itself, so nothing was
downgraded that did not need to be.

That is the whole fix verified server-side. The client half — a **new** Claude
Code session on cleo enumerating the tools and completing a real call, which
is the only check that can see this bug's actual failure mode — passed against
the canary before the ship, and the canary is the shipped code (`live-test.md`
has the equivalence and the battery).
