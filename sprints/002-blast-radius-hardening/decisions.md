# Sprint 002 decisions

The two decision-shaped items in this sprint. Both were raised by the
2026-08-02 live test; both are the kind of call that is cheap now and
expensive after the fleet deploy.

---

## D1 — journal blobs and secret retention (korg #909)

### The question

`blobs` holds complete pre- and post-images of every file a transaction
touches. Editing any file containing a credential copies that credential
into `journal.db`, where it outlives the edit and is not protected by
whatever protected the original. The live test also showed the journal
ingesting a pre-image kaed never authored (version `0dd410aa481de049`, an
out-of-band ssh edit) — so third-party content lands there too.

### Decision

**Blobs stay whole-content.** The deny list is the control. Retention
becomes real, and shorter.

Concretely:

1. **Whole content is kept, not diffs.** Storing diffs against a
   content-addressed base was on the table and is rejected *as a security
   measure*: a diff of a line containing a secret contains the secret. It
   is a size optimisation wearing a safety costume. Meanwhile whole content
   is what makes the conflict delta work today (the live test used it) and
   what `diff`/`revert` will need. Paying a real feature cost for an
   illusory safety gain is the wrong trade.

2. **The deny list (#908) is the primary control, and it covers journaling
   for free.** A denied file cannot be read and cannot be edited, so it can
   never be blobbed. This is why #908 was sequenced first: it does not just
   reduce #909's surface, it *is* #909's mechanism. No separate
   journal-side denylist exists, and adding one would be a second thing to
   keep in sync with the first.

3. **`journal.retention_days` now means blob retention, defaults to 7, and
   is actually enforced.** Sprint 001 shipped this knob with GC deferred —
   a retention setting that retains forever is worse than none, because it
   reads as a guarantee. GC now runs when the journal opens and at most
   hourly thereafter. The default drops 30 → 7: content is a convenience
   for recent conflicts, and a week covers every conflict window that has
   ever mattered here.

4. **Transaction metadata is kept indefinitely.** Applying retention to it
   was a mistake in the 001 design. The metadata — who, what, when,
   diffstat, intent, git HEAD — is the audit trail, is tiny, and is the
   thing you most want when asking "what touched this file three months
   ago". Only the content ages out.

5. **`journal.db` is created 0600**, explicitly rather than by umask
   accident, and documented as being **as sensitive as the most sensitive
   file kaed is allowed to edit**. That sentence is the operator-facing
   contract; the deny list is what keeps its right-hand side small.

### What is knowingly not fixed

Third-party pre-images still get ingested. kaed cannot know the provenance
of bytes that were already on disk when it looked, and that pre-image is
exactly what makes the conflict delta useful. It is covered by the same two
controls as everything else — deny list, then retention — and that is the
whole of the answer.

---

## D2 — the token-rotation story (korg #914)

### The question

Rotating a token today is a hard cut: every live session breaks until each
client restarts with the new value. Fine with one client; a coordination
problem across claude + ghcp on cleo/kai/kubs0. Decide before the fleet
deploy.

### Decision

**Implement both halves, in the order the dependency dictates: reload
first, grace window second.** They were written as two bullets on the WI;
they are not a menu.

The review comment on #914 established the fact that forces the order:
kaed reads token env vars **only at startup** (evidenced by
`ExecMainStartTimestamp` matching the env file's mtime to the second at the
2026-08-02 rotation — the new token working afterwards was proof of the
restart, not of a per-request read). So:

- A dual-token grace window **alone buys nothing**. You still restart the
  process to load either value, and the restart is what kills live
  sessions — a fact about the process lifecycle, not about tokens.
- Reload-without-restart is therefore the load-bearing fix.
- The grace window is what makes reload *safe*: it covers the clients that
  have not yet restarted to pick up the new secret. Reload without a grace
  window just moves the hard cut to a different instant.

**The consequence that shapes the config surface:** a process cannot
re-read its own `EnvironmentFile` — systemd injected those vars at exec and
they are frozen for the process's life. Reload therefore requires tokens to
be readable *at reload time*, which means from a **file**. So `[auth]`
grows `token_file` alongside `token_env`, and `prev_token_file` for the
grace window. `token_env` keeps working unchanged (the live kai deploy uses
it) but cannot participate in reload — that limitation is inherent, and is
documented rather than papered over.

Reload trigger: **SIGHUP**. It is the convention, it needs no new surface
area, and `systemctl --user reload kaed` maps onto it with one unit line.

### What stays broken on purpose

The client-side half. Claude Code and Desktop Claude load MCP config only
at session start, so a client learns a new secret by restarting. No server
change fixes that, and pretending otherwise would be worse than saying it.
What the grace window buys is that the restart can happen *whenever the
client next restarts anyway*, instead of immediately and in lockstep.

### Split out, not solved here

Auth failures are rejected before the transaction layer, so #910 will never
make them visible in the journal no matter how thorough it is. "Did every
client pick up the new token?" — the actual question during a fleet
rotation — needs its own counter at the auth layer. This sprint logs
grace-token use at `warn` with the identity named, which answers the
question during a rotation window and costs nothing; a real counter is a
follow-up.
