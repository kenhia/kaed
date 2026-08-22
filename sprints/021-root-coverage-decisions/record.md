# Sprint 021 — root coverage decisions

*korg proposal 1561, covering #1560. Branch `021-root-coverage-decisions`.
Started 2026-08-22. Slice 1 of korg program 1563; slice 2 is agent-skills
#1559 (proposal 1562), which documents whatever this concludes and must not
start before it.*

## Goal

Answer two questions about kaed's root coverage, either way, and write the
answers down where they will be found:

- **Q1** — should kubs0 have a `scratch` root? kai has one; kubs0 does not.
- **Q2** — is `~/.config/systemd/user/` in scope at all?

No code. The deliverable is two recorded decisions plus, if Q1 is yes, a
manifest change in k-homelab.

## Why this sprint exists

korg k-homelab WI-1503, 2026-08-22: a long copyparty session on cleo authored
and edited ~12 files across kai and kubs0 — `run.sh` on two hosts through four
revisions, five theme files, two systemd units — and made **zero kaed calls**.
The cost was concrete: a heredoc that broke on apostrophes, a `sed -i` that
silently did not apply, three md5 round-trips spent proving writes landed.

Roughly half those paths were out of root. The other half were not, and that
is the part worth the sprint. The work was **symmetric across two hosts**, and
kaed covered it on only one — so the agent took the single route that covered
both, for the whole task, including the half kaed did cover.

## Q1 — yes, `kubs0:scratch`. See [PD-8](../planning/decisions.md).

The interesting part is not the answer, it is that sprint 004's D1 already
answered it the other way and gave a reason:

> **No `scratch` root on kubs0**: there is no scratch directory there, and
> inventing one to match kai would be symmetry for its own sake.

Correct when written. Falsified since: `/home/ken/scratch` exists on kubs0
(`ken:ken 0775`, 2026-08-20), created by WI-1503 itself. So this is not a
decision being overturned on taste, it is one whose premise expired — which
is a much better reason, and worth distinguishing in the record so the next
person does not read it as 004 having been wrong.

Two findings that came out of checking rather than assuming:

- **`kai:scratch` is used, and used for one thing.** All four journalled
  transactions are fleet verification — the 011 secret smoke test, the 012
  leak-detection refusal, the 018 cross-host live test, the 020 rotate smoke
  test. "scratch + dogfood area" is exactly right; it is where a deploy proves
  itself against something that is not a real checkout.
- **kubs0 already worked around not having one.** The 018 live test wrote
  `.kaed-018-from-kubs0.md` into `kai:scratch` as `claude-kubs0`. Right thing
  for that test, but it is also the only writable non-repo target kubs0 can
  address — without this root, a kubs0-local smoke write lands in `kubs0:src`
  or `kubs0:k-homelab`.

**kubsdb deliberately still gets none** — it has no `~/scratch`, so 004's
reasoning holds there verbatim. That keeps the rule principled rather than
symmetric: root a scratch directory where one exists and is used; do not
invent one. A new asymmetry with a stated reason is not the failure mode; an
unexplained one is.

## Q2 — no root. See [PD-9](../planning/decisions.md).

This was the genuinely open one, and the argument that settled it is not the
security argument the WI led with.

`~/.config/systemd/user/` is a **rendered install target on every host**, and
k-homelab's own recipes say so: `kaed.service` comes from kaed's published
bundle, `kfdc-curator.{service,timer}` from `~/src/tools/kfdc/systemd/`,
`kdeskdash-claude-poll.*` from kdeskdash's package-store bundle,
`kpidash-client.service` is rendered by its recipe. Each recipe asserts the
installed copy matches its source and reinstalls when it does not. Editing
there desyncs the host copy from the repo that owns it and gets overwritten on
the next apply — the exact shape 004's D1 refused for kubsdb's rsync targets.

**Every one of those sources is already in a kaed root.** So Q2 names a
*routing* gap, not a reach gap: the agent editing units over ssh was not
blocked by kaed, it was editing the wrong copy.

The security tail is real but secondary, and it is what rules out the
root-it-with-a-warning compromise that 013 used for `kubsdb:hvsim`.
`kaed.service` lives in this directory with `ExecStart=%h/.local/bin/kaed
serve`; `deny.rs` layer 1 is non-configurable precisely so kaed cannot rewrite
its own credential, and its own launch vector is the same class. It cannot be
fixed lexically either — deny `kaed.service` and every other enabled unit still
reaches execution at its next timer fire.

The WI's "the status quo is not neutral" point assumes the caller already has a
shell. **The set of kaed callers is larger than the set with ssh**, and Desktop
Claude on cleo — the client kaed was built for — has kaed and no exec at all.
For that caller the root is not a wash, it is boot-time execution as `ken`
granted to the one identity deliberately without any.

## What shipped

- **[PD-8](../planning/decisions.md)** — root a scratch directory where one
  exists and is used; `kubs0:scratch` added, kubsdb still none. Carries the
  generalisable finding: partial coverage of a symmetric task selects against
  kaed harder than no coverage would, so a gap in one host's roots is a
  fleet-wide cost, not a local one.
- **[PD-9](../planning/decisions.md)** — `~/.config/systemd/user/` out of
  scope, on source-of-truth grounds, with the alternative named: author the
  unit in its owning repo (all of which are roots) and install from there.
- **k-homelab `manifests/kubs0.yml`** — the `scratch` root declared. It has to
  land in the manifest: `roots` are owned as an exact set by the `kaed-service`
  recipe, so a hand-added root is *deleted* on the next apply.
- **k-homelab `recipes/kaed-service/README.md`** — the "Roots are deliberately
  not uniform" table updated to match, since it is the copy an agent reads.
- **`.claude/skills/deploy-fleet/SKILL.md`** — the fleet table's convenience
  copy of kubs0's roots.

No kaed source changed; `just check` is unaffected but was run.

## Follow-ups

- **The manifest change needs `bin/apply kubs0 kaed-service`**, which
  **restarts kaed on kubs0** and drops every live MCP session there — roots are
  resolved once at startup, so a SIGHUP will not do it. Deliberately not run as
  part of writing this record.
- **Slice 2 (agent-skills #1559, proposal 1562) is now unblocked.** Its inputs
  from here: the root list gains `kubs0:scratch`, and its "kaed does not cover
  X — for those, do Y" list gains `~/.config/systemd/user/` with PD-9's
  routing answer attached, not just the exclusion.
