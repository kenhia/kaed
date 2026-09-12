# Sprint 023 — decisions

Per-sprint `D-n`. Cross-sprint decisions are `PD-n` in
`sprints/planning/decisions.md`; this sprint adds PD-10 (the identity model) and
PD-11 (`/etc/klams`), and supersedes the *mechanism* half of PD-4.

## D-1 — A declared name beats a bearer, and an unknown name never falls through

`X-Homelab-Agent` is checked first. If it is present and names an allow-listed
identity, that identity wins — even if a valid bearer rides along. If it is
present and names nothing, the request is **refused**, and does *not* fall
back to the bearer.

That last clause is the whole decision. The tempting implementation tries the
name, fails, and tries the token — which would authenticate a caller as
something it did not claim to be. A caller that says "I am `mallory`" and
holds `claude`'s token is either a misconfiguration or an attempt; either way
the honest answer is a refusal, and the dishonest one is a journal full of
`claude` rows that `claude` did not write.

A whitespace-only header is *not* a declaration and falls through to the
bearer. Pinned by test, because "empty string is a name" is the reading that
turns a stray header into a fleet-wide 401.

Same rule klams settled in its sprint 049 (D-3). Adopted verbatim rather than
re-derived: the program settles this shape once.

## D-2 — The transition window IS the token rows; there is no flag

An identity that still declares `token_env` / `token_file` also accepts that
bearer. Deleting the field closes the window **for that identity**. There is
no `[auth] allow_bearer = true` and no fleet-wide switch.

Two reasons. A separate flag is a second piece of state that can disagree
with the rows, and the question anyone actually asks — "is this identity off
the token yet?" — is then answerable only by reading both. And a flag is
fleet-wide where the cutover is per identity and per host.

`Config::resolve` warns at startup naming every identity still holding one, so
the state of the cutover is readable from the host rather than by opening
three config files by hand. That is the shape 019 arrived at for the mirror
question and it was right.

**A consequence worth stating, because it constrained the whole sprint.**
`install.sh` deliberately never rewrites a config, so the new binary meets the
*old* config file on every host it is deployed to. With
`#[serde(deny_unknown_fields)]`, deleting a field from the struct turns a
deploy into a fleet-wide failure to start. So `prev_token_file` and
`[peers.*.tokens]` are **still parsed** and ignored, and are removed from the
files by the cutover rather than by the binary. Pinned by two tests.

## D-3 — The gateway forwards the caller's name and holds no credential

`[peers.<host>.tokens]` is retired. A proxied call carries
`X-Homelab-Agent: <the caller's own name>` to the peer, verbatim, and the peer
journals under it.

PD-4 chose per-(author, backend) tokens to protect one property: a proxied
edit is attributed to the agent that asked for it, never to the gateway.
**That property is unchanged.** What changed is the proof of the name, and
with it the cost: nine `(backend × author)` credentials for three authors and
three backends, growing as authors × endpoints, with no inventory. PD-4 itself
left this door open — "allowlisted-peer forwarded headers … remain open if
token sprawl actually starts to hurt."

Two consequences:

- **`no_peer_credential` is gone.** There is no credential to be missing, so
  an author the gateway has never heard of is simply forwarded. The refusal
  moves to the backend, where it belongs: a peer that does not allow-list the
  name answers 401 and the gateway reports `peer_credential_rejected` naming
  the identity and the fix (add the name to that host's `[auth]`, and
  **restart** — the allow-list is config shape).
- **A refused identity is not an outage.** `hosts_unavailable` gained
  `identity_refused`, with no `since`. Reporting a host that answered as
  `unreachable` would be a false claim about the world in the one field whose
  job is stopping an agent forming a wrong picture — the same failure korg
  #1089 filed. Before this sprint the case was gated earlier as
  `no_credential`; removing that gate made it newly reachable. **Repaired in
  passing**, pinned by the fleet-search test.

## D-4 — `unknown` is a real answer, and the node is a property of the connection

