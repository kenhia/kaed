# Brainstorm — secrets-aware editing

*Status: brainstorm, nothing decided. 2026-08-02, from a session with Ken.*
*Context: follow-on to korg proposal #916 ("kaed 002 — blast-radius hardening"),
and it bears directly on WI #909 (journal blobs retain full file content).*

Ken's framing: since we control the editor, we can do more than put up guards —
we can make editing secrets **safer in general**. Two starting ideas:

1. A `.gitignore`-shaped `.kaedignore` plus an in-file `# kaedignore` marker,
   either of which blocks kaed from returning or editing a file.
2. Special handling for `.env`-shaped files: **redact + restore** (return keys
   and comments, replace values with placeholders the agent can ask about) and
   **blind edit** (for known token shapes, let the agent ask kaed to generate
   and write a value it never sees).

---

## First, the honest frame

Any agent with a shell can `cat .env`. So none of this is an access-control
boundary — it is **blast radius + ergonomics**, exactly the frame #916 already
uses. That sets the complexity budget: features that make the *common,
well-intentioned* path safe are worth a lot; features that try to stop a
determined or injected agent are worth near zero. It also means every refusal
must say *what to do instead*, or the agent routes around kaed via ssh and we
lose the journal too.

Second frame: kaed sees **every write**, not just every read. Both starting
ideas are read-side. The higher-frequency real-world incident is the opposite
direction — a secret getting *written* into a test fixture, a README, a korg
comment, a commit. See "Things not on the list" below.

---

## `.kaedignore` + `# kaedignore`

Both cheap and worth having, and they are not a new mechanism — they are a
project-scoped, in-repo extension of the resolver denylist #908 already put in
`fsops.rs`. One choke point, same refusal codes. Two invariants to nail down:

**Policy files can only restrict, never relax.** Server config > `.kaedignore`
> in-file marker, and each layer may only *add* denials. Otherwise the first
thing a compromised agent does is write `!~/.ssh` into a `.kaedignore`.
Corollary: `.kaedignore` must be implicitly unwritable through kaed.
gitignore-style `!` negation is fine, but only to un-deny something *that same
file* denied.

**Distinguish deny from classify.** Heuristics (`.env*`, `*.pem`, `id_*`,
`credentials*`, `*.kdbx`) should *classify* → redact. Only explicit config
should *deny*. A heuristic hard-deny produces confusing dead ends; redaction
degrades gracefully.

On the in-file marker specifically — worth taking, with one caveat to be
deliberate about: it is **in-band signaling**, file content controlling server
policy. A vendored or fetched file containing that string silently becomes
unreadable. Low severity, but it is the same shape as prompt injection.
Mitigations: only honor it in the first ~5 lines, and always return a
structured `reason` (`server_denylist` | `kaedignore` | `in_file_marker`) so
the agent does not burn turns guessing why a read failed.

---

## Redact + restore — the one worth building

Details that decide whether it works:

