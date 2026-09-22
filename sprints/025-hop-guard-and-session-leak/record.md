# Sprint 025 — the hop guard and the session leak

**Proposal:** korg:2588. Covers **WI 2587** (kaed leaks established sockets
until its 1024-descriptor table fills), and with it the three reports 2587
consolidates: **WI 2537** (the per-connection `No CA certificates` panic),
**WI 2512** (EMFILE on kubsdb with no retry hint) and **WI 2559** (EMFILE on
kai's recursive walk).

**Goal.** Make the fleet's descriptor use bounded by construction. Two code
defects, in the order they matter: a `roots`/`search`/`list` fan-out with no
hop guard recurses forever across the symmetric three-way peer mesh, and
`checkout` builds a session per racing miss — each with its own reqwest client
and connection pool — and replaces the cached one without cancelling it. The
panic and the EMFILEs are what a full descriptor table looks like from inside
reqwest and from inside a directory walk; they are not independent bugs.

## Premise check (start of sprint, against `main` at `685d49b`)

Every falsifiable claim in the four items, checked before any code was
written.

- **WI 2587 — premise holds, with one measurement drifted and one correction
  to the mechanism.**
  - *No hop guard exists.* Holds. `hop`/`forwarded`/`depth` across
    `src/fleet.rs` and `src/server.rs` finds only prose and `list`'s own
    `depth` parameter. Nothing distinguishes a peer-forwarded call from an
    agent-originated one.
  - *Both fan-out sites call the peer's own `roots`.* Holds — the `roots`
    handler and the root-pattern expansion each spawn `probe_roots` per
    routable peer, which calls the peer's `roots` tool. (Line numbers have
    drifted a few lines from the report; the sites are the same two.)
  - *The peer mesh is symmetric.* Holds, verified in all three live configs:
    kai declares kubs0 + kubsdb, kubs0 declares kai + kubsdb, kubsdb declares
    kai + kubs0. One `roots` call anywhere therefore recurses.
  - *`checkout` releases the lock between miss and insert, and the insert
    replaces without cancelling.* Holds, both halves, and `drop_session`
    removes without cancelling too.
  - **Drifted:** the emergency is over. The simultaneous restart of all three
    units at 22:55:58 PDT held; every host sits at 13–14 open descriptors
    now, against 1021–1023 when the item was filed. The soft limit is still
    1024 (hard 1048576) on all three, exactly as filed. So this sprint is
    fixing the cause with nothing currently burning — the standing warning is
    what is holding the line, and lifting it is the deliverable.
  - **Correction to the mechanism, which changes what fix 2 has to do:**
    `RunningService` carries a `DropGuard`, so a dropped session *does*
    cancel its token — asynchronously, and without awaiting the task. The
    uncancelled-eviction leak is therefore real but second-order. What
    actually filled the table is the storm's own concurrency: hundreds of
    simultaneous proxied calls, each a cache miss, each building its own
    reqwest client. Bounding sessions per `(host, author)` is the fix that
    bounds descriptors; awaiting the cancel is the hygiene that makes the
    bound observable.
- **WI 2537 — premise holds, and the mechanism is now exactly located.** The
  panic is `default_http_client()`'s `.expect("failed to build default
  reqwest client")`, reached from `from_config`. rmcp also exposes
  `with_client`, so kaed can build one client itself, once, fallibly — which
  removes the panic by construction rather than by fixing the CA store. The
  item was filed on a design decision between pinning a TLS root store and an
  environmental fix; **neither is needed**, and that is the answer to its
  question.
- **WI 2512 — premise holds.** No `EMFILE`, `retryable` or
  `resource_exhausted` anywhere in `src/`: an EMFILE surfaces as a bare
  `internal`, exactly as filed.
- **WI 2559 — premise holds**, and 2587 already answered its "walker leak or
  low limit?" question: neither, the table was full of leaked sockets. Its
  error-shape half stands on its own and is in scope here.

## What shipped

**1. A hop marker, and the two fan-out sites that honour it.** Every session a
gateway opens to a peer carries `X-Kaed-Hop: <forwarding host>` beside the
identity header (`fleet::Peers::checkout`); the auth middleware stamps a
`Forwarded` extension from it, and `roots` plus the root-pattern expansion
each check it before probing anyone. A forwarded call answers from local
knowledge, and the peers it did *not* probe are still reported — `probe:
{status: "skipped", detail}` in `roots`, `hosts_unavailable` with
`status: "not_probed"` in a pattern search — each naming the hop it came
from. Contract: a new bullet under **R10**. Cross-sprint: **PD-12**, which
also records why the star topology was rejected.

