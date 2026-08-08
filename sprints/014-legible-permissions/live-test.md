# Sprint 014 — live test from a real client (korg #1094)

*The closing verification for every finding
`sprints/013-kubsdb-broad-access/live-test.md` left open. Companion to that
document; same client, same route, same probes, so the two can be read
side by side.*

> **Status: RUN 2026-08-08** from cleo through the kai gateway against
> `0.1.0 (88005bc 2026-08-07)` on all three hosts. **Every finding closed;
> no regressions; no new findings filed.**
>
> Both of `deploy.md`'s pre-registered disagreements are confirmed, and the
> second one resolves:
>
> - The five newly-classified files do refuse as
>   `not_readable_by_service_identity`, not `classified_opaque` — recorded
>   as a pass for the stated reason (0600 root:root; the OS refuses before
>   kaed can read the bytes it would classify). The classify globs are
>   dormant policy that becomes load-bearing only if a mode changes, which
>   is the korg #1085 scenario they were added for.
> - `unreadable_hidden` is **6** on a *complete* walk, not 1 or 2. Both
>   lower numbers were **truncated** counts: the smoke test's 2 and this
>   run's own first probe (144 files, `truncated: true`) stopped walking
>   when `max_results` filled. Re-run with a non-matching pattern the count
>   settles at 6 over 154 files, and it decomposes exactly — see “The six”
>   below. Nothing is wrong; a partial walk reports a partial count and
>   says so via `truncated`.

## How to run it, and why it matters that it is run this way

From **cleo**, through the **kai gateway**, never against kubsdb directly.
Both the original run and korg #1085's recon were client-side, and the
point of the exercise is what a real agent sees — which is not what the
host sees. The kai host session that wrote the fix is the wrong vantage
point by construction: it can read `/datastore` over ssh and would prove
nothing about the contract.

Record results inline below, then:

- file the result as a korg report with a `finding` edge to whichever items
  it closes;
- **comment the outcome on korg #1085** — that brainstorm was set `done` on
  the strength of analysis rather than observation, and this run is what
  turns that into verified. If anything here fails, #1085's conclusion is
  what should be revisited first.

---

## What must have changed

| Finding (013) | Item | Expected on re-run | Result |
|---|---|---|---|
| **F-1** — `search` on `kubsdb:datastore` dies on `/datastore/lost+found` | #1088 | unscoped `search` over the root **works**; EACCES directories skipped and *counted* in `unreadable_hidden`, alongside `denied_hidden` / `classified_hidden`. All three original forms: concrete root, `kubsdb:*`, `*:*` | **PASS** — all three forms return results. `unreadable_hidden: 6`, `denied_hidden: 8` |
| **F-2** — `kubsdb:*` proxied wholesale, returning kubsdb's `no_credential` world-model as the caller's | #1089 | `hosts_unavailable` empty (or absent) for `kubsdb:*`, matching `kai:*` and `*:*` | **PASS** — field absent entirely; three-root `fanout` identical in shape to `*:*` |
| **F-3.1/3.2** — EACCES is a bare `internal` error | #1091 | structured `denied` carrying `path`, `reason: not_*_by_service_identity`, `owner`, `service_identity`, and `root_advisory` routing to `kubs0:k-homelab/recipes/docker-services/files/<svc>/` | **PASS**, and past spec — also carries `directory`, `rule`, and a hint that corrects the *original finding's own misdiagnosis* |
| **F-3.3** — `dry_run` false green | #1092 | dry_run against a root-owned file refuses with the same reason the real write gives | **PASS** — refusal payload byte-identical to the real write's |
| **#1085** — root-owned config is discoverable-then-opaque | #1093 | the `kubsdb:datastore` description names k-homelab as the source of truth; the five secret files refuse as `classified_opaque`, not as an OS error | **PASS** with the pre-registered variance — description names k-homelab; refusal is `not_readable_by_service_identity` |

## The specific probes worth repeating verbatim

The originals, so the two documents compare line for line.

1. **`search {root: "kubsdb:datastore", pattern: "…"}`** — unscoped, the
   call that used to die. Then the same as `kubsdb:*` and `*:*`.
2. **`read` / `write` / `search` against
   `/datastore/prometheus/prometheus.yml`** — the exact file that produced
   kubsdb `failure_id 1`. Read should still work (it is world-readable);
   the *write* is the one that must now explain itself.
