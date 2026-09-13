# Sprint 024 — decisions

Per-sprint `D-n`. Cross-sprint decisions are `PD-n` in
`sprints/planning/decisions.md`; this sprint adds no PD but appends a dated
**addendum to PD-7** (the identity grain, now tested by a second agent).

## D-1 — Delete the retired config fields, and make the refusal name them

WI 2471 asked for the token fields to go from the structs. 023 D-2 had
deliberately kept them *parsed*, for a reason that has not changed:
`AuthEntry` and `PeerConfig` carry `#[serde(deny_unknown_fields)]`, and
`install.sh` never rewrites a config — so deleting a field turns any host
whose `config.toml` still names one into a daemon that will not start.

**Decided: delete them anyway, and keep that failure — it is the correct
one.** A surviving credential row is exactly the thing this sprint exists to
remove, and a binary that quietly tolerates one cannot tell you it is there.
Refusing to start is the only outcome that cannot be missed.

What was *not* acceptable is diagnosing it from serde's bare
`unknown field 'token_file'`. So `Config::load` scans the raw text first
(`retired_fields`) and bails with the field, the sprint and the fix. The
precondition was verified on the day rather than inherited from 023's
handoff: `kaed check-config` on kai, kubs0 and kubsdb each reported
identity-only auth, no identity annotated as still accepting a bearer, and no
surviving peer-token rows.

Two things about the scan that are load-bearing rather than incidental:

- **It strips comments before matching.** The fleet's own configs and
  `deploy/config.example.toml` carry prose *about* the cutover, and refusing
  to start over a comment would be the guard failing the hosts it exists to
  protect. Pinned by test, and by `check-config` on the example config.
- **It matches longest-first and consumes the match**, because
  `prev_token_file` contains `token_file` as a substring. The naive version
  reported both, which would send an operator hunting for a field the file
  never named. That was a real bug in the first draft, caught by its own
  test.

It is a text scan and not a permissive parse on purpose: a second
deserialization shape for fields that no longer exist would be the very thing
being deleted, kept alive to describe its own absence.

## D-2 — Read the `Authorization` header for the DIAGNOSTIC, never to authenticate

The obvious deletion removes every mention of `Authorization` from the
middleware. That would have been a mistake, and WI 2490 is the evidence:
three GitHub Copilot configs sat sending a bearer and getting `401` for a
full day after 023's cutover, and nothing said why. The failure was silent
because a client sending a retired credential is indistinguishable, from the
outside, from a client sending nothing.

**Decided: keep reading the header, use it only to shape the 401.** The
middleware resolves identity from the declared name alone — `had_bearer` is a
`bool` that never reaches the allow-list. What it buys is a third 401 case:
an `Authorization` header with **no** name now gets a body naming the header
to send and the one to drop. The likeliest failure after this ships is a
client still on a token, and this makes that self-diagnosing.

The challenge still names the `Bearer` scheme, which is worth stating
plainly: nothing accepts a bearer, but a 401 MUST carry `WWW-Authenticate`
(RFC 9110 §15.5.2) and no registered scheme describes "declare a name in a
header". The scheme is the envelope; `error_description` carries the truth.

## D-3 — Copilot gets per-host identities, and klams's bare `ghcp` is not a conflict

The overseer recommended `ghcp-kai` / `ghcp-kubs0` from sprint 018's
reasoning. Taken — but the reasoning was re-derived from measurement, and it
changed what the decision rests on.

**Measured on the fleet**, not assumed: Claude Code declares bare `claude` to
klams from *both* kai and kubs0, and `claude-kai` / `claude-kubs0` to kaed.
So the two services already disagree about grain, deliberately: klams's
roster is per-grant and names the **application**; kaed's question is which
machine edited a file, so it names the **machine**. klams having already
minted a single bare `ghcp` is therefore the same pattern, not a divergence
to reconcile.

The part worth keeping is why 023's node field does not make the per-host
name redundant. kubs0's and cleo's Copilot entries both point at **kai's
gateway**, and a proxied call resolves to the *gateway's* node (023 D-5) —
verified live here: a write from kubs0 journaled `author = ghcp-kubs0,
node = kai`. Under a shared bare `ghcp`, that row and one written from cleo
would be identical. The author name is the only thing that distinguishes
them, which is precisely PD-7's argument surviving into the declared-identity
era.

cleo gets `ghcp-cleo` rather than inheriting the bare-`claude` exception:
that name is historical, and minting a *new* bare one would propagate the
wart instead of the rule.

Consequence for korg:2470 (rendering both client files from k-homelab): it
needs a per **(service, host)** identity value, not one identity per host.
Recorded as a dated addendum on PD-7 so the next client wiring finds it.

## D-4 — SIGHUP stays, and now changes nothing

`reload()` existed to re-read token files. With the files gone it re-resolves
the allow-list from a spec captured at startup (018 D-3) — which is to say it
produces the same answer every time.

**Decided: keep it.** `SIGHUP` is a documented signal and `ExecReload` is in
an *installed* unit file; removing it silently would be a contract change
dressed as a cleanup, and a host that lost it would gain nothing. The docs
now say plainly that every `[auth]`, `[whois]` and `[peers]` change is a
restart, and the test pins the half that matters: a reload must neither drop
an identity nor invent one.
