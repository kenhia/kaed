# Sprint 026 — do inbound MCP sessions actually accumulate?

**Proposal:** korg:3060 · **Covers:** WI 2589 · **Program:** korg:3062
(Low-hanging fruit, run 2), slice 16 · **Branch:**
`026-inbound-session-measurement`

Run as a karc leg (`kaed-125c1f`) on kai under an Opus overseer.

## Goal

WI 2589 is the half of WI 2587's hypothesis 2 that sprint 025 did not close:
whether rmcp's streamable-HTTP server, *"having no idle TTL"*, lets legacy
clients pile up held SSE streams for a host's whole uptime. It was filed with
a measurement named rather than a fix guessed, and 025 deployed fleet-wide on
2026-09-13 — so eight days of real traffic were already banked.

The overseer's call, in the proposal notes, was that this is a **trigger, not
a soak**: the sample already exists, so one measurement today answers the
question the item framed as a day of hourly sampling. That turned out to be
right, and the answer arrived faster than expected because the premise
collapsed under the first grep.

## What the premise check found, before any measuring

WI 2589 names its mechanism precisely — *"there is no idle TTL and no reaper
— the config has `sse_keep_alive`, `sse_retry`, `session_store` and a
`cancellation_token`, and nothing that expires an idle session"* — and that
description is accurate about `StreamableHttpServerConfig` and wrong about
the server.

rmcp keeps the expiry knob on the **session manager's** config:
`SessionConfig::keep_alive`, defaulting to **300 seconds**, alongside a 60s
`init_timeout`. kaed constructs its service with
`Arc::new(LocalSessionManager::default())`, so it inherits both. rmcp has
been pinned at 3.1.0 since sprint 001, so this was not a version that moved
under the item.

That is D-1, and it changes the shape of the whole sprint: the measurement
stops being "pick between two fixes" and becomes "confirm there is nothing
to fix".

## The measurement

Taken 2026-09-21 21:58 PDT, on each host, **from that host** — the counters
read `/proc/<pid>/fd` and `ss` for the local kaed process, so kai was
measured locally and kubs0/kubsdb over ssh with `BatchMode=yes
ConnectTimeout=10`, both returning rc 0 with output (not an ambiguous
non-zero).

| host | open fds | limit | process uptime | inbound ESTAB | outbound peer ESTAB |
|---|---|---|---|---|---|
| kai (gateway) | **23** | 65536 | 8d 02h | 8 | 2 |
| kubs0 | **15** | 65536 | 8d 02h | 2 | 0 |
| kubsdb | **14** | 65536 | 7d 00h | 1 | 0 |

Against the proposal's stated post-deploy idle baseline of 13–16 and its
"flat is roughly 16–30" band: **flat**. kubs0 and kubsdb sit *below* the
band; kai sits inside it, and kai is the gateway, the busiest host, and the
one that was at 339 descriptors nine days ago.

`LimitNOFILE=65536` is live on all three — 025's unit change (`5c89bc5`)
confirmed in place rather than assumed.

### The second sample, which is the part that actually settles it

A single count cannot distinguish "flat" from "climbing slowly", so kai was
counted again five minutes later, at 22:03 PDT:

| kai | 21:58 | 22:03 |
|---|---|---|
| open fds | 23 | **17** |
| inbound ESTAB | 8 | **2** |
| outbound peer ESTAB | 2 | 2 |

**Six inbound connections closed inside five minutes, and the descriptor
count went down.** Those are the sockets that had been idle 90–251 seconds at
the first count — they were released, on their own, with no restart and no
intervention. Whatever else is true, the inbound population is not monotonic,
which is the property WI 2589 exists to test. The outbound peer pair is
unchanged at 2, exactly as 025 D-2 bounds it (`peers × authors`).

### Counting the observer, not subtracting it

The overseer session is itself a live kaed client on kai while this runs, and
so is this leg. Both are **included** in kai's 8. One inbound socket was
measurably active (`lastrcv` 1.4s) at the moment of the count; the rest were
idle 90–251 seconds. Nothing has been netted out.

### The reaper, demonstrated end to end rather than read off the source

D-1 is a code reading, and a code reading can be wrong about what a running
binary does. So the TTL was fired against the live instance on kai, as a
**triggered test** — the criterion is 300 seconds of idle, and 300 seconds is
something a sprint can simply wait out, so this is not a soak and did not
become one:

| step | time (PDT) | result |
|---|---|---|
| `initialize` at `2025-11-25` | 21:58:5x | `mcp-session-id: c4fad3da-…` issued |
| `tools/list` on that session | 21:59:39 | **HTTP 200**, tool list returned |
| *(left strictly untouched — any request re-arms the timer)* | | |
| `tools/list` on the same session | **22:05:21** (T+330s idle) | **HTTP 404 — `Session not found`** |

The session was retired somewhere between 300 and 330 seconds after its last
use, which is `DEFAULT_KEEP_ALIVE` to the second. The reaper WI 2589 says does
not exist ran, on the deployed build, while being watched.

It also answers the item's own second acceptance bullet in passing — *"a
client that reconnects after a reap gets a comprehensible error"*. It gets
`404 Not Found: Session not found`, which is exactly that.

## Which revision each client negotiated

kaed does not log the negotiated revision (`RUST_LOG=kaed=info`, and the only
per-request log on that path is the 401 warning), so it is read off the wire
instead (D-4). Against the running instance on kai:

- `initialize` with `"protocolVersion": "2026-07-28"` → **no
  `mcp-session-id`** response header. Stateless, per SEP-2567.
- `initialize` with `"protocolVersion": "2025-11-25"` → **`mcp-session-id:
  c4fad3da-…`**, plus the `retry: 3000` priming event. A session is created.

So "holds a session" and "negotiated `2025-11-25`" are the same fact, and the
socket timers tell them apart — which is the discriminator D-2 turns on:

| population | negotiated | `lastrcv` observed |
|---|---|---|
| 2 outbound sockets to peers | `2025-11-25` (pinned, 016 D-2) | **6.6 s, 10.8 s** |
| 8 inbound sockets on :4870 | — | **90 s – 251 s** |

A held legacy SSE stream gets a keep-alive write every 15 seconds
(`sse_keep_alive`), so it cannot be idle longer than that. The outbound pair
— *known* legacy, because 016 D-2 pins them there on purpose — show exactly
that signature, which validates the discriminator in place rather than
asserting it. The eight inbound sockets miss it by an order of magnitude:
they hold no SSE stream, so they are not legacy sessions. They are
`2026-07-28` clients on idle HTTP keep-alive connections.

**Every inbound client on the fleet today is `2026-07-28`.** The only
`2025-11-25` sessions kaed has are the ones it opens itself, as a peer
client, deliberately.

## What the 339 of 2026-09-12 actually were

WI 2589 recorded `lastrcv ≈ 11 s` on those sockets — the keep-alive
signature. They were real held sessions, and they were kaed's own peer
clients, minted by the fan-out recursion far faster than a 300-second TTL
could retire them. 025 removed the recursion and the count fell with it. The
reaper was never missing; it was being outrun. That is D-2, and it is the
piece that makes the old number and the new number tell one story instead of
two.

## Gate

`just check` green on the branch: `cargo fmt --check` clean, `cargo clippy
--all-targets -- -D warnings` clean, **357 tests passed, 0 failed** across the
five test binaries. No code changed, so this is a confirmation that the doc
edits broke nothing, not a claim that new behaviour is covered.

## What shipped

**No code change.** Both closures WI 2589 named are removed by the evidence
(D-3): a custom `session_store` is unnecessary because
`SessionConfig::keep_alive` already is one, and flipping `legacy_session_mode`
would change protocol behaviour for every `2025-11-25` client on the fleet —
including every gateway's own peer client — to fix an accumulation that is
not happening.

What ships is the correction, because the false premise was written down in
three places and a later session would have acted on it.

## Repaired in passing

- **`CLAUDE.md`'s 025 bullet claimed "rmcp's server has no session idle
  TTL"** and pointed the next session at a choice between two changes that
  are both wrong. Replaced with a 026 bullet carrying D-1 through D-4. This
  is the repo's live navigational doc; a false mechanism claim there is the
  most expensive kind of stale line.
- **`sprints/025-…/decisions.md` and `…/record.md`** carried the same claim.
  Both **left as written** — a sprint record is what that sprint believed —
  with a dated correction note appended pointing at 026. Rewriting the
  narrative would have destroyed the record; leaving it uncorrected would
  have left two more copies of a falsified premise.

## Not filed, and why

The kimac auth gap turned up in kai's journal during this work —
`ghcp-kimac` and a bare `ghcp` 401ing on a ten-minute cycle through
2026-09-20, stopping about 28 hours before this measurement. **korg WI 2943
already covers it** (`kaed allow-list has no identities for kimac (or
komarchy)`), open, in this project, filed 2026-09-20 with the same finding.
Fresh evidence went on that item as a comment. No second item.

## Answering WI 2587's third acceptance bullet

> *"ending a client session is followed by its inbound socket closing"*

Answered by measurement rather than by a fix, which is the disposition WI
2589 allowed for. For the clients actually on this fleet the bullet does not
arise: they negotiate `2026-07-28`, hold no session, and their connections
are ordinary idle HTTP keep-alives — the longest observed being 251 seconds,
with nothing older surviving across eight days of uptime. For a `2025-11-25`
client, an abandoned session is retired by `SessionConfig::keep_alive` at 300
seconds whether or not its close ever reaches kaed through `tailscale serve`
— which was the specific worry, and it is handled.

## Deploy — deliberately not run

**Phase 7 (`deploy-fleet`) was skipped, by the overseer's explicit ruling**
(korg:3060, clearance comment 2888). This section exists so a later reader
sees a decision rather than a missing step: kaed declares a deploy in
`.sprint-deploy`, and this is the sprint that did not run it.

Three reasons, the third of which the leg could not have known:

1. **No code changed.** A deploy would move all three hosts to a new version
   string for a binary that is byte-for-byte equivalent in behaviour. A
   version bump that does not mean a behaviour change spends the one signal
   anyone has for "something is different here".
2. **A kaed restart is scarce.** WI 2943 (the kimac identities) is already
   waiting on Ken's window precisely because a restart interrupts every live
   agent session. Spending that interruption on a docs-only change spends the
   scarce thing on nothing.
3. **Nine sibling legs of program korg:3062 were running on kai at the time.**
   Restarting kaed under them is the same self-inflicted failure the
   program's sequencing notes forbid for karc.

The fleet therefore remains on `0.1.0-5c89bc5`, which is the build this
sprint measured. **The next code-carrying sprint moves the version.**
