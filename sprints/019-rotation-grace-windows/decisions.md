# Sprint 019 — decisions

## D-1: fix the eight, then make the ninth impossible to add quietly

The literal ask was "add `prev_token_file` to eight `[auth]` entries". Doing
only that would leave the mechanism that produced the gap intact: three
sprints added identities without a grace window, and the state was invisible
until someone read three config files on three hosts.

So the config change is one of four, and the other three are the durable
part:

- `config.example.toml` ships `prev_token_file` on the default identity and
  on the commented per-machine authors, so a new host starts covered.
- `kaed-new-token --rotate` **refuses** when none is configured (D-4).
- kaed names the uncovered identities in a `WARN` at startup, so the fleet's
  coverage is legible from the host instead of by audit.

Any one of these alone still allows the next identity to be added bare.

## D-2: `prev_token_file` is not defaulted to `<token_file>.prev`

Tempting — universal grace windows with no config at all — and rejected.

The file's *existence* is what makes a token valid. Defaulting the path
means any file that happens to be at `token.prev` authenticates, including
one left behind by a rotation someone abandoned halfway a year ago. Nobody
declared it, nothing points at it, and it is a fully live credential. Today
that file is inert unless an operator deliberately named it, and that is the
security property, not an ergonomic gap.

Declaring the slot is one line and it is now in the template. The cost of
being explicit lands on the person adding an identity; the cost of the
default would land on whoever inherits a stale file.

## D-3: the startup warning is in `resolve()`, not `resolve_identities()`

`resolve_identities` runs at startup *and* on every SIGHUP.
`identities_without_grace_window` is called only from `resolve()`, which
runs once.

Whether an identity has a grace window is a property of the config *shape*,
and SIGHUP does not re-read the config — that is exactly why adding one is a
restart (`docs/setup.md`, "What reloads, and what needs a restart"). A
warning repeated on every reload would be reporting something no reload can
have changed, and the operator who just reloaded would reasonably read it as
"my change didn't take".

`token_env` identities are excluded from the warning rather than overlooked:
`resolve` already refuses `prev_token_file` without `token_file`, so they
cannot have a window at all, and their rotation needs a restart anyway
(#914).

## D-4: `--rotate` refuses, rather than warning harder

The status quo already warned. `--rotate` printed a clear paragraph telling
you to set `prev_token_file` — *after* rotating, which is after the damage —
and the fleet still ended up at one of nine. A message that has been ignored
eight times out of nine is not a message that needs better wording.

`--force` is the escape, and it is the honest one: sometimes cutting every
session off immediately is precisely what you want (a leaked token). The
refusal is about the *accidental* hard cut, which was the whole finding.

## D-5: `--force` writes no `.prev` file

The old code path copied the current token to `token.prev` unconditionally.
Under `--force` there is no configured window, so that file would authenticate
nobody — while looking exactly like a live credential sitting at mode 0600
next to a real one. Skipping the copy keeps "a `.prev` file exists" meaning
"a window is open".

## D-6: `--identity` reads paths from `config.toml`; it never derives them

A convention (`--identity NAME` → `token-NAME`) would need a special case on
day one, because the original `claude` identity's file is plain `token` with
no suffix, on all three hosts. Worse, a wrong guess doesn't fail — it mints a
valid-looking 48-character credential at a path nothing references, which
authenticates nothing and is indistinguishable by inspection from one that
works.

So the script parses the `[auth]` table for the identity's own `token_file`
and `prev_token_file`. The parser is deliberately literal about the one-line
inline-table shape the template ships; anything it cannot read (an
`[auth.name]` subtable, a multi-line entry) reads as *absent* and becomes a
refusal, never a fallback. A credential minter that guesses is worse than one
that stops.

The no-`--identity` path resolves the same way, by finding which entry owns
`$CONFIG_DIR/token`, so the legacy naming needs no special case anywhere. And
when it genuinely cannot tell, it says which kind of cannot-tell it hit —
there is no config at all (the normal first-install order, which must stay a
warning) versus a config that names no entry for this path. Those were one
message until the parser was run against kai's real config, where the wrong
one appeared.

## D-7: peer tokens still get no grace slot

`[peers.<host>.tokens]` remains `token_env` / `token_file` only. A grace
window is a *server-side* affordance — it means "I will still accept the old
value" — and a peer token is the client half of the exchange, where there is
nothing to accept.

What the gateway needs instead is an *order*, and the backend's window is
what supplies it: rotate on the backend, copy the new value into kai's
peer-token file, reload kai, close the window on the backend. Without the
window both orderings 401 in the middle, which is why the six
gateway-consumed credentials were the sharpest part of the finding.
`docs/setup.md` now spells that sequence out.

## Test note: not SIGHUPing the live daemon

`tests/new_token.rs` runs the real script, and the script reloads kaed if it
is running — which on kai it is. The tests point `XDG_RUNTIME_DIR` at a
nonexistent path so `systemctl --user is-active` fails and the script takes
its "kaed is not running" branch. That keeps the isolation in the test rather
than adding a test-only escape hatch to a deploy script.