**Placeholders must be stable and HMAC'd, not hashed.**
`⟨kaed:ANTHROPIC_API_KEY@a3f9⟩` where `a3f9` is a truncated HMAC under a
server-held key. Bare hashes of low-entropy values (`POSTGRES_PASSWORD=postgres`,
`DEBUG=true`) are brute-forced instantly. Stability buys three things for free:
change detection across reads, **equality across files** (is `.env.prod`'s token
the same as `.env`'s?), and the rotation-propagation story below.

**Placeholders are sealed handles in writes too.** The agent may reorder lines,
add keys, rename keys, edit comments, and pass placeholders through verbatim;
kaed substitutes real values back on write. That preserves the whole
versioned-edit contract — *provided* we also redact **the returned diff, the
`version_conflict` delta, and the journal blob**.

> **Sequencing trap.** A redacted read surface is worthless if `journal.db`
> holds plaintext and the planned journal/diff/revert read tools expose it.
> #909 is resolved, but its blob-retention decision was made without this
> feature in view. **Decide the secrets model before the journal read tools
> ship**, or we ship the leak and the guard in the same quarter.

**Make `reveal` its own tool, not a flag on `read`.** Harness permissioning is
per-tool. `kaed__read` gets allowlisted on day one; `kaed__secret_reveal` keeps
prompting. That single structural choice does more real work than any policy
logic inside kaed. Journal every reveal with intent, and return a
`disclosed: true` marker the agent is instructed to surface to the human.

**Typed surface for dotenv, redacted-text for everything else.** For `.env`,
return structure (`{key, comment, placeholder, meta:{len, shape}}`) and take
typed ops (`set`, `rename`, `delete`, `reorder`). Text edits over redacted text
have ugly failure modes — an anchor landing mid-placeholder, a truncated
placeholder, an agent writing a literal that looks like one. dotenv parses
trivially; make those unrepresentable. Keep redacted-text as the general
fallback and do not chase generalized YAML/TOML redaction until something
demands it.

**Guard against silent destruction.** If a write to a classified file makes a
placeholder *disappear*, refuse unless the agent declared `drop_keys: [...]`.
Otherwise the agent silently destroys a secret it never saw and cannot restore.

**Open question worth testing rather than arguing:** how often does the agent
genuinely need plaintext? Usually it needs to know a key exists, add one, or
*use* it in a command — and the last is solved by convention
(`set -a; . .env; set +a`, then reference `$KLAMS_TOKEN`), not by revealing.
Have the read response carry that hint. Ship redaction **without** `reveal`
first and see how much pressure actually materializes.

---

## Blind edit — generalize it

Ken's version: detect shape, let the agent supply the tag, kaed generates.
Generalize into three verbs and one invariant.

**Invariant: the non-random parts of a shape are disclosable; the random parts
never are.** Clean line, easy to implement, easy to explain.

- `describe(key)` → `{shape: "prefixed-hex", prefix: "klams", hex_len: 64}` —
  no value.
- `generate(key, shape, parts)` → mints, writes, returns only a placeholder +
  metadata. **The agent never holds a secret it created.** Shapes come from a
  named server-side registry (`hex(32)`, `base64url(32)`, `uuid4`,
  `prefixed:{tag}-hex32`, `passphrase(4)`), project-overridable — not
  free-form, so this does not become an eval-shaped feature.
- `rotate(key)` → same shape, new entropy. **Rotate a token without ever seeing
  old or new.**

`rotate` is where the HMAC placeholders pay off for this homelab: klams tokens
are per-agent and live in several places. Placeholder equality across files
lets kaed say *"this value also appears in 3 other files under these roots —
rotate all?"* That is probably the single most-wanted feature here.

---

## Things not on the list

Ranked by value per line of code:

1. **Write-side leak detection.** kaed sees every write. Refuse (or flag)
   content matching a known secret's HMAC, a provider prefix (`sk-ant-`,
   `ghp_`, `AKIA`), or a high-entropy shape — *especially* into a file not
   classified as secret-bearing. Shares the entire shape registry + HMAC index
   with the read side. This catches the incident that actually happens.
2. **Gitignore check.** kaed knows the path and can read `.gitignore`. Warn on
   read/write of a classified file that is not ignored. One line of code,
   catches the actual disaster.
3. **`.env` ↔ `.env.example` sync.** Keys and comments, values stubbed.
   Trivially safe, and it is precisely the operation where an agent today
   copies a real value by accident.
4. **Run the shape detector over comments, not just values.** `.env` files are
   full of `# old token: abc123`.
5. **Secrets audit stream in the journal** — every reveal / generate / rotate /
   refused-write. Then "has any agent ever seen the klams token?" is answerable.

---

## Suggested build order

- **Slice 1** (pure policy, no new tool surface): `.kaedignore` + in-file marker
  + structured refusal `reason` + gitignore warning.
- **Slice 2**: classification + redacted dotenv read + placeholder-preserving
  writes + **redacted diffs, deltas, and journal blobs**. No `reveal`.
- **Slice 3**: separate `kaed_secret` tool — `describe` / `generate` / `rotate`
  / `reveal`, plus the shape registry.
- **Slice 4**: write-side leak detection + cross-file occurrence index →
  rotate-everywhere.

---

## Open questions to settle early

- Where the HMAC key lives, and whether it survives restart. It must —
  placeholder stability depends on it. Rotation of *that* key is its own
  question.
- Does `search` redact its hits? (Yes.) And what does a search whose *pattern*
  matches token shapes return?
- Placeholder consistency when the same file is reachable through two roots.
- Whether kaed keeps a recoverable shadow of prior values for classified files.
  A strong "we control the editor" win — an agent-caused secret loss becomes
  recoverable — but directly in tension with #909's blob-retention decision.
  Pick one deliberately.
