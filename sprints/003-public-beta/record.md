# Sprint 003 — public beta readiness

Branch `003-public-beta`, from korg proposal 922 (WIs #918–#921). Ken's
directive right after 002 shipped: make the repo fit to be public on GitHub.

Docs and hygiene only — no behaviour change, so `just check` stayed green
throughout and the risk was close to zero. The interesting work was deciding
what an outside reader is owed.

## Goal

A repo a stranger can land on and, within a minute, know: what this is,
whether it will hurt them, and how it gets built. Plus a deployable setup
guide, because "read the sprint records" is not documentation.

## Cleanup first

Sprint 002 left `~/.config/kaed/env` in place deliberately — its value had
moved to `~/.config/kaed/token`, but deleting it would have made the deploy
destructive for no gain that day. Ken's call was right: things like that sit
forever. Removed the file and the unit's `EnvironmentFile=` line, restarted,
re-verified auth. One token, one copy.

Worth noting the check that nearly went wrong: comparing the two values with
`diff <(grep … env) <(cat token)` reported a **mismatch**, which looked
alarming for about ten seconds. The env file's line ending contributed a
trailing newline that `cat token` did not have. Comparing them as shell
variables (which strip trailing newlines) showed 64 bytes each, identical.
The lesson is small but real: a byte-comparison of a secret is only meaningful
if you control the trailing whitespace on both sides, and "the values differ"
is a scary enough answer to be worth double-checking before acting on it.

## What shipped

### #921 — pre-public hygiene

**The secrets scan found something, and not where grep looks.**

The working tree was clean: no tokens, no key material, no provider-prefixed
credentials, no non-loopback IPs, and every `ts.net` mention already using the
`<tailnet>` placeholder. `git grep` said the repo was fine.

It wasn't. The real tailnet hostname was sitting in a **commit message** on
`origin/001-walking-skeleton`:

```
Sprint 001: deploy to kai + dogfood, deploy notes

Live at https://kai.<the-real-tailnet>.ts.net:4870/mcp — systemd user unit,
```

— in a commit whose *own* message, two commits later, said "Tailnet hostname
kept out of the repo."

Two things made this hard to see, and both are worth remembering as a class:

- **`git grep` searches trees, not messages.** A clean `git grep` on HEAD, or
  even on every branch, proves nothing about what the commit messages say.
- **`git log -S` also misses it.** The pickaxe searches *content changes*, so
  it returned zero hits too. What found it was the blunt
  `git log -p --all | grep`, which happens to include message bodies in its
  output.

The branch was already squash-merged into main and held nothing unique — the
only tree difference against main was main's *newer* roadmap. Ken approved
deleting it, so the commit is unreachable before the repo is ever public, and
main's history was clean all along. No history rewrite needed.

Also landed: `Cargo.toml` metadata (description, license, repository,
keywords, `publish = false` — kaed is a daemon you deploy, not a crate), and
`SECURITY.md`.

**On SECURITY.md's framing.** The temptation with a security doc is to list
features and let the reader infer strength. For a tool whose entire job is
writing to your filesystem over the network, that would be dishonest by
omission. So it leads with the distinction instead: kaed's protections are
blast-radius reduction, *not* an access-control boundary. The deny list stops
kaed from serving your `.ssh` keys; it does not stop the agent holding kaed's
token from reading them, because that agent has a shell and `cat` was never
routed through kaed. Anyone deciding whether to run this deserves that
sentence before the feature list, not after it.

### #918 — README

Three jobs the old one didn't do.

The **risk warning** is a blockquote at the top, above the feature table,
unavoidable: early beta, expect breaking changes to config/contract/schema, do
not expose to an untrusted network, and the safety features are blast-radius
reduction rather than access control. It ends with a sentence that is
deliberately blunt — "If you run this and it eats something you needed, that
is a risk you took." Softer phrasings all read as boilerplate, and boilerplate
is skipped.

The **development process** section is the one Ken specifically asked for, and
it earns its place by explaining the rest of the repo: why sprint records read
as primary sources rather than summaries, why there are `decisions.md` files,
why commits are co-authored, and why the MCP tool descriptions get reviewed as
carefully as the code. Framed honestly as an experiment — an agent given real
ownership of a tool that agents use — because that's what it is.

### #919 — docs/

**`docs/overview.md`** — the why and how for an outside reader. The existing
`sprints/planning/overview.md` is good but written for someone inside this
homelab; it names cleo and kai without introduction and assumes klams/korg
context. The new document keeps the strongest material (the human-editor →
agent-reality table, the staleness and free-verification consequences) and
adds what an outsider needs: the concrete failure modes of the alternatives,
what a version *is* and why content-addressing buys durability for free, the
three deny layers, what's deliberately excluded, and an honest roadmap ending
at the dogfood report — the point being that until that comparison exists,
"kaed is better" is a hypothesis and should be labelled one.

**`docs/setup.md`** — actually deployable by someone who isn't Ken. Build,
token, config with every knob in a reference table, systemd unit, the
`tailscale serve` pattern, client wiring, a rotation runbook, operating notes,
and a troubleshooting table.

Two deliberate emphases:

- **"Choosing roots" gets its own subsection with the story attached.** The
  temptation is a bland "configure your roots" line. But the single most
  consequential decision in the whole setup is which directories an agent can
  reach, and the most persuasive argument against rooting at `$HOME` is that
  this project did exactly that and got its own token read back to it. Telling
  that story costs four lines and is worth more than any amount of "be
  careful".
- **The rmcp `Host`-validation gotcha is called out in a warning block**, with
  the symptom named ("a 4xx that looks nothing like an auth problem"). It cost
  time twice already; it will cost every new deployer an hour otherwise.

The **agent-assisted path** is a prompt a reader can paste to their own agent,
with two constraints built in that come straight from this project's own
mistakes: *don't print the token* (that's how the first one ended up needing
rotation) and *show me `check-config` before trusting the deploy* (the
resolved roots are the entire security boundary and are easy to get wrong in a
way that looks fine). The doc explains why both are there, so a reader can
judge them rather than just copy them.

