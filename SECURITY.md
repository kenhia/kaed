# Security

kaed is a network service that reads and writes files on the machine it runs
on, authenticated by a bearer token. Please read this before deploying it
anywhere that matters.

**Status: early beta.** It is dogfooded daily on one host by its author. It
has not been audited, pen-tested, or run by anyone else in anger.

## Threat model, honestly

kaed's protections are **blast-radius reduction and ergonomics, not an
access-control boundary.**

That distinction is the whole thing, so to be concrete: the deny list stops
kaed from serving your `.ssh` keys. It does *not* stop the agent holding
kaed's token from reading them, because that agent almost certainly also has
a shell, and `cat ~/.ssh/id_ed25519` was never routed through kaed. What the
deny list buys is that the *well-intentioned* path is safe — an agent
grepping a repo doesn't hoover up credentials by accident, and a careless
edit can't rewrite your authorized_keys.

If your agent is actively hostile, or is executing instructions injected by
content it read, kaed is not what stands between it and your filesystem.
Nothing in kaed is designed on the assumption that it is.

### What kaed does defend against

- **Accidental disclosure through the tool.** Reads, searches, listings and
  edits are all refused for denied paths, and denied entries never appear in
  enumerations. A repo can extend the denials with a gitignore-shaped
  `.kaedignore` (readable through kaed, never writable through it), and a
  file can opt itself out with a `# kaedignore` comment in its first lines.
- **Secrets passing through the agent.** Secret-bearing files (`.env` and
  friends) are *classified* rather than denied: reads come back redacted,
  with each value replaced by a sealed placeholder, and edits go through
  typed operations where a placeholder writes the real value back — so the
  common flows (add a key, rename it, copy it, reorder the file) never put
  plaintext in the agent's context. The redaction extends to every derived
  surface: diffs, conflict deltas, search hits (which run over the redacted
  text, so probing for a value by searching finds nothing), and journal
  blobs. There is no reveal operation.
- **Accidental secret destruction.** A write that would destroy a value the
  agent never saw is refused unless the edit explicitly declares it.
- **kaed serving its own credentials.** Its config and journal directories
  are refused unconditionally — no configuration can turn that off.
- **Escaping the configured roots.** Absolute paths and `..` are rejected;
  symlinks are resolved and the result must still be inside a root.
- **Silent corruption.** Every mutation declares the version of each file it
  touches. A stale base fails with a structured conflict, never a wrong
  edit. Multi-file edits are atomic.
- **Unattributed change.** Every applied transaction — and every failed
  attempt — is journaled with the identity that made it.

### What it does not

- **A compromised or injected agent.** See above.
- **A stolen token.** Tokens are bearer credentials with **no expiry**. A
  leaked token is valid until you rotate it. (Rotation is non-breaking; see
  [docs/setup.md](docs/setup.md).)
- **An untrusted network.** kaed binds loopback by default and expects to be
  fronted by something that provides transport security and network-level
  access control — the reference deployment uses `tailscale serve`. Do not
  put it on a public interface.
- **A hostile local user.** Anyone who can read `~/.config/kaed/token` is
  kaed, as far as kaed is concerned.
- **A compromised gateway host.** An instance configured to proxy to peers
  (`[peers.<host>.tokens]`) holds bearer tokens **valid on those peers**, so
  compromising the gateway machine yields credentials for every backend it
  routes to — one machine's compromise is the fleet's, for the identities
  configured there. That is a deliberate trade (the alternative was a shared
  gateway identity, which destroys journal attribution). If that radius is
  unacceptable, don't configure peer tokens: every host remains directly
  reachable with per-host credentials, and routing simply refuses.
- **Your journal.** `journal.db` stores the content of files kaed has
  edited, so it is as sensitive as the most sensitive file kaed is allowed
  to touch. It is created `0600` and its blob content ages out on a
  configurable retention (default 7 days). Classified files are journaled
  as their *redacted* renderings (or withheld entirely when kaed cannot
  redact them), so secrets edited through the typed operations do not land
  in it — but plaintext of every *unclassified* file kaed edits still does,
  and journals written before classification existed may hold plaintext of
  files that would be classified today. The strongest control over what
  ends up in it remains the deny and classify lists.

## Deployment expectations

1. Bind to loopback (the default) and front it with something that
   terminates TLS and restricts who can reach it.
2. Use narrow, explicit roots. Do not root at `$HOME` — that is how kaed
   ended up serving its own token during its first live test, and it is why
   the deny list exists at all.
3. Keep the token file `0600`, and never commit it. The same goes for any
   peer token files a gateway holds — and give them the same weight as the
   backends they unlock, not the machine they sit on.
4. Assume `journal.db` is sensitive; back it up accordingly or not at all.

## Reporting a vulnerability

Open a GitHub issue at <https://github.com/kenhia/kaed/issues>. If you would
rather not discuss it publicly, open an issue saying only that you have
something to report and asking for a private channel.

This is a personal project with no SLA. Expect a best-effort response, and
please do not assume a fix is coming on any particular timeline.
