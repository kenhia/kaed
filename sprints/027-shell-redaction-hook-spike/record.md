# Sprint 027 — redaction-only hook for shell tool output (spike)

**Proposal:** korg:4424 · **Covers:** WI 2372 · **Program:** korg:4368
(backlog drawdown, November 2026) · **Branch:** `027-shell-redaction-hook-spike`

Run as a karc leg (`kaed-a94903`) on kai under an overseer.

## Goal

WI 2372 asks whether kaed's redaction can be applied to what *shell* tools
show an agent, so that `cat .env` or `docker inspect` stops putting live
tokens into transcripts. Ken's answer (2026-11-09) narrowed it to a spike:
a `PostToolUse` hook that reuses kaed's classifiers and returns
`updatedToolOutput` with sealed placeholders. **Redaction only — no recall
service, no reveal.** The deliverable is a measurement and a
recommendation, not a rollout: the hook is proven in an isolated `claude -p`
run with its own settings file and installed nowhere.

## What shipped

- **`leak::redact`** (`src/leak.rs`) — the rewriting twin of 012's
  `leak::scan`. Same tokenizer, same `variants_of` (so `KEY=value` loses the
  value and keeps the key), same tier order (known digest → provider prefix
  → high-entropy shape). The index-free tier decision is now one helper,
  `index_free_tier`, which `scan` and `redact` both call, so the patterns
  are shared rather than copied. Placeholders are 008's
  `⟨kaed:LABEL@digest⟩`. The label is the variable name when the index knows
  it (`⟨kaed:KLAMS_TOKEN@…⟩`), and otherwise the prefix or `private_key`.
  The digest is withheld below the 80-bit floor (PD-2), as everywhere else.
- **`src/hook.rs`** — `load_index` reads `secret_digests` from the host's kaed
  journal **read-only**. `respond` walks the `tool_response` it was handed
  and rewrites strings in place, so the result keeps the tool's exact shape.
  That matters because Claude Code silently drops an `updatedToolOutput`
  whose shape doesn't match the tool's result.
- **`kaed-redact-hook`** (`src/bin/`) — the binary Claude Code runs. It reads
  stdin and prints `updatedToolOutput` only when it redacted something. It
  **fails open**: bad input or an unreadable index leaves the output alone
  and says so on stderr (D-4). `--shaped` turns on the heuristic tier, and
  the default is off. It is **not** in the deploy bundle: `just publish`
  still ships only `kaed`.
- **`examples/redact_measure.rs`** — the measurement harness (below). It never
  prints a value. It reports counts and shapes, plus *skeleton* context in
  which the match and every token that could be a credential become
  `«shape:len»`.
- **`tests/redact_hook.rs`** — the process contract: a secret comes back
  sealed in the right shape, clean output prints nothing, garbage input
  fails open.

## The measurement

**Corpus:** every tool result in the kagviz live mirror for kai, kubs0 and
cleo (`/ai-data/kagviz-data/live/*/projects`, read-only). That is 1,613
transcripts, **157,913 tool results** after de-duplicating resumed sessions
by `tool_use_id`, and 380 MB of output. The span is the mirror's whole history: it never
prunes, starts before 2026-08-23, and runs to 2026-11-09. Bash accounts for 100,395 results, PowerShell 610, Read 4,918,
and the korg MCP 23,462.

**Two oracles, both digest-only:**

- **Known-digest index:** kai's journal holds **19** digests.
- **Committed set:** the digests of everything `redact` would replace in any
  git-tracked file under `~/src` on kai (62 repos, 22,935 digests). A hit in
  this set is a fixture or published example, not a live secret.

Run: `cargo run --release --example redact_measure -- --journal
~/.local/share/kaed/journal.db --committed ~/src --out .scratch/measure.json
/ai-data/kagviz-data/live/*/projects`. It takes about 9 seconds.

### Precise tiers (the hook's default), after D-2/D-3

**30 of 157,913 tool results touched (0.019%)**: Bash 17, Read 11,
PowerShell 1, kaed MCP 1.

