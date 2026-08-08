# Sprint 014 decisions

Per-sprint `D-n`. Cross-sprint decisions live in
`sprints/planning/decisions.md` as `PD-n`.

---

## D-1 — EACCES is `denied` with its own reason, not a new error code

**Considered:** a new R4 code (`not_permitted` / `permission_denied`),
separating "kaed policy refused" from "the OS refused" at the code level.

**Decided:** reuse `denied`, add two `RefusalReason` variants —
`not_readable_by_service_identity`, `not_writable_by_service_identity`.

`denied`'s contract promise is "this will not work, and the remedy is not
a different path, so do not retry it." That is exactly true of a
permission refusal. What was missing was never the code: it was the
`reason`, whose entire job is naming *which layer* refused, and R4 already
promises it. A fifth layer joining four is a new reason, not a new
vocabulary — and adding a code would have made every existing client's
`denied` handling silently incomplete for a case it already handles
correctly.

The distinction #1088 insists on — "policy says no" vs "the OS said no" —
is preserved where it is actionable: in `reason`, in the hint, and in the
separate `unreadable_hidden` count (D-6). It is *not* preserved by
pretending the two need different retry semantics, because they do not.

## D-2 — writability is a question about the DIRECTORY

The obvious probe is `access(file, W_OK)`. It is wrong, and wrong in the
dangerous direction: it refuses writes that work.

kaed writes atomically — stage a temp file beside the destination, then
`rename(2)` over it. That needs write+execute on the **containing
directory** and consults the destination file's own mode not at all. So:

- a root-owned `0644` file in a `ken`-writable directory **is writable
  through kaed**, and the naive check would have refused it;
- a `ken`-owned `0644` file in a root-owned `0755` directory is **not**
  writable, and the naive check would have promised it.

`/datastore/prometheus/prometheus.yml` is the second case, which is why
013's live test saw the write fail. The probe therefore asks about the
deepest *existing* ancestor directory — for a create into a not-yet-existing
subtree, that is what `create_dir_all` will start from.

This is also why the refusal's evidence block names `directory`, not the
file's own mode: reporting the file's `0644` would be true and useless.

## D-3 — the route to the editable copy is config, not code

#1091 asks for a hint that routes to
`kubs0:k-homelab/recipes/docker-services/files/<svc>/`. That string must
not appear in kaed's source: the repo is public (003), and where a host's
rendered artifacts are managed from is a fact about that host.

**Decided:** the refusal carries the addressed root's own `description`
verbatim as `root_advisory`, and folds it into the message.

This is what makes #1093's advisory and #1091's hint "the same words" by
construction rather than by discipline — they are literally the same
string, and editing the root description on the host changes both at once.
It generalises for free: every future host with a managed root gets a
routing hint by writing one sentence in `config.toml`.

## D-4 — `dry_run` probes for real, and the probe runs on the write path too

#1092 offered two shapes: probe (option 1) or declare `writability:
unknown` (option 2). Option 1, as the item expected — it is the one that
matches the contract's posture, and the TOCTOU caveat is irrelevant at
this scale.

The addition: the probe runs **before the dry-run return, on both paths**.
A real write that would have hit EACCES in `stage()` now gets the same
structured refusal from the same line of code. One vocabulary, one code
path, and the prediction is literally the thing being predicted rather
than a second implementation of it that can drift.

`stage()` failing anyway (a race, or a refusal from above DAC) is
reclassified as a backstop rather than escaping as `internal` again.

**Consequence worth stating plainly:** a `dry_run` that used to "succeed"
against an unwritable path now fails. That is the fix, not a regression —
but it is the only change in this sprint that can make a previously green
call go red, so it is called out in `deploy.md` too.

## D-5 — a root pattern is always expanded by the instance that was asked

Two halves, both needed, and they fail differently:

1. **Patterns never proxy.** 010 D-1 routes on the raw `root` string when
   its host prefix names a routable peer, and it fired before D-5's
   local-expansion rule. `kubsdb:*` has host prefix `kubsdb`, so the whole
   call forwarded. Now `is_pattern` is checked first, so `kubsdb:*`,
   `kai:*` and `*:*` are one code path instead of two.
2. **A host the pattern excludes is not probed.** Without this, a gateway
   answering `kubsdb:*` would still probe kai and kubs0 and report *their*
   reachability under a search that could never touch them — #1089's lie
   with the hosts swapped.

The control, pinned in the gateway test: a caller with a genuine
credential gap still sees it under `*:*`, where the host really is part of
the search. D-5's no-silent-gap rule is preserved; only the claimed gap
that does not exist is removed.

## D-6 — `unreadable_hidden` is a third sibling, not a bigger `denied_hidden`

`list` and `search` already report `denied_hidden` and `classified_hidden`
so a filtered result is never mistaken for the whole (R7). Unreadable
entries join them as `unreadable_hidden` rather than folding into
`denied_hidden`, because the two have different remedies: one needs a
config change by a human, the other needs a chmod, a different identity,
or nothing at all (a `lost+found` no one wants).

A count, not a path list — the sibling shape, and the shape the live test
asked for. Two things newly counted that were **silently** skipped before:
an unreadable directory (which used to be fatal) and an unreadable file
(which used to be a bare `continue`).

Non-permission walk errors still fail the call loudly. They are not a
coverage question, and swallowing them would trade one silence for
another.

## D-7 — nothing kubsdb-shaped enters the shipped defaults

Same call as 013 D-7, restated because #1093 tempts otherwise: the six
classify globs are host config, pinned in a CI test that documents kubsdb's
config, not in `DEFAULT_CLASSIFY`. `**/docker-compose.yml` as a default
would classify every compose file on every host — most hold no secrets,
all would become opaque, and 013's live test verified the opposite
behaviour on `korg/docker-compose.yml`.
