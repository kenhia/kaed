# Sprint 027 — decisions

Covers korg WI 2372, under proposal korg:4424, a slice of program korg:4368.

## D-1 — Reuse means one tier decision, called from two places

The overseer's brief said to reuse the crate code and not fork the patterns.
`redact` therefore calls 012's tokenizer (`is_token_char`, `MIN_TOKEN_LEN`,
`variants_of`), its classifiers (`provider_prefix`, `shaped_for_leak`,
`clears_floor`, `digest_of`) and 008's `placeholder`. The one piece of
`scan` that was inline, the strongest-tier choice, became `index_free_tier`,
and both callers now use it. It returns *which variant* hit, so that
`redact` replaces exactly that span: `KLAMS_TOKEN=<value>` keeps its key.

Two things differ on purpose, because the input differs:

- There is no "old content", so every token is a candidate. 012 D-1's
  newly-introduced rule is about edits.
- Armor is found anywhere in a line, not only as a whole line, because tool
  output is often JSON with the key's newlines escaped as `\n`.

## D-2 — An armor label is uppercase words; a key block ends where key material ends

Measured: 7 of 11 armor hits were regexes or prose *about* armor. With no
END marker, the first cut ran each one to the end of the output and ate up
to 6.5 KB. Two rules fix it:

- The label must be `[A-Z0-9 ]*PRIVATE KEY`. This is shared with `scan`; see
  "Repaired in passing" in the record.
- The block runs line by line through base64 lines and the legacy
  `Proc-Type:`/`DEK-Info:` headers, ending after the matching END marker.
  A prose mention has no key lines, so only its header is replaced.

A truncated real key still loses every key line it has.

## D-3 — Prefixed tokens under 20 characters stay in tool output; the write path is unchanged

Measured: 15 of 22 distinct prefixed matches were docs naming a prefix, or an
agent's own fingerprint, and all of them were under 20 characters. Every
real credential was 58 or more. The floor is `MIN_PREFIXED_REDACT_LEN`,
applied in `redact` only:

- On the write path, a false refusal costs an `allow_secrets` override.
- In tool output, a false redaction costs the reader the very string they
  were looking up, with no override available.

The thresholds differ because the costs differ, not because the pattern
does. AWS access-key IDs are exactly 20 characters and stay redacted.

## D-4 — The hook fails open

On unparseable input, an unreadable journal or a stdin error, the hook
prints nothing (so the original output goes through) and notes the failure
on stderr. A missing index degrades to the index-free tiers. The
alternative, blocking the tool result, turns any hook bug into "every Bash
call is broken", and a hook like that gets uninstalled the same day,
protecting nothing. A spike is not the place to argue for fail-closed. If
rollout wants it, that is a decision for the rollout.

## D-5 — Recommend `Bash|PowerShell` only; not `Read`, `Grep` or `Edit`

The mechanism works for every tool, and the hook would happily run on
`Read`. The reason not to match it is not about the hook. An agent edits
files using the text that `Read` showed it. If `Read` shows a placeholder
where a fixture is, the `Edit` `old_string` built from it matches nothing,
and the edit fails for reasons the agent cannot see. 11 of the 30 touched
results in the corpus were exactly that: `Read`s of kaed's own test fixtures.
Shell output is read, not edited from, which is why WI 2372 asked about
shell tools in the first place.

## D-6 — The `secret_shaped` tier is off, and the measurement is the reason

012 D-3 kept the heuristic to a warning on the write path for the same
reason. This sprint puts a number on it: 8.6% of all tool results touched,
mostly git SHAs and sha256 ids, at about 0.2% precision. `--shaped` exists
only so the measurement can be reproduced.