**2. One peer session per `(host, author)`.** The session map holds an
`Arc<SessionSlot>` per key, and the slot's own lock is held across the
connect, so concurrent misses wait for the first session instead of each
building one. Eviction — and a cached session found already closed — cancels
its `RunningService` explicitly rather than leaving it to the drop guard.
Descriptor use on a gateway is now bounded by `peers × authors` whatever the
traffic, and `Peers::sessions_built()` is the counter that says so.

**3. One HTTP client for the process, built fallibly.** rmcp's `from_config`
builds a `reqwest::Client` per transport and `.expect()`s it, which is the
whole of WI 2537's `No CA certificates were loaded` panic: with the table
full, opening the bundle fails. kaed now builds its own once via
`with_client`, with rmcp's own builder settings, and a failure is a structured
error naming the likely causes. `reqwest` becomes a direct dependency at the
version and features rmcp already resolves.

**4. EMFILE says retry.** `KaedError::internal` classifies a descriptor
exhaustion into `reason: "resource_exhausted"`, `retryable: true` and a hint;
the code stays `internal` on 014 D-1's precedent. The MCP instructions gained
the matching sentence, so an agent retries once instead of falling back to
ssh — which is what both WI 2512 and WI 2559 actually did.

**5. A backoff** before rebuilding a session whose transport closed under us.
The retry was immediate, which under exhaustion meant every failure was
attempted twice as fast as it could fail.

### The tests, and the experiments that prove they are gates

Four new integration tests and three unit tests. Each was verified by
**re-introducing the defect and watching it go red** — a gate that passes
either way is worth nothing:

| test | with the defect restored |
|---|---|
| `a_symmetric_peer_mesh_answers_roots_without_recursing` | no hop header → `roots` does not answer in 10 s (it recursed) |
| `a_forwarded_roots_probes_no_peers_and_says_why` | no server-side guard → the peer is probed, `verified: true` |
| `a_forwarded_pattern_search_expands_locally_and_reports_the_peer` | no server-side guard → the pattern reaches the peer |
| `concurrent_proxied_calls_to_one_peer_build_one_session` | pre-025 racy `checkout` → **8** sessions built, not 1 |

The last one is also a lesson worth keeping: the first version of it asserted
on the session *map*, which passes with the bug present — a racing insert
**replaces** the entry, so the map shows one session while eight are alive.
The observable had to become a counter of sessions *built* before the test
could see the defect at all.

The symmetric-pair fixture needed both listeners bound before either config
existed, so `start_instance_on` takes a pre-bound listener; `start_pair` and
friends are unchanged wrappers.

## What did not ship, and where it went

- **Inbound client sessions still never expire** — korg:2589. rmcp's
  streamable-HTTP server has no idle TTL and `legacy_session_mode` is on by
  default, so a `2025-11-25` client holds an SSE stream for as long as it is
  connected, and behind `tailscale serve` its close may not reach kaed at
  all. Peer sessions are bounded now and `2026-07-28` clients are stateless
  by SEP-2567, so what remains is legacy *agent* clients accumulating over a
  host's uptime. Closing it means a custom `session_store` or serving legacy
  clients statelessly — both change behaviour for every client, and the
  measurement that would choose between them can only be taken after this
  deploys. Filed with that measurement named. This is WI 2587's third
  acceptance bullet, and it is not delivered.

  > **Corrected by sprint 026 (2026-09-21):** the measurement was taken and
  > the premise is false — rmcp has had a 300s idle session TTL all along
  > (`SessionConfig::keep_alive`, on the session manager rather than on
  > `StreamableHttpServerConfig`). Nothing accumulates; neither closure
  > ships; WI 2587's third bullet is answered by the measurement. See
  > `sprints/026-inbound-session-measurement/`.
- **`LimitNOFILE`** — k-homelab korg:2590, because the unit's source of truth
  is that repo's recipe and applying it is an ops action on three hosts. The
  item names the decision: raise it for margin, or leave it low precisely
  because a low limit is what made this defect visible in hours instead of
  hiding the next one for weeks.
- **The rmcp upstream report** (a library constructor that panics instead of
  returning an error) is not filed. kaed no longer reaches that code path, and
  opening an issue on a third-party repo under Ken's identity is his call, not
  this sprint's. The finding is recorded here and in 025 D-3 so it is not
  rediscovered.

## Repaired in passing

Nothing. The gate was green on `main` and stayed green; no unrelated defect
surfaced.

## Deploy note

