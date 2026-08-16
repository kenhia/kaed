# Sprint 018 decisions

Per-sprint `D-n`. Cross-sprint decisions live in
`sprints/planning/decisions.md` as `PD-n` — this sprint adds **PD-7**.

---

## D-1 — the identity is the host, not the harness and not the human

**Considered:** reuse the existing `claude` identity for all three clients
(zero work, zero new credentials); or identify by *harness* (`claude-code`
vs `claude-desktop`); or by human (`ken`), since one person drives all of
them.

**Decided:** one identity per **machine** — `claude-kai`, `claude-kubs0`,
with cleo keeping bare `claude`.

Reusing `claude` was the tempting one, and it is the one that quietly
destroys the product. kaed's whole proposition is *verified, attributed*
writes. An attribution that resolves to "some Claude somewhere" is not an
attribution; the journal stops being able to answer "which machine changed
this", which is the first question anyone asks of a cross-host edit. It
also collapses revocation: one compromised client means rotating the
credential that all three share, so the blast radius of any single client
is the whole fleet.

Harness-shaped identity (`claude-code`) fails the same test for the same
reason — two Claude Code sessions on different machines would be
indistinguishable, and *machine* is the axis that actually varies here.
Human-shaped identity fails worse: there is one human, so it carries no
information at all.

The machine is the right grain because it is what the credential can
actually be bound to. A token lives in a file on a host; that is the thing
that can be stolen, and that is the thing that can be revoked. Naming the
identity after the machine makes the credential's scope and its name the
same fact.

Cleo keeping bare `claude` is not an inconsistency worth fixing today: it
is a live credential in a client config on a Windows box whose config
rewrite once wiped every MCP server on the machine (korg #931). Renaming it
buys tidiness and risks that. It can be renamed the next time it is rotated
for a real reason.

## D-2 — six token files, not two, because the gateway proxies *as the caller*

The naive reading of "two new identities" is two new tokens. It is six, and
the reason is PD-4: kai does not hold one credential per backend, it holds
one credential **per (author, backend) pair**, because a proxied edit must
journal on the backend under the same author a direct call would.

So each new author needs: a token to authenticate *to kai*, plus a token
for kai to present *to kubs0*, plus one for *kubsdb*. Two authors × three
endpoints = six.

Two consequences that are easy to get wrong:

- **The backends must know authors that never dial them.** kubs0's
  `[auth]` lists `claude-kubs0` even though kubs0's client connects to kai,
  never to its own kaed. The identity arrives proxied. This looks redundant
  in the file and is not.
- **kai's `[auth]` must list every author it proxies for.** `config.rs`
  enforces this at startup: a `[peers.x.tokens]` entry for an author absent
  from `[auth]` is a hard startup failure, with the reasoning that nobody
  can authenticate as an identity this host does not know, so the token is
  dead weight that looks like working config. Correct, and it caught
  nothing here only because the ordering was known in advance.

**Considered and rejected:** a single shared "gateway token" that kai
presents to backends for every caller. That is precisely what PD-4 refused
in sprint 010, and re-adopting it here would have unwound that decision by
the back door — the backends' journals would have said `kai-gateway` for
every edit, which is the attribution collapse of D-1 with an extra hop.

## D-3 — adding an identity is a RESTART, not a SIGHUP

Documented here because the setup doc's rotation section trains the
opposite reflex, and the failure is silent-ish: SIGHUP succeeds, the new
identity does not appear, and the client gets a 401 that says the token
matches no configured identity — which reads as "wrong token", not "the
server never loaded your identity".

`AuthState` captures `spec` (the `[auth]` table) and `peers_spec` at
startup. `reload()` re-reads the token **files** those specs point at; it
does not re-read the config. So:

| Change | SIGHUP enough? |
|---|---|
| new value in an existing token file | yes |
| new `[auth]` identity | **no — restart** |
| new `[peers.x.tokens]` entry | **no — restart** |
| new peer / changed peer url | no — restart (already documented) |

This is the right trade, not a bug to fix: making `[auth]` hot-reloadable
means re-reading and re-validating config on a signal, and a config that
fails validation on reload leaves the daemon in a state with no good
answer. A restart drops live sessions, which is the cost, and it is paid
once per identity added rather than once per rotation — rotation being the
frequent operation is exactly why *it* is the one that got the hot path.

## D-4 — the gate had to be a real Claude Code session, and the raw probe was not enough on its own

CLAUDE.md's standing rule from sprint 016 is that a green rmcp-client test
says nothing about `2026-07-28` behaviour. The mirror of that rule bit here
and is worth stating as its own decision: **a green raw-JSON-RPC probe says
nothing about the client half.**

The raw probe (`probe.py`) is the precise instrument. It proved 12 tools
with correct SEP-2549 metadata, the four-way edit matrix, per-author
journal attribution, and no double-journaling on the gateway — all things a
Claude Code session reports too vaguely to be evidence.

But sprint 015's actual failure was *Claude Code connected and registered
zero tools*, and `claude mcp list` called that connection healthy. So
"connected" is not the gate and neither is a hand-driven handshake; the
gate is a real session on each host that lists the tools and then uses one.
Both were run headless via `claude -p --allowedTools`, which starts a
genuinely fresh session and therefore genuinely reloads MCP config — the
thing the WI insisted on, achieved without needing a human to restart
anything.

Run both. They fail differently, and neither failure is visible from the
other.
