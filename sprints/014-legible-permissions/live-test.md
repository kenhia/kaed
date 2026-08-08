# Sprint 014 — live test from a real client (korg #1094)

*The closing verification for every finding
`sprints/013-kubsdb-broad-access/live-test.md` left open. Companion to that
document; same client, same route, same probes, so the two can be read
side by side.*

> **Status: not yet run.** This is the prepared script; it is filled in
> from an actual session. It **must run after the deploy**, or it verifies
> the previous build.

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
| **F-1** — `search` on `kubsdb:datastore` dies on `/datastore/lost+found` | #1088 | unscoped `search` over the root **works**; EACCES directories skipped and *counted* in `unreadable_hidden`, alongside `denied_hidden` / `classified_hidden`. All three original forms: concrete root, `kubsdb:*`, `*:*` | |
| **F-2** — `kubsdb:*` proxied wholesale, returning kubsdb's `no_credential` world-model as the caller's | #1089 | `hosts_unavailable` empty (or absent) for `kubsdb:*`, matching `kai:*` and `*:*` | |
| **F-3.1/3.2** — EACCES is a bare `internal` error | #1091 | structured `denied` carrying `path`, `reason: not_*_by_service_identity`, `owner`, `service_identity`, and `root_advisory` routing to `kubs0:k-homelab/recipes/docker-services/files/<svc>/` | |
| **F-3.3** — `dry_run` false green | #1092 | dry_run against a root-owned file refuses with the same reason the real write gives | |
| **#1085** — root-owned config is discoverable-then-opaque | #1093 | the `kubsdb:datastore` description names k-homelab as the source of truth; the five secret files refuse as `classified_opaque`, not as an OS error | |

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

- [ ] the `denied_hidden: 2` listing of `kubsdb:datastore`
- [ ] `korg/korg.env` reading redacted
- [ ] `/gratch` still `outside_root`
- [ ] journal identity propagation (a txn on kubsdb under `claude`)
- [ ] the deny list is back to five globs — `/datastore/lost+found` gone,
      matching `deny.rs::the_kubsdb_broad_access_shape_holds` again

---

## Run

*(filled in from the session)*

## Findings

*(filled in; file each one before moving on, as 013 did)*