**The standing warning against calling `roots` is lifted by the deploy, not
by the merge** (025 D-5). One un-upgraded host in a symmetric mesh can still
be the level that recurses, so it holds until all three hosts run this build.
Once they do, the two measurements WI 2587 asked for are worth taking and
attaching to korg:2589: `ls /proc/$(pgrep -x kaed)/fd | wc -l` hourly on each
host, and one `roots` through kai timed with the journal's panic count read
before and after.


## Deployed

**`0.1.0-1b80351` (the squash-merge of PR #30), 2026-09-13, all three hosts** —
published to the package store from clean `main` on kai with `just publish`,
then installed from that artifact on kai, kubs0 and kubsdb with
`install.sh --from-store`. No config was touched on any host.

| host | installed | `kaed --version` | unit | MCP `serverInfo.version` |
|---|---|---|---|---|
| kai | `0.1.0-1b80351` | matches | active | matches |
| kubs0 | `0.1.0-1b80351` | matches | active | matches |
| kubsdb | `0.1.0-1b80351` | matches | active | matches |

One verification detail worth keeping for the next deploy: **kubsdb's `[auth]`
does not carry `claude-kubsdb`, and should not** — no agent runs there, so the
identity the round-trip check must use is one kubsdb actually allow-lists
(`claude-kai`). The first attempt used the host's own name and got a 401 whose
body named the problem exactly, which is the check working rather than
failing.

### Verified live — this is WI 2587's second acceptance bullet

The fan-out that was the storm's trigger, with all three hosts on this build:

- **Five sequential `roots` through kai's gateway: 0.057 s each, 3/3 hosts
  `active` and `verified` every time.** Before: 3 s on kai and **30 s** on
  kubs0, with a peer intermittently reported unreachable.
- **A fleet-wide `search` over `*:*`: 0.517 s, 8 roots searched, no host
  unavailable.** The pattern was `X-Kaed-Hop`, which exists only in kai's
  tree — so the search demonstrably reached and read the other hosts rather
  than quietly narrowing.
- **Descriptors flat at 16 on every host** after those six fan-outs, from a
  14–15 idle baseline. Before the fix the fleet sat at 1021–1023 of 1024.
- **Zero panic lines on any host** across the whole window, against ~50k an
  hour each before.

### The standing warning is lifted

The instruction not to call `roots`, or `search`/`list` with a root pattern,
is withdrawn as of this deploy: all three hosts run the guard, so there is no
level left that can recurse. karc legs are unblocked — `start-sprint` calls
`roots`, which is what the warning had stopped.

### Still open, deliberately

`ls /proc/$(pgrep -x kaed)/fd | wc -l` hourly over a day of real traffic is
the measurement korg:2589 needs, and it can only be taken now that this is
live. Six fan-outs proves the bound holds under a burst; it does not prove
inbound client sessions stop accumulating over a host's uptime, which is
exactly what that item is for.

### Follow-on: `0.1.0-5c89bc5`, the descriptor headroom (korg:2590)

Ken asked for the file-handle limit raised rather than left as a decision, so
it shipped the same day: `LimitNOFILE=65536` in `deploy/kaed.service` (PR #31),
published and installed on all three hosts, verified at **65536 soft and hard**
on each with the unit carrying the line and `0.1.0-5c89bc5` answering on the
network.

**The work item's premise about where the limit lives was wrong, and it would
have failed silently.** Both WI 2587 and WI 2590 recorded k-homelab's
`kaed-service` recipe as the unit's source of truth. It is not — the recipe's
own README says so, and `install.sh` overwrites
`~/.config/systemd/user/kaed.service` from this repo's `deploy/kaed.service` on
**every** deploy. A limit set in the recipe, in the installed copy, or as a
`kaed.service.d/` drop-in would have been reverted by the next kaed upgrade
with nothing reported: the quietest form of the second-source-of-truth mistake.
That is why the line is here and the recipe only *asserts* it.

k-homelab's half (`18d3a6d` there): `recipes/kaed-service/apply.sh` asserts
`^LimitNOFILE=` exactly as it already asserts `^ExecReload=`, with the same
diagnosis — absence means the installed unit predates the build that ships it,
so the fix is an install. The README's ownership table says so, including an
explicit "asserts the limit and must never set it". Each host's
`min_build_date` rose to `2026-09-12`.

Two things checked rather than assumed. `bin/apply <host> kaed-service` reports
`ok` on all three, so the assertion passes and no config was touched. And the
assertion was tested in **both** directions against real content — silent on
the shipped unit, firing on the pre-`5c89bc5` one from git — because the two
sprint-025 builds share a build date, so the date floor alone cannot tell a
host running the older unit from one running this.
