# Sprint 011 — secret lifecycle and cross-session handoff

*korg proposal 1060; work items #1051 (lifecycle, L) and #1052 (handoff, M).
Started 2026-08-08. Design sources: `../planning/brainstorm-secrets-editing.md`
(slice 3), PD-2 (BLAKE3 identity), PD-3 (handle = location + digest; the
gateway carries values). Depends on 008 (shipped) and 010 (shipped).*

## Goal

The payoff slice of the secrets model. Two halves:

1. **Lifecycle (#1051)** — a `secret` tool (describe / generate / rotate /
   occurrences) over a named shape registry, plus `secret_reveal` as its
   **own tool** so harness per-tool permissioning is the gate. An agent can
   mint, rotate and propagate a token it never sees. `.env` ↔ `.env.example`
   sync and the secrets audit stream ride along.
2. **Handoff (#1052)** — a secret reference is `location + digest`
   (host-qualified root, path, key; BLAKE3 per PD-2), persisted **in the
   file the secret already lives in** — no registry, nothing to expire.
   `describe` returns the handle; an edit op can consume one via
   `value_from`, including across hosts through the gateway, where the value
   transits gateway memory and never the agent's context (PD-3).

## The pre-build gate: 008's reveal-pressure measurement

The proposal requires reading 008's measurement before building `reveal`.
Measured 2026-08-08 against both live journals (kai direct, kubs0 over ssh):

- **kai**: 0 failures of any kind since 008 deployed (2026-08-07); 0
  feedback rows ever; the only `denied` on a `.env` in the journal's life
  is the sprint-002 verification exercise.
- **kubs0**: 0 `denied` ever, 0 feedback ever; post-008 failures are two
  ordinary `version_conflict`s on a README.
- No `classified_opaque` refusals, no redaction-shaped friction, anywhere.

Caveats recorded rather than hidden: the window is ~1 day of live usage,
and reads are not journaled (009 D-2), so read-side friction is
structurally invisible; feedback exists precisely for that and is silent.

**Conclusion (D-1 in decisions.md):** zero observed pressure for plaintext.
`reveal` ships minimal and hard-gated — required `intent`, one key per
call, always journaled as a disclosure event, plus a config kill-switch —
and the lifecycle verbs do the real work of removing the need for it.

## Shape of the change

New modules:

- `src/shapes.rs` — the shape registry: a closed spec grammar
  (`hex(N)`, `base64url(N)`, `uuid4`, `prefixed(tag,inner)`), named
  project-level entries in `[secrets] shapes`, minting via OS entropy, and
  shape *detection* so `rotate` can re-mint like-for-like without storing
  anything.
- `src/secret_tool.rs` — describe / generate / rotate / occurrences /
  reveal, over the existing txn engine (writes are ordinary journaled
  transactions) and the audit stream.

Changed:

- `src/journal.rs` — `secret_events` table (migration on open): every
  generate / rotate / reveal / transport, with `disclosed` and claimed
  `destination`. Metadata-clock retention (kept forever), like txns.
- `src/history.rs` — `journal` gains `kind: "secret"`; coverage gains
  `secrets_from`.
- `src/txn.rs` — `env_set` gains `value_from` (location + optional digest);
  same-root resolution in the engine; `env_sync_example` op.
- `src/server.rs` — the two new tools; cross-host `value_from` resolution
  at the gateway boundary (fetch via source host's `secret_reveal` under
  the caller's identity, substitute in memory, zeroize after use);
  capabilities gain `secret_lifecycle`.
- `src/config.rs` — `[secrets]` section: `shapes`, `allow_reveal`.

## What shipped

Both work items, one branch, `just check` green throughout.

**#1051 — the lifecycle.** `secret` (action: describe / generate / rotate /
occurrences) and `secret_reveal`, wired per D-3: one allowlistable tool
that never discloses, one escape hatch that keeps prompting. The shape
registry is a closed grammar (`src/shapes.rs`: `hex(N)`, `base64url(N)`,
`uuid4`, `prefixed(tag,inner)`; minimums sized so every mintable value
clears the PD-2 floor; `passphrase` deliberately absent) plus named
`[secrets] shapes` config entries, validated at startup and printed by
`check-config`. `generate` mints new keys only; `rotate` detects the
current value's shape (conservatively — provider tokens refuse) and
re-mints, writing the primary and same-root `also` targets in ONE
transaction, remote targets as proxied writes with per-target outcomes.
`occurrences` scans this host by digest over the same redacted surface
`search` uses. `env_sync_example` rides the edit envelope (D-7), comments
run through the shape detector on the way out, and an all-empty example
is exempt from the gitignore warning. The audit stream is
`journal.secret_events` — read back as `journal` kind `"secret"`, with
`coverage.secrets_from` dating its beginning.

**#1052 — the handoff.** `describe` returns the handle (D-2) —
`{root, path, key, digest}` plus a paste-able `handle_line` — and
`env_set.value_from` consumes one: same root in the engine (evolving
buffers respected), other roots and hosts resolved by the receiving
instance before the transaction (D-5). Cross-host, the value moves
kaed-to-kaed under the caller's identity — the source host journals a
`transport` event with the claimed destination (D-6), the target journals
a redacted write, the gateway journals nothing (010 D-7), and the held
buffer is `Zeroizing`. Digest mismatch fails loudly everywhere (PD-3):
engine, server fetch, reveal, and placeholder passthrough all share the
rule.

Tests: 44 new (shapes 8, secret_tool 16, txn value_from/sync 9, plus the
http.rs full-lifecycle wire test and three gateway tests covering
local→remote transport, remote→local transport with the exit journaled on
the peer, and rotate-both-hosts). Contract gained R11 and the two tool
sections; capabilities gained `secret_lifecycle`; `deploy/
config.example.toml`, `docs/setup.md` and `CLAUDE.md` updated.

## Follow-ups

- Write-side leak detection (#1053, next slice) gets the shape registry and
  the audit stream for free from this sprint.
- Fleet-wide `occurrences` fan-out deliberately not built: `search` with the
  digest and a root pattern (`*:*`) already answers it since 010; the typed
  single-host `occurrences` feeds `rotate.also`.
- The korg `handoff` skill should carry `describe`'s handle line, never a
  value (PD-3 corollary) — a change to that skill, not this repo.
