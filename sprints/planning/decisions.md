# Planning decisions

> Decisions taken at the **planning** level — spanning sprints, not made inside
> one. Numbered `PD-n` to keep them distinct from the per-sprint `D-n`
> decisions in `sprints/00N-*/decisions.md`.
>
> Design docs: [roadmap](roadmap.md) · [summary](summary.md) ·
> [overview](overview.md) · [mcp-contract](mcp-contract.md) ·
> [architecture](architecture.md) ·
> [secrets brainstorm](brainstorm-secrets-editing.md) ·
> [gateway brainstorm](brainstorm-gateway-mcp.md)

---

## PD-1 — Sequencing: addressing, then secrets, then routing, then kubsdb

*2026-08-06, session with Ken. Recorded as korg program 1063 (nine slices).*

Four things were on deck with no agreed order: #930 (fleet discoverability),
#929 (kubsdb), the secrets model, and the gateway. The order taken is:

1. Journal evidence pass (#1044) — no code
2. Host-qualified addressing + declared fleet (#1045, closes #930)
3. k-homelab #1033 — stale kaed-service advisories
4. Secrets slices 1–2 (#1047, #1048)
5. History tools + `feedback` (#1049, #1046)
6. Gateway peer mode (#1050)
7. Secret lifecycle + cross-session handoff (#1051, #1052)
8. Write-side leak detection (#1053)
9. kubsdb (#929)

**The two load-bearing reasons:**

**Addressing goes first because it is what locks you in.** Not the gateway —
only *step 1* of it: host-qualified root names and a richer `roots` response,
shipped against a single instance. Two later features need a host-qualified
namespace (peer mode, and the location half of the secret handle in PD-3).
Today that costs a config edit and a client rewire; after either has shipped it
is a migration of live references. Peer routing is then a routing feature
rather than a redesign, which is exactly the split
`brainstorm-gateway-mcp.md` recommends.

**Secrets go before kubsdb because #929 is a secrets problem in a deployment
costume.** Its central difficulty — `/datastore/postgresql` and
`/datastore/korg/korg.env` side by side under a lexical deny matcher — *is* the
deny-vs-classify distinction from the secrets brainstorm. Without
classification there are only two shapes for broad kubsdb access: deny so much
it is narrow again (which #929 says buys almost nothing), or hand agents
plaintext DB credentials.

**Measurement goes before both** because three later slices are waiting on
evidence, and because it is the honest answer to "how do we get feedback from
the agent's view" (see PD-6).

**What would reorder this:** if the evidence pass finds a meaningful fraction of
edits *bypassed* kaed, that outranks everything here. The whole program assumes
agents are using the tool and the open question is what it can safely do. If
they are routing around it, the problem is a different one.

---

## PD-2 — Secret placeholder identity is BLAKE3 with an entropy floor, not HMAC

*2026-08-06. Supersedes the HMAC proposal in `brainstorm-secrets-editing.md`
§ "Redact + restore". Implemented in #1048.*

The brainstorm specified a truncated **HMAC** under a server-held key, on the
grounds that bare hashes of low-entropy values (`POSTGRES_PASSWORD=postgres`,
`DEBUG=true`) are brute-forced instantly. Ken pushed back in favour of
**BLAKE3**. The resolution keeps the security property by a different
mechanism:

- Placeholder digest is `blake3(value)`, truncated for readability.
  **Use the same truncation the version primitive already uses** —
  `fsops.rs::version_of` is `blake3::hash(bytes).to_hex()[..16]`, i.e. BLAKE3
  at 64 bits, shipped since 001. Matching it settles the length question by
  precedent instead of by argument, and keeps one hash story in the codebase.
- A value that does not clear an **entropy floor** gets no digest at all:
  `⟨kaed:DEBUG⟩`, not `⟨kaed:DEBUG@1a2b⟩`. The brute-force objection is
  answered by *withholding* the digest, not by keying it.

**Three consequences, and the third is the reason this matters:**

1. It **deletes an open question** the brainstorm listed as needing an early
   answer — where the HMAC key lives, whether it survives restart, how *it*
   gets rotated. There is no key.
2. Stability is preserved, so the three things the brainstorm wanted still come
   free: change detection across reads, equality across files, and
   rotate-everywhere.
3. The digest becomes **host-independent**. Any kaed instance can verify a
   handle by recomputing it, with no shared fleet secret and no coordination.
   That is what makes PD-3 possible.

**A fourth consequence, noticed after the decision was taken and worth
recording because it makes the choice over-determined:** kaed *already* uses
BLAKE3 as its content-address primitive — `version_of` in `fsops.rs` has hashed
file bytes with it since sprint 001, and every `version` an agent has ever held
is a truncated BLAKE3. So this is not a new dependency or a new hash story; it
is the existing one applied to a value instead of a file. The symmetry is exact
and it is the same symmetry PD-3 leans on: a `version` is a durable content
address for a file, and a placeholder digest is a durable content address for a
secret.

**Do not re-litigate this back to HMAC mid-sprint** without reopening this
decision. The entropy floor is doing the work the key was doing.

---

## PD-3 — Cross-session secret handoff: reference by location + digest, and the gateway carries values

*2026-08-06, Ken's scenario and Ken's risk call. Implemented in #1052.*

**The problem.** A korg program spans sprints in different sessions on
different machines. A service gets a new access token in sprint 1; the client
needs it in sprint 3, on another host. The two existing options are both bad:
paste it into the handoff (a secret in korg, forever) or have the second
session read it (an agent holding plaintext it has no use for).

**The design.** `load_secret(path, key)` returns a *handle*, never a value, and
**the persistence lives in the file the secret was already written to** — no
side registry, nothing to expire or garbage-collect. Ken's first sketch was a
registry of generated opaque references; he replaced it with this, and the
replacement is right for a reason kaed already committed to: **R1 says versions
are durable content addresses, not session handles.** A secret reference obeys
the same rule. A UUID-keyed registry would have been a session handle wearing a
content-address costume.

**The handle carries two things because there are two questions:**

| part | form | answers |
|------|------|---------|
| location | host-qualified root + path + key | "give me the current value here" |
| digest | BLAKE3, per PD-2 | "is it still the value I was told about" |

Resolve by location; if the digest no longer matches, **fail loudly** rather
than silently returning the current value. Both semantics are legitimate — a
handoff usually wants current, a verification wants exact — but which one is
happening must never be ambiguous.

**Detection and transport are separate, and only one needs the gateway.**
Because PD-2 made the digest host-independent, *change detection* works between
unconnected instances with no shared secret. *Transport* — actually moving the
bytes from the host holding the value to the host writing it — needs peer mode
(#1050). Through the gateway the value transits in memory, over the tailnet, and
**never through the agent**.

**Accepted risk (Ken's call, recorded rather than buried):** peer-mode transport
makes the gateway a secret-bearing component. Ken's assessment — *"if someone
got that deep on my machine, the game is already over"* — is the right call for
this homelab, and the honest frame in the secrets brainstorm agrees: this is
blast radius, not access control, and an attacker at that depth can `cat` the
file anyway.

**Obligations that follow, to be implemented and not merely noted:**

- Never log or journal the value on the transport path. Journal the *event*,
  not the payload.
- Do not persist it gateway-side.
- Zero the buffer after use.

**Prefer avoiding transport entirely.** If kaed generated or rotated the token
itself (#1051) it already holds the value and can write both locations — across
hosts, via peer mode — in one operation at rotation time; the handoff then never
happens and sprint 3 only verifies a digest. The handoff protocol is the
**fallback for externally-issued secrets** (a provider's OAuth token, a vendor
console API key). Build the rotate-both-places path first.

**Corollary for the korg `handoff` skill:** a handoff document carries the
reference — host-qualified path, key, digest — and never the value. "Was a
secret ever put in korg?" then answers "no, by construction", which is worth
more than a policy saying not to.

---

## PD-4 — Gateway identity propagation: per-agent tokens per backend

*2026-08-06. Selects option 3 of the three in `brainstorm-gateway-mcp.md`
§ "The thing that must not break". Implemented in #1050.*

The gateway holds a per-agent token for each backend and proxies with the
caller's real identity. Nothing about backend auth changes.

This **deliberately gives up** the "one auth surface" win: the client still
carries N tokens and #914's rotation story stays O(hosts × agents). That is the
correct trade. If the gateway proxied under its own credential, every edit on a
backed host would journal as "gateway", and conflict-rate-per-author (#910), the
evidence pass (#1044) and "has any agent ever seen this secret" (#1051) would
all collapse into one indistinguishable actor. **Identity fidelity beats
token-count reduction.**

Signed author assertions (option 1) and allowlisted-peer forwarded headers
(option 2) are more elegant and remain open if token sprawl actually starts to
hurt. Do not build them speculatively.

---

## PD-5 — The declared fleet lives in `config.toml [peers]`, surfaced through `roots`

*2026-08-06. Supersedes the shape recommended in korg #930's second comment.
Implemented in #1045.*

#930 recommended a `deploy/check.sh` reading a committed declared-fleet file,
and flagged "where does the declaration live" as the actual work. That shape has
rotted: sprint 005 went store-native and **kubs0 has no checkout** to run a
repo-shipped script from (the same root cause as k-homelab #1033).

The declaration therefore lives in `config.toml`, which is *installed* rather
than cloned — durable, per-host, present on clone-less hosts, and not a
gitignored file that lives nowhere:

```toml
[peers.kubsdb]
status = "deferred"
ref    = "korg:929"
note   = "broad-access design not settled; /datastore + /gratch adjacency"
```

and is surfaced through the `roots` response, which every MCP client calls
anyway and which needs no repo at all. #930's own criterion is what selects
this: the answer must be reachable *"from where the agent is already looking —
the host, or the tool surface — not from a doc it would have to think to
consult."*

**Three states must stay distinguishable** — `deferred` (with `ref`),
`unreachable` (with `since`), and `never-declared`. Collapsing any two rebuilds
exactly the confusion #930 was filed about.

Corollary: **do not add `list_available_targets`.** `roots` is that tool; it
just gains fields. Two discovery mechanisms answering one question is worse for
an agent than either alone.

---

## PD-6 — Agent feedback is instrumented, not surveyed

*2026-08-06. Re-shapes the `feedback` roadmap item; implemented in #1046.*

The question that started the session was how to get feedback on kaed from the
agent's point of view. The roadmap's answer was a `feedback` tool. That stays,
with a changed trigger model, because a tool an agent must *remember* to call is
answered by the agents that were already having a good time — the session that
hit friction and fell back to ssh never files a report.

**Primary channel: the journal.** kaed has recorded revealed preference since
#910 journalled failed transactions. Conflict rates, `denied` hits followed by
silence on that path (the agent probably routed around kaed), `ambiguous_anchor`
followed by a whole-file read (the agent gave up on anchors), retry chains,
`intent` quality. None of it requires asking.

**Secondary channel: `feedback`, fired at the moment of friction.** Errors that
are plausibly kaed's fault carry a short structured invitation naming the txn
reference. One required field at most — anything costing a thinking step loses
to finishing the task. Prompt only on errors the evidence pass shows actually
cause friction; prompting on all of them is noise, and noise is how a feedback
channel dies.

The deliverable is the roadmap's second clause: *the first contract revision
driven by a real agent friction report.* An unread channel is worse than none,
because it looks like a channel.

---

## PD-7 — One kaed identity per machine, not per harness or per human

*2026-08-16. Sprint 018 (korg #1350). Extends PD-4, which decided that the
gateway proxies as the caller; this decides what "the caller" is named
after.*

kaed had one client identity, `claude`, because it had one client. Wiring a
second and third — Claude Code on kai and on kubs0 — forced the question the
single-client era never asked: **what is an author an author *of*?**

**Decided: the machine.** `claude-kai`, `claude-kubs0`, cleo keeping bare
`claude`. Not `claude-code` (harness-shaped), not `ken` (human-shaped), and
emphatically not one shared `claude` for all three.

The reason is that a credential's name should describe the thing that can be
stolen and the thing that can be revoked, and both of those are *a file on a
host*. Machine-grain identity makes the credential's scope and its name the
same fact. Harness-grain does not — two Claude Code sessions on different
boxes would be indistinguishable, and the machine is the axis that actually
varies. Human-grain carries no information at all when there is one human.

Two consequences that outlive this sprint:

- **Attribution is the product, so sharing an identity forfeits it.** A
  journal that answers "some Claude somewhere" cannot answer "which machine
  changed this", which is the first question anyone asks of a cross-host
  edit. kaed sells verified attributed writes; a shared token is the one
  configuration choice that silently unsells it while every test still
  passes.
- **Revocation gets a blast radius equal to the identity's span.** One
  identity for three clients means one compromised client costs all three a
  rotation. Per-machine identity makes the fleet's credential graph a set of
  independent edges.

The cost is real and should be stated: **credentials multiply as
authors × endpoints**, because PD-4 means the gateway holds a distinct token
per (author, backend) pair rather than one per backend. Two new authors cost
six token files, and the fleet is now at nine with no inventory anywhere.
That is the honest price of PD-4 + PD-7 together, and it is the argument for
`krot` learning about kaed before a fourth client is added — not an argument
for going back to a shared token.

A backend must list authors that never dial it, because they arrive proxied.
`config.rs` already refuses to start on the converse mistake — a
`[peers.x.tokens]` entry for an author absent from `[auth]` — on the grounds
that nobody can authenticate as an identity the host does not know.
