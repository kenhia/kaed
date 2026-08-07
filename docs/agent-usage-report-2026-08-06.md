# Agent usage report — 2026-08-06

*First journal evidence pass. korg:1054 (sprint 006) / WI #1044.*
*Snapshot: kai + kubs0 journals as of 2026-08-07T01:40Z.*

> **This is a baseline, not a study.** The corpus is 41 transactions. Nothing
> here is a rate in any statistical sense, and it is not written as one. Its
> value is as a fixed point to compare a later pass against — which is why the
> queries are committed alongside it as `scripts/journal-report.py`.
>
> Reproduce with:
> ```
> python3 scripts/journal-report.py                    # on kai
> ssh kubs0 python3 - < scripts/journal-report.py      # from kai
> ```

## The corpus

| | kai | kubs0 | total |
|---|---:|---:|---:|
| transactions | 34 | 7 | **41** |
| file touches | 46 | 11 | **57** |
| failed attempts | 4 | 1 | **5** |
| blobs | 62 | 14 | 76 |
| feedback rows | 0 | 0 | 0 |
| span | 2026-08-02 → 2026-08-07 | 2026-08-03 → 2026-08-06 | 5 days |
| authors | `claude` | `claude` | 1 |

Of those 41, **34 are organic work**. The rest are instrumentation of kaed
itself and should not be counted as usage:

- kai #1 (`sprint 001 deploy dogfood`), kai #6 (`002 live verify`)
- kubs0 #1, #2 (`sprint 004 kubs0 deploy verification`)
- kai #32, #33, #34 — this sprint's own writes

Same for the failures: **3 of 5 were deliberately provoked** during deploy
verification (`intent` reads `should be refused`, `002 live verify: forced
conflict`, `force a conflict`). Only 2 are real friction.

---

## Findings

### F1 — kaed is in sustained use, not abandoned after the dogfood

34 organic transactions across 5 days, 2 hosts and 6 repos, with activity on
every day since each host was deployed. The obvious failure mode for a tool
like this — used enthusiastically during its own sprint, then quietly dropped
for ssh — has not happened.

### F2 — In practice it is a documentation editor

**81% of file touches are `.md` or `.html`** (46 of 57). Line churn is
**+2988 / −183** — a 16:1 additive ratio, the signature of writing new planning
documents rather than changing existing code.

| extension | touches |
|---|---:|
| `.md` | 45 |
| `.py` | 4 |
| `.rs` | 2 |
| `.sh` | 2 |
| `.html` | 1 |
| `.yml`, `.example`, none | 3 |

kubs0 is **100% markdown**. And the sharpest version of the finding: **no Rust
source file has ever been edited through kaed outside the two verification
fixtures** (`kaed-dogfood/demo.rs`, `kaed-002-verify/app/main.rs`) — in a
homelab whose most active projects are Rust.