3. **`edit` with `dry_run: true`** on that same file — the #1092 case.
   Expect a refusal, not a diff. This is the one behaviour change that can
   make a previously green call go red, and seeing it go red **is** the
   pass condition.
4. **`list postgresql`** — confirm the discoverable-then-opaque shape now
   resolves to a *reasoned* refusal rather than a size followed by an
   unexplained failure. The five classified files should now refuse
   `classified_opaque` on read.
5. **The value probe**: `search` for `://` scoped to `korg`, and
   `postgres(ql)?://` over `kubsdb:src`. These passed originally and are the
   regression guard for 008 — a classify-list change (#1093) is exactly the
   kind of edit that could disturb them.
6. **`roots`** — confirm the description advisory is present and reads
   cleanly next to D-4's existing `hvsim` DEPLOY TARGET text. Then check
   that the *same words* come back inside a write refusal's
   `root_advisory` (014 D-3 — they are literally the same string).

## Also confirm nothing regressed

D-1 through D-5 of 013 all held as specified in the original run. Re-check
the cheap ones so this document stands alone as evidence rather than
needing the first one to be trusted:

- [x] the `denied_hidden: 2` listing of `kubsdb:datastore` — unchanged,
      same 11 entries
- [x] `korg/korg.env` reading redacted — same two sealed placeholders,
      same digests as 013 (`@05119dd15299304b`, `@e1e43d10d5b8e8a5`)
- [x] `/gratch` still `outside_root`
- [x] journal identity propagation — txn 2 on `kubsdb:src`, `author:
      "claude"`, read back through the gateway
- [x] the deny list is back to five globs — `check-config` on kubsdb shows
      `**/secrets`, `**/*.key`, `/datastore/*/data`, `/datastore/_retired`,
      `/datastore/packages` and **no** `/datastore/lost+found`
- [x] value probe intact (the 008 regression guard) — `://` scoped to
      `korg` still returns zero matches over two files searched;
      `postgres(ql)?://` over `kubsdb:src` still zero with
      `classified_hidden: 2`

---

## Run

### 1 — F-1: the call that used to die *(#1088)*

All three forms work. The root is searchable again:

| form | files_searched | denied_hidden | unreadable_hidden |
|---|---|---|---|
| `search {root: "kubsdb:datastore"}` (complete walk) | 154 | 8 | 6 |
| `kubsdb:*` → datastore leg | 154 | 8 | 6 |
| `*:*` → datastore leg | 154 | 8 | 6 |

The counters are consistent across all three, and `denied_hidden: 8`
reconciles exactly against D-1's rules: `_retired` + `packages` at the top
level, plus the six `*/data` directories (grafana, kwebi, mongodb,
postgresql, prometheus, redis).

**The six.** The sprint predicted one (`lost+found`); the smoke test saw
two. The complete-walk figure is six, and it is exactly the five files
#1093 classified plus the directory that started all this:

```
/datastore/lost+found                                    (dir, 0700 root)
/datastore/postgresql/docker-compose.yml                 (0600 root)
/datastore/mongodb/docker-compose.yml                    (0600 root)
/datastore/redis/docker-compose.yml                      (0600 root)
/datastore/unpoller/up.conf                              (0600 root)
/datastore/grafana/provisioning/datasources/datasources.yml  (0600 root)
```

Confirmed against the host: those are precisely the paths ken cannot read
outside the denied trees, and precisely the five paths `check-config`
lists as explicitly classified. Nothing unaccounted for.

The earlier low counts were truncation, not undercounting — this run's own
first probe returned `files_searched: 144`, `unreadable_hidden: 2` **with
`truncated: true`**, because `max_results` filled before the walk
finished. Re-running with a non-matching pattern completes the walk and
settles at 6. Worth knowing when reading these counters: they describe
what the walk actually reached, and `truncated` is the flag that says so.

### 2 — F-2: the single-peer pattern *(#1089)*

`search {root: "kubsdb:*"}` now returns **no `hosts_unavailable` field at
all**, with a three-entry `fanout` covering `kubsdb:datastore`,
`kubsdb:hvsim` and `kubsdb:src`. Structurally identical to what `kai:*`
and `*:*` produce. The gateway is expanding the pattern locally instead of
forwarding it whole, so the peer's world-model no longer reaches the
caller.

### 3 — F-3.1/3.2: EACCES made legible *(#1091)*

`read` of `postgresql/docker-compose.yml` — the 0600 root-owned file that
was a bare `internal` error in 013:

```jsonc
{
  "code": "denied",
  "reason": "not_readable_by_service_identity",
  "rule": "unix permissions",
  "path": "postgresql/docker-compose.yml",
  "owner": {"uid": 0, "gid": 0, "mode": "0600"},
  "service_identity": {"uid": 1000, "user": "ken"},
  "root_advisory": "… MANAGED: grafana/, prometheus/, package-store/ and
     registry/ are rendered by k-homelab's docker-services recipe … edit
     the source at kubs0:k-homelab/recipes/docker-services/files/<svc>/…"
}
```

Every field #1094 asked for, plus `rule`. The hint also states outright
that *"no deny rule, `.kaedignore` or classification is involved, which is
why `list` can still see the name"* — which directly answers 013's F-3.1
complaint. The file is still discoverable; that is now explained rather
than merely true.

`write` to `prometheus/prometheus.yml` — the exact file behind kubsdb
`failure_id 1` — refuses as `not_writable_by_service_identity`, with
`directory: "prometheus"` and `owner.mode: "0755"`.

**The hint corrects a misdiagnosis in the original finding.** 013 recorded
this as a root-owned *644 file* failing to be written. The real obstacle
is the containing directory: kaed writes atomically — temp file beside the
destination, then rename — so it needs write+execute on the directory, and
the hint says so in as many words: *"The file's own mode is not the
obstacle."* The 013 report reasoned from the file mode and got the right
outcome for the wrong reason. This error would have prevented that.

### 4 — F-3.3: dry_run made honest *(#1092)*

`dry_run: true` against `prometheus/prometheus.yml` now **refuses**, and
the refusal payload is byte-identical to the real write's — same `reason`,
`owner`, `directory`, `service_identity`, `root_advisory`, same hint. The
previously green call is now red, which is the pass condition.

### 5 — #1093: the root description, and D-3's single string

`roots` carries the extended `kubsdb:datastore` description naming
k-homelab as the source of truth, reading cleanly after D-4's existing
`kwebi/app` advisory in the same string, and sitting alongside `hvsim`'s
unchanged DEPLOY TARGET text.

**D-3's assertion holds verbatim**: the `root_advisory` returned inside
both refusals above is character-for-character the `description` returned
by `roots`. One fact, two directions, one string — no drift possible
because there is only one copy.

### 6 — A side-effect worth recording

`unreadable_hidden` is fleet-wide, and it immediately surfaced something
nobody was looking for: `kubs0:src` reports **1**, a pre-existing
condition that was silently invisible before this sprint. Spot-checking
the host shows broken symlinks inside third-party checkouts — benign, and
not worth chasing. The point is that the counter earned its keep on a host
the sprint was not even about, which is the argument for having fixed the
class rather than adding `/datastore/lost+found` to a deny list.

## Findings

**None.** Nothing filed through `feedback` this run — the first live test
in this program to end that way.

The two observations above are recorded rather than filed, because neither
is a defect:

- **Truncated walks report partial counters.** `unreadable_hidden` (and
  its siblings) describe what the walk reached, so a search that fills
  `max_results` reports less than the full picture. It is flagged —
  `truncated: true` rides the same response — and reading one without the
  other is a caller error, not a contract gap. Noted because it explains
  three different numbers (1, 2, 6) for the same root across three
  sessions, and someone will otherwise re-derive that confusion later.
- **`kubs0:src` `unreadable_hidden: 1`.** Newly visible, pre-existing,
  benign.

### On the 013 findings

All five are closed by observation, not by assertion. F-1 and F-2 were
filed through `feedback` (kai journal ids 1 and 2) and are answered by
#1088 and #1089; F-3's three parts are answered by #1091 and #1092; and
#1085's premise — root-owned config being discoverable-then-opaque — is
answered by #1093 plus the structured refusal, which together turn the
opaque case into a signposted one.

korg #1085 was set `done` on analysis. This run is the observation that
backs it: the scenario it described is reachable from a client, the
refusal now names the host fact causing it, and the advisory routes to
k-homelab where the file is actually managed. Nothing in it needs
revisiting.
