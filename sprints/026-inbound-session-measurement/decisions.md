# Sprint 026 — decisions

Covers korg WI 2589 (the inbound half of WI 2587's hypothesis 2), under
proposal korg:3060, slice 16 of program korg:3062.

## D-1 — rmcp 3.1.0 **does** have an idle session TTL; WI 2589's central claim was never true

WI 2589 states the mechanism plainly: *"there is **no idle TTL and no
reaper** — the config has `sse_keep_alive`, `sse_retry`, `session_store` and
a `cancellation_token`, and nothing that expires an idle session."*

Every clause of that is a true statement about
**`StreamableHttpServerConfig`** and a false inference about the server. The
expiry knob is not on the server config at all; it is on the **session
manager's** config, one layer down:

```rust
// rmcp-3.1.0/src/transport/streamable_http_server/session/local.rs
pub struct SessionConfig {
    /// Defaults to 5 minutes. Set to `None` to disable (not recommended
    /// for long-running servers behind proxies).
    pub keep_alive: Option<Duration>,      // DEFAULT_KEEP_ALIVE  = 300s
    pub init_timeout: Option<Duration>,    // DEFAULT_INIT_TIMEOUT = 60s
    pub completed_cache_ttl: Duration,     //                       60s
    ...
}
```

`LocalSessionManager` derives `Default`, so it carries
`SessionConfig::default()` — and `server.rs` builds the service with
`Arc::new(LocalSessionManager::default())`. kaed has therefore had a
**300-second idle session TTL and a 60-second init timeout** for its whole
life.

The reaper is the session worker's own select loop
(`local.rs:1107–1127`): `keep_alive_timeout` is re-armed on every iteration
and raced against the event channel, so any activity resets it and a session
idle past `keep_alive` quits with `WorkerQuitReason::IdleTimeout`. rmcp's own
doc comment on the field describes precisely the failure WI 2589 hypothesised
— *"such sessions become zombies"* — as the thing this default exists to
prevent.

**rmcp has been pinned at 3.1.0 since sprint 001** (`git log -S` on
`Cargo.lock` finds one commit, `3cb8ba1`, the walking skeleton), so this was
not a dependency bump that closed the gap between the item being filed and
now. The knob was there on 2026-09-12 when the 339 descriptors were counted.

The lesson generalises past this item and is why it is a decision rather than
a note: **rmcp splits one subsystem's configuration across two structs, and
reading the one the constructor takes is not reading the configuration.**
A claim that a knob does not exist needs the grep to have covered the
manager, not only the server.

### Fired, not just read

A code reading can be wrong about what a running binary does, so the TTL was
triggered against the deployed instance rather than asserted from source. A
`2025-11-25` session was opened on kai at 21:58, used at 21:59:39, then left
strictly alone — any request re-arms the timer — and re-probed at 22:05:21,
**T+330s idle**:

```
HTTP/1.1 404 Not Found
Not Found: Session not found
```

Retired between 300s and 330s after last use. This is a **triggered test, not
a soak**: the criterion is five minutes of idle, and five minutes is
something the sprint that shipped the finding can simply wait out. No soak
work item was created, and none was warranted.

It settles WI 2589's second acceptance bullet as a side effect — *"a client
that reconnects after a reap gets a comprehensible error"* — which is
`404 Not Found: Session not found`, and is the concern that made option 1
look expensive when the item was written.

## D-2 — The 2026-09-12 count was the fan-out outrunning the reaper, not an absent reaper

If sessions never expired, the inbound sockets would be **held SSE streams**,
and a held stream has a signature: `sse_keep_alive` is 15s, so kaed writes to
it every 15 seconds and `ss -tanpi` shows `lastsnd`/`lastrcv` under 15000 ms
continuously. WI 2589 recorded exactly that on 2026-09-12 — *"`lastrcv` ≈
11 s — live SSE streams being kept alive"*.

The same discriminator, re-run today, separates the two populations on one
host in one command, and it validates itself in place:

| socket | direction | negotiated | `lastrcv` |
|---|---|---|---|
| 2 sockets to peer hosts | **outbound** (kaed's own peer client) | `2025-11-25`, pinned by `fleet::PEER_PROTOCOL_VERSION` (016 D-2) | **6.6 s, 10.8 s** |
| 8 sockets on :4870 | **inbound** (agent clients via `tailscale serve`) | — | **90 s – 251 s** |

The outbound pair are *known* to be legacy sessions — 016 D-2 pins them
there deliberately — and they show the sub-15s keep-alive signature. The
eight inbound sockets do not, by an order of magnitude. They are HTTP
keep-alive connections with **no held SSE stream behind them**.

So the 339 of 2026-09-12 were real, and they were legacy peer sessions being
created by the fan-out recursion faster than a 300-second TTL could retire
them. 025 removed the recursion; the count fell with it. The reaper was never
missing — it was being outrun.

## D-3 — Nothing is changed, and that is the finding

WI 2589 framed two closures, and this measurement removes the premise under
both:

1. **A custom `session_store` that expires idle sessions** — unnecessary.
   `SessionConfig::keep_alive` already does this, and kaed takes the default.
   Had a TTL been wanted, the change was never a custom store; it was one
   field on `LocalSessionManager`.
2. **Turning `legacy_session_mode` off** — unwarranted. It would change the
   protocol behaviour kaed presents to every `2025-11-25` client on the
   fleet, including every gateway's own peer client (016 D-2), to fix an
   accumulation that is not happening.

Neither ships. The honest close is the measurement, and the reason this is
written as a decision rather than left implicit: a later session reading
CLAUDE.md's 025 bullet would otherwise find the inbound half described as
"deliberately still open" and reach for one of two changes that the evidence
says are both wrong.

## D-4 — The protocol revision is read off the wire, not out of a log

The overseer asked which revision each connected client negotiated. kaed does
not log it: `RUST_LOG=kaed=info`, and the only per-request logging on the
auth path is the 401 warning. Rather than add logging for one measurement,
the revision is established from the two things already observable:

- **What the server does per revision**, measured directly against the
  running instance. `initialize` at `2026-07-28` returns **no
  `mcp-session-id`** header; the same call at `2025-11-25` returns one, plus
  the `retry: 3000` priming event. That is SEP-2567 working as documented —
  a `2026-07-28` client is served statelessly and holds no session.
- **The keep-alive signature of D-2**, which tells a session holding a stream
  from a connection that is not.

Together those settle it without a code change: the eight inbound clients on
the gateway hold no session, so they are `2026-07-28`. Adding per-session
revision logging would be a reasonable thing to want on its own merits — but
it is a contract change to the log surface made to answer a question that is
already answerable, so it is not done here.