**Read this correctly.** kaed's primary purpose is *remote* editing — replacing
rclone-mount and base64-over-ssh for agents working on the Linux hosts from
elsewhere. By that measure the tool is doing the job it exists for, daily, on
two hosts. The doc-heavy mix is therefore a fact about **what remote sessions
are**, not a shortfall in kaed's reach: remote sessions from cleo are
predominantly planning and design work, while code sessions run locally on the
host where the code is (F5, and Ken's confirmation of the zero-kaed repos).

So the open question is not "will kaed graduate to editing code" — it is
whether there is remote work that *wants* to touch code and doesn't, because
something is missing. See [Baseline for the next pass](#baseline-for-the-next-pass).

This is still the single most decision-relevant fact in the report. See
[Implications](#implications-for-the-program).

### F3 — Reliability is high, and the errors are genuinely recoverable

**2 real failures in 34 organic transactions.** Both were recovered from within
half a minute, using the error payload rather than a re-read:

| failure | recovered by | elapsed |
|---|---|---:|
| kai #3 `invalid_input` 04:24:16 — `create` on a file that already existed | txn #9 04:24:42 | **26 s** |
| kai #4 `version_conflict` 23:44:40 — kfdc roadmap moved under the agent | txn #18 23:45:00 | **20 s** |

The second is the more interesting one, because the recovery is self-documenting:
txn #18's intent ends `...(re-applied after sprint-002 roadmap update)`. The
agent read the conflict, understood *why* the file had moved, and said so. That
is design principle 4 — errors carry recovery data — working exactly as
intended, and it is precisely the kind of evidence a survey would never surface.

The `invalid_input` case is worth a second look though: the agent tried to
`create` three files, one of which existed. The message named the fix
("pass overwrite to replace it, or declare it in base and edit it") and the
retry succeeded. Good error, but the agent had to *fail once* to learn the file
was there.

### F4 — `intent` is universally populated and substantive

**0 of 41 transactions are missing an intent.** Median length 142 chars on kai,
134 on kubs0; longest 330. They are explanatory, not ceremonial — e.g.

> *"Fix cold-start.sh's credential verification: loopback inside the postgres
> container matches pg_hba's `trust` rule, so the check passed regardless of
> password. Connect via the container's network name instead…"*

R6 promises a successor can understand an edit. On this corpus that promise is
being kept, without any enforcement mechanism requiring it.

### F5 — Usage concentrates where the work is remote and doc-shaped

Against all git activity on kai since 2026-08-02 (9 repos, 76 commits, 527
file-changes):

| repo | git file-changes | kaed touches | kaed share |
|---|---:|---:|---:|
| agent-pitches/khound | 25 | 17 | **~68%** |
| tools/korg | 209 | 10 | ~5% |
| tools/kfdc | 126 | 8 | ~6% |
| ai/kaed | 93 | 5 | ~5% |
| tools/kdeskdash | 39 | 0 | 0% |
| fun/hv-simulator | 17 | 0 | 0% |
| ai/kmon | 16 | 0 | 0% |
| tools/sknext, tools/kpidash | 2 | 0 | 0% |

khound — a planning/pitch repo driven remotely from cleo — is the one place
kaed did most of the work. The five repos with **zero** kaed involvement are
accounted for: Ken confirms those were sessions run locally on the machine,
where the built-in editing tools are the correct choice and kaed was never the
intended path.

So the 527 figure is **not** a bypass rate, and must not be quoted as one. It is
the wrong denominator for judging kaed, and the right denominator — edits
attempted by *remote* sessions — is not recoverable from this data (see G3).

### F6 — `ambiguous_anchor` has never fired

Zero occurrences in production. Only three error codes appear at all:
`version_conflict` (2), `denied` (1), `invalid_input` (1). Either anchors are
being chosen well, or agents are pre-emptively avoiding them by reading first.
The journal cannot distinguish those, and the difference matters for #1046 —
prompting for feedback on an error that never fires is wasted surface.

### F7 — The `feedback` table already exists

`journal.rs:65` creates it (`author, category, summary, detail, context,
created_at`). It has 0 rows and **nothing in `src/` writes to it** — no MCP tool
is exposed. The storage half of #1046 is already built.

---

## What the journal cannot answer

These are structural, not sampling, limits. They will not improve with time.

### G1 — Reads are not journaled at all

Only write transactions and failed write attempts are recorded. #1044 asked for
"read shape: `window`/`range` vs whole-file" and "`denied` hits and what the
agent did next" — for reads, **both are unanswerable**. A `denied` on a read,
an agent giving up on anchors and pulling a whole file, a `too_large`
truncation: none of it exists on disk.

This is the biggest blind spot, and it sits directly over the question the
program most wants answered — whether refusals push agents to ssh.

### G2 — The failure record starts 2026-08-02T18:02

`txn_failures` began with sprint 002 (#910). kai transactions #1–#5 predate it,
and **at least one failure is known-missing**: txn #3's intent reads
*"Re-anchor after version_conflict"*, but no corresponding failure row exists.
Any before/after comparison must start from 2026-08-02T18:02, not from first
deploy.

### G3 — Remote and local sessions are indistinguishable

Nothing in the journal records where the client was. F5's denominator problem
cannot be closed from this data. Closing it needs either a client-side
attribution field or asking — it will not emerge from more journal rows.

### G4 — There is one author

Both hosts see a single identity, `claude`. Per-author conflict rate (#910) is
degenerate today: it is one bucket. It becomes meaningful when identities
multiply — which is exactly what PD-4 protects in the gateway design.

### G5 — Tooling notes

- **There is no `sqlite3` CLI on kai or kubs0.** The "one query" story assumes
  a query path; in practice it is `python3`.
- **kai's `journal.db` is 4 KB with a 1.7 MB WAL.** All rows live in the WAL.
  Opening with `immutable=1` silently reports an **empty database** rather than
  an error — anyone who reaches for it will conclude kaed has never been used.
  Use `mode=ro`. This trap is documented in the script header.

---

## Implications for the program

| slice | change |
|---|---|
| **#1046** feedback tool | **Re-size down** — storage exists (F7). But G1 means the friction prompt cannot live only on the transaction path, since the friction most worth catching is on reads. And F6 says prompt selectively: don't wire prompts to codes that never fire. |
| **#1049** history tools | The query axes in `scripts/journal-report.py` are the proven set. Also a decision this report forces: **add a read log, or accept G1 deliberately** — right now the blind spot is an accident, not a choice. |
| **#929** kubsdb | **Best evidence yet, and it points the same way #929 leaned.** F2 says agents use kaed to edit docs and config, not data or code. On kubsdb that is compose files, `.env`-shaped config and runbooks — exactly the "config dirs yes, data dirs no" shape — and it strengthens the case that `/gratch` and the database data dirs are simply not what this tool gets used for. |
| **#1045** addressing | kai's journal references a `home` root that **no longer exists** (narrowed away by 002). Renaming roots orphans historical journal rows against names that cannot be resolved. Host-qualification will do this again at scale — decide whether `roots` history is migrated or just accepted as historical. |
| **#1048** secrets | No signal either way. Only one `denied` ever fired, and it was a deliberate test against `.env`. There is **no evidence yet of agents hitting secret files in real work** — which is itself an argument for shipping redaction without `reveal`, as planned. |

---

## Baseline for the next pass

Re-run `scripts/journal-report.py` and compare against:

| metric | 2026-08-06 |
|---|---:|
| organic transactions | 34 |
| hosts / repos / authors | 2 / 6 / 1 |
| doc share of file touches | 81% |
| line ratio (added:removed) | 16:1 |
| real failures / organic txns | 2 / 34 |
| median recovery time after failure | ~23 s |
| transactions missing `intent` | 0 |
| `ambiguous_anchor` occurrences | 0 |
| `feedback` rows | 0 |
| distinct error codes seen | 3 |

**The three questions to ask next time:**

1. **Did the doc share move, and if not, is anything blocking it?** The doc-heavy
   mix is expected — remote sessions are mostly planning sessions, and kaed is
   already succeeding at its actual purpose, which is remote editing. The
   question worth asking is the narrower one: **was there remote work that
   wanted to edit code and didn't?** kaed today has no `insert`/`delete`/
   `rename`, no tree-sitter `outline`/`node_replace`, and no `apply_patch` —
   all queued on the roadmap. If remote code editing is being routed to ssh
   because anchor/range edits alone are too blunt for real refactoring, that is
   a capability gap and it should reorder those roadmap items. If instead code
   work simply happens where the code is, the mix is correct and needs no fix.
   G3 means the journal cannot separate these — ask, or add client attribution.
2. Did `feedback` get rows, and did any of them change the contract? That is
   #1046's real deliverable, not the tool.
3. Did a second author appear? Everything about attribution — #910, PD-4, the
   secrets audit stream — is untested at one identity.
