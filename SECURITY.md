# Security

kaed is a network service that reads and writes files on the machine it runs
on. Callers **declare an identity** — a name on an allow-list, sent as
`X-Homelab-Agent` — and the network perimeter is what keeps strangers out.
Please read this before deploying it anywhere that matters, because that
sentence is doing real work.

**Status: early beta.** It is dogfooded daily on one host by its author. It
has not been audited, pen-tested, or run by anyone else in anger.

## Threat model, honestly

kaed's protections are **blast-radius reduction and ergonomics, not an
access-control boundary.** Its identity model says the same thing out loud:
**a declared name is attribution, not authentication.** Anything that can
reach kaed's port can claim any name on the allow-list. What the name buys
is that every edit is recorded against the agent that made it, and that a
name nobody configured is refused rather than served anonymously.

That is a deliberate change (sprint 023), not an omission. kaed previously
issued a bearer token per identity, which read like authentication and was
not: the tokens had no expiry, lived in plaintext client configs on every
machine that talked to kaed, and were held *by the very agents* they were
meant to distinguish — agents that also had a shell on the same host. They
bought attribution and nothing else, at the cost of a credential to mint,
copy, inventory and rotate for every (agent × host) pair. Now the perimeter
does the access control it was always actually doing, and the name does the
attribution it was always actually doing.

**So the perimeter is load-bearing.** See "An untrusted network" below; it
is no longer a secondary control.

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
  blobs. The whole lifecycle — generate, rotate, locate every copy, copy a
  value to another file or another host — runs without disclosure: kaed
  mints and moves values server-side and hands back placeholders.

  There **is** one reveal operation, `secret_reveal`, added after shipping
  without one for a sprint to see how much pressure for it materialised.
  It is deliberately its own tool, because harness permissioning is
  per-tool and that split is the actual gate: one key per call, a required
  `intent`, always journaled to the secrets audit stream, and refusable
  host-wide with `[secrets] allow_reveal = false`.
- **Secrets written *into* files that would not redact them.** The
  higher-frequency real incident is a token pasted into a README, a
  fixture or a doc, so writes to *unclassified* files are scanned for
  newly-introduced secrets. Content matching a known secret's digest, a
  provider token prefix or a private-key block is refused, naming the
  explicit override to pass if the write is deliberate; merely
  high-entropy content warns and applies. The honest limit: the precise
  tier covers secrets kaed has *seen*, not every secret on the host.
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
- **Anyone who can reach the port.** There is no secret to steal, and no
  secret to check: a caller names itself and kaed believes it. Revoking an
  identity means deleting it from `[auth]` and restarting; keeping a
  stranger out means the network layer. This is the trade the identity
  model makes explicitly.
- **An untrusted network.** **This is the access-control boundary.** kaed
  binds loopback by default and expects to be fronted by something that
  provides transport security *and* network-level access control — the
  reference deployment uses `tailscale serve`, which keeps the port on a
  private tailnet. Do not put it on a public interface. Do not run it
  anywhere the set of hosts that can reach it is not the set of hosts you
  would hand a shell to.
- **A spoofed identity from inside the perimeter.** kaed records the
  caller's tailnet node (`tailscale whois`) beside the declared name on
  every journaled mutation, so a name claimed from an unexpected machine is
  *visible afterwards*. It is recorded, not enforced, unless you pin
  identities to nodes (`nodes = [...]` plus `[whois] enforce`), which is off
  by default. Even pinned, this is a tailnet-address check, not a
  cryptographic one.
- **A compromised gateway host.** An instance that proxies to peers
  forwards the **caller's own declared name**, so compromising the gateway
  lets it claim any name its backends allow-list — but it yields no
  credential, because there is none to steal. Before sprint 023 the gateway
  held a bearer token per (author, backend) pair and its compromise handed
  over all of them; that radius is gone, and what remains is the radius of
  being inside the perimeter at all.
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

  It also holds an index of **digests** of secrets kaed has seen, which is
  what lets a write be recognised as leaking one. Digests only, never
  values, and only for values above an entropy floor — precisely because a
  digest of a low-entropy value is guessable and a digest of a
  high-entropy one is not.

## Deployment expectations

1. Bind to loopback (the default) and front it with something that
   terminates TLS and restricts who can reach it.
2. Use narrow, explicit roots. Do not root at `$HOME` — that is how kaed
   ended up serving its own token during its first live test, and it is why
   the deny list exists at all.
3. Treat reachability as the credential. The list of hosts that can open a
   connection to kaed is the list of hosts that can edit the files it
   serves, under any name in `[auth]`. Audit that list the way you would
   once have audited a token.
4. Assume `journal.db` is sensitive; back it up accordingly or not at all.

## Reporting a vulnerability

Open a GitHub issue at <https://github.com/kenhia/kaed/issues>. If you would
rather not discuss it publicly, open an issue saying only that you have
something to report and asking for a private channel.

This is a personal project with no SLA. Expect a best-effort response, and
please do not assume a fix is coming on any particular timeline.