| class | hits | distinct | real secret | fixture (committed) | not a secret |
|---|---|---|---|---|---|
| known digest | 33 | 5 | 4 — `ANTHROPIC_API_KEY`, `KLAMS_AUTH_TOKEN`, `REDISCLI_AUTH` (also as a redis `requirepass`), a rotation script's `NEW` | 1 — `SMOKE_TOKEN` (28 of the 33 hits) | 0 |
| provider prefix | 12 | 7 | 2 — a `github_pat_` in decrypted vault output; cleo's `claudeAiOauth.accessToken` (`sk-ant-`, 108 chars, via PowerShell) | 4 | 1 — an agent printing a 21-char prefix of a key as a fingerprint |
| private-key armor | 4 | 2 | 0 | 2 (header only) | 0 |
| **total** | **49** | **14** | **6** | **7** | **1** |

**False positives among the precise tiers: 1 of 14 distinct values (2% of
hits) is not a secret at all.** Counting fixtures as false positives too
gives 8 of 14, but redacting a fixture is harmless in Bash output. It is not
harmless in Read output: see D-5.

### Before D-2/D-3, with the shared rules exactly as 012 wrote them

- **Provider prefix:** 41 hits and 22 distinct values, of which 15 were
  documentation naming a prefix (`sk-ant-admin01-…` in API docs fetched by
  WebFetch) or agents' own deliberate fingerprints (`first 12 chars`). Every
  one was under 20 characters, and every real credential was 58 or more.
- **Armor:** 11 hits, and 7 of them were prose or regexes *about* armor
  (`BEGIN [A-Z ]*PRIVATE KEY`, `BEGIN … PRIVATE KEY`). With no END marker,
  each one swallowed the rest of the output: 893 to 6,517 bytes of korg
  comments and kaed source vanished per hit. That was the one failure that
  actually damaged output.

### The heuristic tier (`secret_shaped`)

- **39,760 hits, 17,334 distinct values, 13,603 tool results touched (8.6%
  of all; 9,339 of 100,395 Bash results).**
- By length: 18,386 are 32–39 characters, 2,654 are exactly 40 (1,987 of
  them hex, i.e. git SHAs), 4,150 are exactly 64 (4,017 hex: sha256 and
  klams/korg ids), and the rest are base64 ids, URLs and image data.
- The only real secrets it adds are the ~35 in the next section. **Precision
  is about 0.2% by distinct value.**

### False negatives: what leaked that the precise tiers missed

There is no ground truth for "every secret in the corpus", so this is a
probe, not a census. It looks for values assigned to a credential-named key
(`token`, `secret`, `password`, `api_key`, `bearer`, `authorization`, …)
with at least 50 bits of estimated entropy and a digit, which the precise
tiers left in place.

| class | hits | distinct | what it is |
|---|---|---|---|
| high-entropy, credential-keyed | 96 | 46 | Reviewing all 46 skeletons: **~35 real credentials**. They are klams `[[auth.tokens]]` entries (~16), MCP config `Authorization: Bearer` headers (~6, including the pre-024 kaed bearers), `*_TOKEN=` lines in dumped `.env`s, a vllm API key, an MCP-inspector proxy token, and **cleo's Claude Desktop `oauth:tokenCache` (2,864 and 3,140 characters, via PowerShell)**. **~9 are not secrets**: wikimedia and media `?token=` URLs, a public map token, a test fixture, an endpoint URL. The remaining ~2 are unclear. |
| above the floor but not shaped | 229 | 48 | Overwhelmingly not secrets: `token_file = "~/…"` paths (87 contain `/`), `tokenizer='…'` model names, `secrets_from` timestamps. |
| below the floor | 127 | 24 | Agents' own 12-character digest fingerprints, `tokens = tokens[1:]`-style code. |
| known digest, value split by the tokenizer | 0 | 0 | No indexed secret escaped by containing `/` or other non-token characters. |

**So the precise tiers caught 6 real secrets and missed about 35: recall is
roughly 15%** against what this probe can see. The misses are not shape
failures. They are **unprefixed random tokens kaed has never seen**: the
homelab mints its own credentials, and kai's index holds 19 digests, of
which klams, korg and the MCP configs have contributed almost none.

### The isolated live proof

Run in `.scratch/hook-proof/` (gitignored) with Claude Code 2.1.308:

`claude -p --settings ./settings.json --allowedTools "Bash(cat:*)"`

The `settings.json` there registers only `PostToolUse` / `Bash` →
`kaed-redact-hook`. No live settings file on any host was touched. The
prompt was `cat fake.env`, a file holding a freshly generated, never-used
`ghp_` token, and the model was told to echo stdout verbatim.

