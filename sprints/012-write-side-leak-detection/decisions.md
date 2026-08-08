# Sprint 012 decisions

> Per-sprint `D-n`, distinct from the cross-sprint `PD-n` in
> `../planning/decisions.md`. The two calls the proposal requires making
> deliberately — refuse-with-override vs warn, and precision-vs-heuristic
> strictness — are D-2 and D-3 here.

---

## D-1 — Detection runs on the write path, over newly-introduced content only

The check lives in the transaction engine, beside the vanish guard (008
D-10): after ops apply to the evolving buffers, every touched file that is
**not** classified is scanned. Classified dotenv files are exempt — they
are where secrets belong, and writes to them are already redacted,
guarded and journaled; classified-opaque files cannot be written through
kaed at all.

**Only newly-introduced tokens trip anything.** Candidates come from the
diff's added lines, and a candidate that already occurs anywhere in the
file's old content is skipped. Without this, a file that already contains
a token becomes uneditable through kaed — including the edit that would
*remove* the token — which is exactly the route-around-via-ssh failure
the brainstorm's honest frame warns about. A `create` (and an overwrite)
is all new content.

Placement in the engine, not the server, means every write path is
covered by construction: text ops, env ops on unclassified dotenv files,
`create`, `revert`, and `secret generate/rotate` targets — and on a
proxied edit the check runs on the target host, where the index lives, so
gateway mode needs nothing.

## D-2 — Refuse with a named override, not warn (the precise tiers)

A warning in a tool response is something an agent scrolls past; a
refusal that names its override forces a decision (the 008 `drop_keys`
reasoning, applied unchanged). Matches against a **known digest** or a
**provider prefix** refuse the transaction — `invalid_input` with
`data.reason: "secret_leak"`, the match list, and the exact override to
pass — mirroring the vanish guard's shape rather than `denied` (which
agents rightly treat as permanent and unretryable; this refusal has a
legitimate retry: the same request with the override).

The override is `allow_secrets: [...]` on the edit request, and its
entries are the exact `detail` strings the refusal reported — the
matched digest, or the provider prefix. A blanket boolean would be
learned once and passed always; naming the specific match keeps each
secret's disclosure a per-decision act, exactly like naming keys in
`drop_keys`.

Every refusal says what to do instead, per 008 D-5: reference the
variable rather than the value (the digest tier names the key and file the
value lives in), point at `value_from`/placeholders for dotenv targets,
and — when the target genuinely should hold secrets — at classifying it.

A PEM private-key armor line (`-----BEGIN … PRIVATE KEY-----`) rides the
refuse tier too: it is as precise as a provider prefix, the file it
belongs in is classified-opaque (unwritable through kaed anyway), and a
key block pasted into a doc is the same incident.

## D-3 — The entropy heuristic flags, and the flags are journaled so the evidence can promote it

The generic high-entropy check (011's `looks_secret_shaped`: provider
charset, ≥32 chars, clears the PD-2 floor) false-positives on base64
fixtures, checksums, minted-but-unindexed values. Getting the strictness
backwards produces a tool agents learn to route around, which costs more
than the leaks it catches — so this tier **warns and applies**: the match
lands in the response's `warnings`, never blocks.

But "flag rather than refuse *until evidence says otherwise*" requires
the evidence to exist: flagged writes are journaled as `leak_flagged`
audit events (with the txn id), so promotion to the refuse tier is a
measurement (#1044-style) rather than an argument. Refusals are journaled
as `leak_refused`, and a write that used its `allow_secrets` override is
`leak_allowed` — kept distinct because "heuristic noise" and "deliberate
disclosure-shaped act" are different questions to count. Dry runs journal
none of these — nothing was attempted — but the checks **run** on dry
runs, because predicting the real outcome is what `dry_run` is for. In
the rows, `path` is the write *target*; `key` carries the source key when
the digest tier knows it, else the detector's disclosable detail (prefix,
armor label, shape description). The backfill deliberately skips `leak_*`
rows: their target path must never become a claimed *location* of the
secret.

Extra tightening the tests forced on 011's `looks_secret_shaped` (leak
context only; the comment-redaction behavior is untouched): the token
must contain a digit and not be a bare UUID — otherwise every long
snake_case identifier and every UUID pasted into a doc warns, which is
precisely the noise that teaches agents to scroll past warnings. A UUID
that IS a known secret still refuses via the digest tier.

## D-4 — The known-digest index is a journal table fed by what kaed already sees; no walk, no plaintext

`secret_digests(digest, root, path, key, first_seen)` in journal.db —
digests only, above-floor only (PD-2's withholding rule applies to the
index exactly as everywhere else), locations included so a refusal can
name the variable to reference instead. Populated from:

1. **Backfill on open** (one-time migration sweep): placeholders parsed
   out of retained *redacted* blobs, joined back to their root/path;
   plus every digest already in `secret_events`.
2. **Every redacted blob journaled** — a write to a classified file
   indexes its values' digests as a side effect.
3. **Every secret event** — generate and rotate index old and new.
4. **Redacted reads** — serving a classified dotenv read (or its `stat`
   -adjacent typed view) records the placeholders' digests.

Read-capture is the load-bearing coverage: the hand-provisioned token
kaed never wrote (the klams token installed by k-homelab) enters the
index the first time any agent reads its file, which in practice happens
long before anyone pastes the value somewhere wrong. It is **not** read
journaling — no author, no event, no timestamp semantics beyond
`first_seen`, nothing in any journal stream — so 009 D-2's deliberate
gap (reads are not journaled) is untouched, and no plaintext shadow is
created, so 008 D-11/#1051 stay closed.

**The honest limit, stated rather than implied:** the precise tier's
coverage is "secrets kaed has seen" — read, written, minted, or
journaled — not "secrets on this host". A value that never passed
through kaed falls to the heuristic tier. No filesystem walk runs on the
write path to close that gap; `occurrences`' walk stays a deliberate
per-call act.

Staleness is a feature: a rotated-away value's digest stays indexed, and
writing an *old* secret into a README is still a leak worth refusing.

## D-5 — One knob: `[secrets] leak_checks = "refuse" | "flag" | "off"`, default `"refuse"`

The measured-rollout lever, same stance as `allow_reveal` (011 D-1):
per-call escape is `allow_secrets`, host-wide policy is config, and a
host where the precise tiers misfire in practice can be downgraded to
`"flag"` (everything warns, nothing blocks) without a redeploy —
because the alternative to a lever is agents learning to route around
kaed, which loses the journal too. `"off"` exists for completeness and
disables scanning entirely. The default is the designed behavior:
precise refuses, heuristic flags.