Four situations record the same node: whois disabled, no address, tailscaled
down, and an address the tailnet does not know. One word for all four, on
purpose — a reader of the journal should not be invited to treat any of them
as more trustworthy than the others. Rows written before the column existed
default to `unknown` too, which is honest: nothing was recorded, so nothing is
known.

The node reaches the journal through `Journal::as_node(&node)`, a per-request
recorder view, rather than as a parameter threaded through `txn::apply` and
every call site that already carries an author. The node belongs to the
connection, not to a transaction, and the wrapper says so.

The cost is that the *unscoped* `impl TxnRecorder for Journal` records
`unknown` — so a call site that forgets to wrap does not fail, it fills the
column with a plausible default forever. That failure is silent, so it has a
gate test (`the_node_scoped_recorder_stamps_and_the_bare_one_does_not`)
asserting both halves.

## D-5 — Node pinning is the only enforcement, it is opt-in, and it is off

WI 2392 says a peer should accept a forwarded identity "only from an
allow-listed gateway peer". The mechanism for that is per-identity `nodes`
plus `[whois] enforce`, and **enforcement is off by default**.

Being straight about why, because the gap between the wording and the
implementation is real: under declared identity there is no proof of origin
available at all. Any host on the tailnet can send `X-Homelab-Agent:
claude-kai` directly to kubsdb, gateway or no gateway. A "gateway allow-list"
that is not backed by an origin check is decoration. The only origin signal
kaed has is the tailnet address, and that is what `nodes` checks.

So the honest statement is the one in SECURITY.md: the perimeter is the
tailnet, the name is attribution, and the node is recorded so a name claimed
from an unexpected machine is visible afterwards. Enforcement exists for hosts
that want it, is dormant policy until a host turns it on — the same shape as
kubsdb's classify globs in 014 — and the program's own rule ("whois recorded,
record-only; enforcement toggle default off") is what keeps it off.

One trap documented rather than designed around: a proxied call resolves to
the **gateway's** node, so pinning a proxied identity means listing the
gateway alongside its own host.

## D-6 — An identity outlives its token, and an unreadable token no longer disables it

Before this sprint, `resolve_identities` **dropped** any identity whose token
would not resolve, with a warning. Under declared identity that is exactly
backwards: the name is the credential, so a missing legacy file would delete
the agent along with its retired secret — and the cutover's own last step
(deleting the token files) would have taken every identity with it.

Now the identity is always built; only the bearer half goes missing, with a
warning. Pinned by test, and by the http test that deletes the token file mid
flight and asserts the name still works.

## D-7 — `[whois]` is top-level, not `[auth.whois]`

klams put these settings inside `[auth]` because its auth config is a struct.
kaed's `[auth]` is a **map of identity names**, so an `[auth.whois]` table
would be indistinguishable from an identity called `whois` — and would become
one the day someone names an agent that. A top-level section costs nothing and
cannot collide. The only deliberate divergence from klams's shape.

## D-8 — The legacy bearer path ships, and is deleted one release later

After the cutover (WI 2393) no host declares a token, so the bearer comparison
is dead code in a deployed binary. It stays for one release anyway.

The reason is cleo: it is edited last, it is the historically-missed host, and
its Claude Desktop config is MSIX-packaged and awkward to edit. Keeping the
bearer path means a missed client is rescued by restoring one config line and
a reload, not by a fleet redeploy. That is a deliberate rescue hatch, not
leftover code.

Deleting it needs a second deploy after the window is observed closed, which
is why it is a filed follow-up rather than a repair in this change: the
trigger is the program overseer confirming cleo's header is in effect from
cleo, which cannot happen before this sprint ships.

## D-9 — `kaed-new-token` is deleted, and `install.sh` removes stale copies

Retiring the script is not enough: a copy left on `PATH` is live-looking
rotation machinery for a credential that no longer exists, and the next agent
to find it will try to use it. `install.sh` deletes
`~/.local/bin/kaed-new-token` if it is there, so the upgrade cleans up after
itself on every host.

The script is also out of the published deploy bundle. Installing a pre-023
version still works — the bundle and its installer are fetched together per
version, so they never mix.