- The model's reply was `FAKE_GH_TOKEN=⟨kaed:ghp@68af9e687ca077df⟩`. The
  token was not in it.
- **This answers the question comment 3218 left open: the session `.jsonl`
  stores the REPLACEMENT.** The token appears nowhere in the session file.
  Both `message.content[].content` (what the model saw) and
  `toolUseResult.stdout` (the structured result) hold the placeholder.
  Downstream mirrors of `~/.claude/projects`, such as kagviz, inherit the
  redaction. Per the hooks docs, OpenTelemetry tool spans still capture the
  original.
- **Latency:** under 2 ms per call, including opening the journal for the
  index, for both a 5-byte and a 400 KB result. The whole 380 MB corpus,
  with every tier, ran in 9 seconds.

## Recommendation — **adopt, with these patterns only**

**Install `kaed-redact-hook` with the precise tiers, matching
`Bash|PowerShell` only.** It costs nothing measurable: 30 results touched
in 158k, and one non-secret value redacted in two and a half months. It
would have kept six real credentials out of transcripts, including cleo's
Claude OAuth access token. Specifically:

- **Tiers:** known digest, provider prefix (20 characters or longer), and
  private-key armor. **Never** the `secret_shaped` tier: 8.6% of all tool
  results touched, at 0.2% precision.
- **Matcher:** `Bash|PowerShell` only, **not** `Read`, `Grep` or `Edit`. 11
  of the 30 touched results were `Read`s of kaed's own test source, and an
  agent that reads a placeholder in place of a fixture will build an
  `Edit.old_string` that matches nothing (D-5). That is exactly the
  file-editing breakage kaed exists to prevent. MCP results are left out too
  (1 touched in 23k korg/klams results), because kaed already classifies its
  own reads.
- **Say plainly what it does not buy.** It closes about **15%** of the
  credential exposure this corpus shows. The other 85% is unprefixed
  homelab tokens. Adopting the hook must not be read as "transcripts are now
  clean".

Two follow-ons would move that 15%. Each is a decision rather than a repair,
so they are described here for the overseer to route and not filed:

1. **A credential-context tier:** a high-entropy value *assigned to a
   credential-named key*, which is exactly the probe above. Measured: about
   +35 real credentials for about 9 non-secret false positives over 158k
   results, roughly 80% precision against the shaped tier's 0.2%. It is a
   new pattern with no counterpart in kaed's write path, so whether kaed
   owns it (and whether 012's `scan` should gain it too) is a design call.
2. **A wider digest index.** The known-digest tier is the only one with no
   false positives, and it is starved: 19 digests on kai, none on cleo
   (which has no kaed). Feeding it the digests of every credential `krot`
   registers would close most of the 35 by name (`⟨kaed:KLAMS_TOKEN@…⟩`).
   That is a contract between kaed and krot, and needs krot's owner.

Rollout itself, if adopted, is per host: a `PostToolUse` entry in each
host's user settings. cleo needs a Windows build and gets no known-digest
tier until (2) exists. That routing is the overseer's.

## Repaired in passing

- **`leak::scan` refused on nonsense armor labels.** The shared
  `private_key_armor` accepted any `-----BEGIN <anything>PRIVATE KEY-----`
  line, including a regex. Both `scan` and `redact` now go through
  `is_private_key_label` (uppercase words only). This narrows the write
  path's refusal to real labels and widens nothing. Gate: `just check`
  (`prose_and_regexes_about_armor_swallow_nothing_after_them`, plus the
  existing `a_private_key_block_refuses`).
- **`config::default_journal_path`** is now public, so the hook reads the
  same default location as the daemon instead of re-deriving it.

## Follow-ups

None filed, per the overseer's brief. The two follow-ons above and the
rollout are for the overseer to route.

## Deployed

**Nothing was deployed, deliberately.** The overseer's clearance (korg:4424
comment 4495) was "merge only": no kaed restart or redeploy while other legs
are using it, the same call 026 made. Merged as `2f2aeb1` (PR #33) on
2026-11-10. `deploy-fleet` was not run, so the fleet stays on its previous
build. The armor-label narrowing in `leak::scan`, the only daemon-behaviour
change, ships with the next kaed deploy. `kaed-redact-hook` is not in the
bundle and is in no live settings file on any host; whether to roll it out
is Ken's decision on the recommendation above.
