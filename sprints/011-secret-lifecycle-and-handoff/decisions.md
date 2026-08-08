# Sprint 011 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. PD-2 (BLAKE3 identity) and PD-3 (handle =
> location + digest; the gateway carries values; Ken's accepted risk) were
> taken before this sprint and are implemented, not re-decided, here.

---

## D-1 — `reveal` ships minimal, because the measurement came back empty

008 shipped redaction without `reveal` to measure real pressure for
plaintext. The measurement (2026-08-08, both hosts' journals): zero
classified refusals, zero feedback, zero redaction-shaped friction — the
`set -a; . .env; set +a` hint and placeholder passthrough have covered
every live case so far. Honest caveats: ~1 day of window, and reads are
not journaled, so the channel that would carry read-side friction is
`feedback`, which is silent.

So `reveal` exists — the genuinely irreducible case is a value that must
leave the kaed-writable world (a client config on cleo, a vendor console)
— but at minimum viable width:

- One key per call. No bulk reveal, no whole-file reveal.
- `intent` is **required** — the only kaed tool where it is. A disclosure
  with no recorded reason is the row the audit stream exists to prevent.
- Always journaled as a `secret_events` row with `disclosed: true`.
- The response carries `disclosed: true` and a note instructing the agent
  to surface the disclosure to the human (brainstorm's marker).
- `[secrets] allow_reveal = false` refuses the whole tool with a
  structured reason — for hosts where even the per-tool prompt is too
  much gate (the kubsdb shape). Default `true`: the per-tool boundary is
  the load-bearing gate (that is why `secret_reveal` is its own tool),
  and a deploy is already a deliberate act.

## D-2 — `describe` *is* `load_secret`; the handle has no second store

#1051 sketches `describe(key)` (shape metadata, no value) and #1052
sketches `load_secret(path, key)` (a handle: location + digest). They
return the same facts, so they are one action: `secret` /
`describe` returns `{key, shape, len, digest?, placeholder, handle}`,
where `handle` is the host-qualified location plus digest — PD-3's
reference, ready to paste into a korg handoff. Persistence is the file
itself (R1's rule applied to secrets: a durable content address, not a
session handle). Two names for one read would have been two tools
answering one question — the PD-5 mistake in miniature.

`describe` is not journaled: it discloses the digest and shape, which the
redacted `read` already serves; the audit stream records *disclosure of
values*, not of their fingerprints.

## D-3 — One `secret` tool with an `action` discriminator; only `reveal` is separate

Harness permissioning is per-tool, and the brainstorm's load-bearing
choice is that `reveal` must therefore be its own tool. The same argument
does *not* apply between describe/generate/rotate/occurrences — they are
one trust tier (none ever returns a value), and four more top-level tools
would tax every MCP client's context for no safety gain. So: `secret`
(action: describe | generate | rotate | occurrences) + `secret_reveal`.
The capability flag for the whole surface is `secret_lifecycle`, one
entry in `ROOT_CAPABILITIES` — a peer without it maps to
`unsupported_capability` at call time via the 010 machinery, which is how
a mid-upgrade fleet stays honest.

## D-4 — Shapes are a closed grammar plus named config entries, and rotate re-mints by detection

`generate` accepts either a named entry from `[secrets] shapes`
(project-overridable) or a spec in a **closed grammar**: `hex(N)`,
`base64url(N)`, `uuid4`, `prefixed(tag, inner)`. Nothing free-form — the
brainstorm's "this must not become an eval-shaped feature" rule — and
grammar minimums (hex ≥ 16, base64url ≥ 16 chars) keep kaed from minting
a weak secret on request. The brainstorm's `passphrase(4)` is deliberately
not built: a 4-word passphrase cannot clear the 80-bit floor that PD-2's
whole digest story rests on, and no live use case wants one from an agent.
Wait for demand.

`rotate` stores nothing about how a value was made — persistence lives in
the file (PD-3), so the shape is *detected* from the current value
(hex/uuid/base64url charset + length, provider prefixes). A value whose
shape detection is ambiguous (`text`) refuses rotation unless the call
names a shape explicitly: minting "something like that" on a value kaed
does not understand is exactly the silent-wrong-write R-rules exist to
prevent. Rotation's overwrite is exempt from `drop_keys` — replacing the
value **is the declared intent of the verb**; the old value's destruction
is not a side effect the agent might have missed, and the audit row
records old and new digests.

## D-5 — `value_from`: same-host resolves in the engine, cross-host at the gateway boundary, and the source side journals the exit

`env_set` gains `value_from: {root?, path, key, digest?}`, mutually
exclusive with `value`.

- **Same-host** (root omitted, or resolving to a root this instance
  serves): the txn engine loads the source through `load_text` — every
  policy layer applies, so a denied or opaque source refuses exactly as a
  read would — takes the key's current value, and verifies the digest if
  one was given. The source is not a `base` entry: nothing mutates it,
  and the digest *is* the staleness check when the caller wants one
  (PD-3: current-vs-exact must never be ambiguous — no digest means
  current, a digest means exact-or-fail-loudly).
- **Cross-host**: whichever instance receives the call resolves any
  `value_from` whose root's host differs from the host that will apply
  the edit — before local apply, or before forwarding a proxied `edit`.
  Resolution is a `secret_reveal` call to the source host **under the
  caller's identity** (PD-4), carrying a `transport` context naming the
  destination; the substituted plaintext rides only the forwarded
  request (tailnet TLS, gateway memory — PD-3's accepted risk), is
  zeroized after use, and is never logged or journaled by the gateway
  (D-7 of 010: the gateway journals nothing; the *source* host journals
  the disclosure, the *target* host journals the redacted write).
- The raw-JSON substitution on the proxy path follows 010 D-1: kaed
  rewrites exactly the `value_from` it consumed and forwards everything
  else byte-for-byte, so a newer peer's unknown fields still pass.

A `dry_run` still resolves (the digest check is the point of a dry run),
so a cross-host dry run still journals a disclosure on the source host —
the value really did leave it. Documented, not hidden.

## D-6 — The audit stream records events and claims, never payloads

`secret_events` (metadata clock — kept forever, like txn rows): action
(`generate` | `rotate` | `reveal` | `transport`), author, location,
digests (withheld below the entropy floor, per PD-2), `disclosed`,
claimed `destination`, `txn_id` when the action wrote, redacted `intent`.
Read back through `journal` as `kind: "secret"`; `coverage` gains
`secrets_from` with the same honesty rule as `failures_from` — a window
reaching back past it is silent, not clean.

The honest limit, stated: `destination` on a transport row is the
*caller's claim*. A caller with a valid token could invoke reveal-shaped
disclosure and label it transport; kaed cannot verify where bytes went
after they left. The row's true content is "this value left this host,
when, under whose credential, toward what stated destination" — which is
exactly what "has any agent ever seen the klams token?" needs, and within
the secrets model's honest frame (blast radius, not access control: an
agent with a shell can `cat` the file).

## D-7 — `.env.example` sync is an edit op, one direction, values stubbed empty

`env_sync_example` rides the `edit` envelope (D-9 of 008's precedent:
atomicity, base versions, dry_run, journaling for free). One direction
only — `.env` → example — because the reverse (example → live) has no
values to carry and a merge story nobody asked for. Keys and attached
comments are preserved in order; every value is stubbed empty (`KEY=`),
which the classifier then renders harmlessly. The example path defaults
to `<path>.example` and must be declared in `base` when it already exists
— it is a generated file, but silently clobbering hand edits would break
R2's promise. The vanish guard does not fire on the example (empty values
are not secrets), and the op refuses to *target* the live file.

## D-8 — `occurrences` is single-host and digest-keyed; the fleet version already exists

`occurrences` walks this instance's roots (same policy filters as
`search`), parses classified dotenv files, and reports every entry whose
value digest equals the source's — the feeder for `rotate.also`, with
scanned/hidden tallies per the R7 honesty rules. Below-floor values
refuse: without a disclosed digest, equality is deliberately not
answerable (PD-2), and pretending otherwise would rebuild the oracle D-8
of 008 killed. Fleet-wide occurrence hunting is *not* rebuilt here:
`search {pattern: <digest>, root: "*:*"}` has answered it since 010, and
a second fan-out mechanism would violate PD-5's one-question-one-tool
rule. `rotate.also` may name remote roots; each remote write is a
proxied env_set after the local transaction lands, reported per-target —
cross-host rotation is **not atomic** and the response says which
targets landed, which is PD-3's rotate-both-places path with its honest
failure story.
