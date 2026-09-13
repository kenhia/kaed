# Sprint 025 — decisions

Per-sprint `D-n`. This sprint also adds **PD-12** to
`sprints/planning/decisions.md` (the mesh stays symmetric; termination is a
hop marker) and a hop bullet to **R10** in the contract.

## D-1 — The hop marker is a header, unchecked, and its value is the forwarding host

The fan-out had to learn to distinguish a call from an agent from a call from
a gateway. Three ways to do that were considered:

- **A depth counter** (`X-Kaed-Hop: 2`, decrementing). Rejected: it encodes a
  budget nobody has a use for. There is no legitimate two-level fan-out in
  this design — patterns are expanded by the instance that was asked (014
  D-5) — so a counter's only distinct behaviour from a flag is the case that
  must never happen.
- **A chain of hosts** (`X-Kaed-Hop: kai,kubs0`), so a peer could skip hosts
  already visited. Strictly more informative, and it would permit a
  legitimate multi-hop fan-out. Rejected as machinery for a fleet shape that
  does not exist: every host declares every other, so one hop reaches
  everything.
- **Presence, with the forwarding host as the value.** Chosen. Presence is
  the whole of the rule; the value is diagnostic, so a `roots` answer can say
  *which* gateway's call it is declining to widen, and a future storm names
  its own origin in one header.

**It is deliberately not authenticated.** Under declared identity there is no
proof of origin to check it against (023 D-5 settled that for node pinning,
and the same argument applies here). Spoofing it narrows the caller's own
answer: a client that sets it by hand gets this host's roots instead of the
fleet's. That is a degradation of its own call, not an escalation, and the one
thing it cannot do is make a fan-out go a level deeper — which is the entire
property being bought.

An unforwardable value loses the *marker*, not the call: `this_host` comes
from config and is a hostname, but if it were somehow not header-safe the
guard degrades to pre-025 behaviour rather than refusing to proxy. A gateway
that cannot proxy is worse than one that can recurse, because the recursion
is now also fixed on the receiving side.

**What a forwarded call may still do is serve an addressed root of its own.**
Only the fan-out is refused, not proxying as such, and that is not laziness:
routing reads the host prefix (R8), so an addressed call terminates at the
host that owns the root by construction. Refusing to chain outright would
also have broken the cross-host `secret rotate` path, where a host serving a
proxied call writes an `also` target on a third host (011 D-5). That case is
legitimate and bounded; the fan-out was neither.

## D-2 — One session per `(host, author)`, held by a per-key lock, and cancelled on eviction

`checkout` took the session-map lock, missed, **released it**, built a
transport, and inserted — so N concurrent misses for one key built N sessions,
and the insert replaced the previous `CachedSession` without cancelling it.

The obvious fix — hold the map lock across the build — was rejected: the build
includes a 5 s connect timeout, and holding the map lock across it serializes
connects to *unrelated* peers. Instead the map holds an `Arc<SessionSlot>` per
key and only the slot's own lock is held across the connect. Concurrent misses
for the same peer wait for the first one's session; different peers still
connect in parallel.

**The correction to the filed diagnosis, because it changes what the fix has
to do.** WI 2587 read the uncancelled replacement as the leak. `RunningService`
carries a `DropGuard`, so a dropped session *does* cancel — asynchronously,
when the **last** `Arc` clone drops, and a caller mid-call holds one for up to
`CALL_TIMEOUT`. So the pre-025 code was self-healing on a 30-second delay, and
what actually filled the table was the storm's concurrency: hundreds of
simultaneous misses, each with its own reqwest client and pool. The fix that
matters is therefore the coalescing, not the cancel. The explicit
`cancellation_token().cancel()` on eviction is still right, and it is what
makes the bound *observable* rather than eventually-true.

Two things this bought that are worth keeping:

- **`sessions_built` is a counter, not a gauge.** The leak was invisible in
  the map by construction — a racing insert replaced the entry, so the map
  showed one session while eight were alive. The regression test asserts on
  the counter; asserting on the map's size passes with the bug present, which
  is exactly the mistake that was made first and caught by re-running the
  experiment.
- **A cached session whose transport has died is evicted on read**, rather
  than handed out to fail once and be retried. Cheap, and it means the retry
  path is for genuine mid-call failures.