### #920 — the visual explainer

`docs/kaed-explained.html`, self-contained: no CDN, no external assets, opens
from `file://`. Also published as a private artifact for convenient viewing.

Design notes, since "infographic-ish" is a quality bar rather than a spec:

- **Type.** Three system-stack roles — a serif for argument, a humanist sans
  for prose, mono for anything the machine actually said. No webfont, because
  the CSP-safe options are inlining ~80KB of base64 into a repo file or
  risking a silent fallback, and a deliberate system stack beats both. Mono is
  used *structurally* (eyebrows, wire samples, hostnames, the version hash)
  rather than decoratively.
- **Colour.** Cool blue-biased slate with a signal amber accent. Both the
  terminal-green-on-black and the purple-gradient defaults were avoided on
  purpose; amber reads as *caution and instrumentation*, which is the right
  register for a tool whose main claim is "it tells you what it did."
- **The one flourish** is the version hash in the hero, set large in mono with
  the first eight characters in accent. It is kaed's atom, and the page's
  argument reduces to "this string is why the rest works."
- Real values throughout — the hashes in the loop diagram
  (`060ad669c1a1d257` → `7e6a34a06119ff41`) and the diff are from the actual
  002 live-verification run on kai, not invented.
- Both themes are token-level, with the viewer's toggle overriding the media
  query in both directions; the SVG inherits the same tokens rather than
  hardcoding colours. Motion is one restrained scroll reveal, disabled under
  `prefers-reduced-motion`.

### The secrets brainstorm — committed

Ken passed along `sprints/planning/brainstorm-secrets-editing.md` from another
session, explicitly leaving inclusion to me and asking only for a
commit/don't-commit call.

**Committing it.** It is honestly labelled as undecided, contains nothing
sensitive, and shows the project's thinking in a way that fits a repo whose
README advertises its sprint records as primary sources. A brainstorm that
argues *against* two of its own starting ideas is a good look, not a
liability.

More importantly it contains a real sequencing constraint that would have been
lost in a planning directory: **a redacted read surface is worthless if
`journal.db` holds plaintext and the planned `journal`/`diff`/`revert` tools
serve it.** #909 settled blob retention without secrets-aware editing in view,
so that decision does not cover this case. Left buried, this gets discovered
after the history tools ship. So it is now a blockquote directly under the
history-tools entry in the roadmap, plus a Later/Ideas entry pointing at the
brainstorm.

## Follow-ups

- **A CI workflow** — `just check` on push and PR to main. Deferred, then
  settled during the ship: the integration tests **should** run in CI. The
  concern raised here originally ("they bind a real port") was wrong; they
  bind `127.0.0.1:0`, so the OS assigns a free one and there is nothing to
  collide with. Measured: 115 ms for the suite, 0 failures in 10 consecutive
  runs, loopback and tempdirs only.

  They are also the tests least worth losing. The 102 unit tests cover logic;
  these six cover the wire contract. `rejects_missing_and_bad_tokens` is the
  only thing that would catch an auth layer refactored into the wrong place
  in `build_app` — a change every unit test would happily pass while leaving
  the service open. Splitting the suite would also make `just check` stop
  being *the* gate, which is worse than any CI cost being avoided.

  Still to build: the workflow itself, with a cargo cache (bundled SQLite is
  the only slow part of a cold build).
- **`docs/kaed-explained.html` will drift.** It hardcodes "two shipped
  sprints" and the roadmap's shape. Worth a glance at the end of any sprint
  that changes the status section.

## Pre-public checklist

Everything below is verified in the final section of this sprint's work:

- [x] No secrets in the working tree
- [x] No secrets in git history — **including commit messages**
- [x] Stale branch carrying the tailnet name deleted from the remote
- [x] `LICENSE` present (MIT), `Cargo.toml` metadata complete
- [x] `SECURITY.md` with an honest threat model
- [x] README leads with the risk warning
- [x] `docs/` deployable by a stranger
- [x] `just check` green
- [x] 002 and 003 merged to main — shipped as separate PRs (#2, #3) so the
      branch history tells the sprint story rather than collapsing into one

Flipping repository visibility is the one remaining step, and it is Ken's to
take.
