# `kaed-redact-hook` — sealing secrets in shell output

kaed redacts secrets on everything it serves. Shell commands bypass it
completely: `cat .env`, `docker inspect`, `env` or a verbose `curl` print
values straight into an agent's context and from there into the session
transcript. `kaed-redact-hook` is a Claude Code
[`PostToolUse`](https://docs.claude.com/en/docs/claude-code/hooks) hook. It
runs kaed's own leak detectors over a shell tool's output and replaces
anything they recognise with the same sealed placeholder kaed uses
everywhere else, `⟨kaed:LABEL@digest⟩`, before the model sees it.

## What it catches, and what it does not

**Read this section before relying on the hook.** Only the *precise* tiers
are on:

| tier | matches | false positives measured |
|---|---|---|
| known digest | any value whose digest kaed has already seen in a classified file on this host | none |
| provider prefix | `ghp_`, `github_pat_`, `sk-ant-`, `AKIA`, `AIza`, … tokens, 20 characters or longer | 1 in about 158,000 tool results |
| private-key armor | `-----BEGIN … PRIVATE KEY-----` blocks | none |

Measured over about 158,000 real tool results from three hosts (sprint 027),
these tiers caught **about 15% of the real credentials that leaked**. The
misses were tokens with no recognisable prefix that this host's kaed had
never seen: bearer headers, `*_TOKEN=` lines and service tokens in config
files. Treat the hook as a partial guard. It is not a guarantee that
transcripts are clean.

Deliberately **not** on:

- **The high-entropy heuristic** (`--shaped`). It touched 8.6% of all tool
  results at about 0.2% precision. Almost everything it hit was a git SHA or
  a checksum.
- **`Read`, `Edit` and `Grep`.** A placeholder in `Read` output becomes an
  `Edit` `old_string` that matches nothing in the file. kaed's own `read`
  already redacts the files it classifies.

The session `.jsonl` stores the **replacement**, not the original (measured
on Claude Code 2.1.308), so transcript mirrors inherit the redaction.

## Installing it

`deploy/install.sh` installs it next to the daemon as
`~/.local/bin/kaed-redact-hook`, from the same checksum-verified bundle and
held to the same published version. **Installing it does nothing on its
own.** Claude Code runs a hook only when a `settings.json` entry points at
it.

## Switching it on

Add this entry to the user's `~/.claude/settings.json`, alongside any other
hooks:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Bash|PowerShell",
        "hooks": [
          {
            "type": "command",
            "command": "/home/<user>/.local/bin/kaed-redact-hook",
            "timeout": 5
          }
        ]
      }
    ]
  }
}
```

- It **must not** be `async`. An async hook cannot replace the output.
- **Merge** the entry into the existing `PostToolUse` array. Do not replace
  the file: other tools keep their own hooks there. To switch the hook off,
  remove the entry.
- Changes take effect in **new** sessions.

## Failing open

On any error the hook exits 0 with nothing on stdout. The tool's original
output goes through, and the reason is on stderr. That covers input that is
not JSON, empty input, an unexpected result shape, and a journal that is
missing, corrupt or not readable. In that last case the known-digest tier is
skipped and the other two still apply. The process tests in
`tests/redact_hook.rs` pin this behaviour. Each call takes well under a
millisecond.

The known-digest index comes from the kaed journal at its default location,
opened **read-only**. Use `--journal PATH` if that host keeps the journal
somewhere else.

## Measuring what it misses

`just leak-inventory <out.json> <transcript dirs>…`
(`examples/leak_inventory.rs`) walks a transcript corpus with the same
detectors plus a credential-context probe. It reports fingerprints, key
names and counts per distinct value, and **never prints a value**. Use it
to answer "what is leaking that the hook does not catch?". It is not part
of the deploy bundle.