The bound is now `peers × authors` sessions per host, whatever the traffic.
That is the number to check against `ls /proc/$(pgrep -x kaed)/fd | wc -l`
after the deploy.

## D-3 — kaed builds its own peer HTTP client, once, fallibly

rmcp's `StreamableHttpClientTransport::from_config` builds a fresh
`reqwest::Client` per transport and `.expect()`s it. With the descriptor table
full, opening the CA bundle fails, zero certificates load, and reqwest reports
`No CA certificates were loaded from the system` — a panicking tokio worker
per connection attempt, ~100k lines a host (WI 2537).

`with_client` takes a client instead, so kaed builds one, once, and a failure
is a structured `internal` naming the likely causes. The builder settings are
rmcp's own, kept deliberately: `pool_max_idle_per_host(0)` (no idle socket
belonging to no call, and no delayed-ACK stall) and no redirects (a forwarded
identity header must never be replayed to a redirect target).

**This is the answer to WI 2537's filed question**, which was "pin a TLS root
store, or fix the environment?" — neither. The bundle was always present and
always loadable; the process had no descriptor left to open it with. Pinning
`webpki-roots` would have made kaed stop honouring host CA policy to fix a
symptom of a different bug, and that is worth recording because it was the
more attractive-looking option.

`reqwest` becomes a direct dependency, at the version and features rmcp
already resolves, on the `rustix` precedent in `Cargo.toml` — already in the
tree, declared so a call can be checked rather than trusted.

## D-4 — EMFILE keeps the `internal` code and gains a `reason`, classified at the funnel

WI 2512 and WI 2559 both ended at the same complaint: an EMFILE arrived as a
bare `internal`, so the agent's reasonable move was to give up on kaed and
shell out — the one route the journal cannot see.

**No new error code**, on 014 D-1's precedent: `reason` is the field whose job
this is, and every client already parses `{code, message, data}`. The
classification is `reason: "resource_exhausted"`, `retryable: true`, and a
hint that says what to do — retry once; narrow a deep walk if a narrow read
works where the walk does not.

**Classified inside `KaedError::internal` rather than at the call sites**,
because that constructor is where every IO failure in the process already
funnels: `From<io::Error>`, `fsops`' canonicalize arms, and the `ignore`
walker's own formatted message. Decorating sites means decorating a dozen and
missing the thirteenth — and the thirteenth is what WI 2559 hit. The cost is a
string test, and its false positive is a *path* containing "too many open
files", which buys one wasted retry and no wrong answer.

## D-5 — "Call `roots` first" stays in the instructions

The proposal asked for that sentence to be removed or guarded as part of fix
(1), and the standing warning told every agent not to call `roots` at all. Both
were right while `roots` was the trigger.

**Decided: the sentence stays, unchanged.** The recursion is now impossible by
construction, so `roots` is a bounded two-call probe again, and it is the only
way an agent learns the host-qualified names it must pass. Removing the advice
would leave every client guessing root names to protect them from a defect
that no longer exists — and the guidance would have to be put back, by
someone who no longer remembers why it went.

The standing warning is a different thing and it is now **lifted by the
deploy**, not by this commit: it holds until every host runs a build with the
guard, because one un-upgraded host in a symmetric mesh can still be the
level that recurses.

## Not in this sprint, and why

**The inbound half of WI 2587's hypothesis 2 — client sessions that never
expire — is filed, not fixed.** rmcp's streamable-HTTP server has no idle TTL
and no session reaper; `legacy_session_mode` is on by default, so every
`2025-11-25` client gets a stateful session holding a standalone SSE stream
until it disconnects, and behind `tailscale serve` a client's close may not
reach kaed at all. Peer sessions are now bounded (D-2) and `2026-07-28`
clients are stateless by SEP-2567, so what remains is legacy agent clients
accumulating over a host's uptime.

Closing it means either a custom `session_store` that expires idle sessions or
turning `legacy_session_mode` off — both change behaviour for every client,
and neither has a measurement behind it yet. That is a decision this sprint
cannot make from the evidence it has: the correct next step is to measure
inbound sockets over a day of normal traffic *after* this deploy, when the
storm is no longer drowning the signal. Filed with that measurement named.
