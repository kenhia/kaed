# kaed

**Ken's Agent Editor** — an editor whose only user is an AI agent.

kaed is a Rust daemon that exposes reading, searching, and editing files as an
HTTP [MCP](https://modelcontextprotocol.io) server: versioned reads, atomic
multi-file edit transactions, structured conflicts instead of silent
corruption, and a durable attributed journal. It exists first for **remote**
editing — an agent on one machine editing files on another, where ssh-piping
and network mounts fail in quiet, expensive ways — with a long-shot second act
as a local power tool.

There is no human UI, and there is not going to be one. Every design decision
optimizes for the agent as the sole user: no cursor, no viewport, no undo
stack — but every response carries the exact information an agent needs to act
on it without a verification round-trip.

---

> ## ⚠️ Use at your own risk — early beta
>
> kaed is a **network service that writes to your filesystem**, gated by a
> bearer token. It is at an **early beta** level of maturity:
>
> - It is dogfooded daily by its author, on a handful of machines on a
>   private network. It has been run by essentially nobody else, and it has
>   not been audited.
> - **Expect breaking changes** — to the config format, the MCP tool contract,
>   and the on-disk journal schema. Version numbers track sprints, not a
>   stable API.
> - **Do not expose it to an untrusted network.** It binds loopback and
>   expects to sit behind something that handles transport security and access
>   control (the reference deployment uses `tailscale serve`).
> - Its safety features are **blast-radius reduction, not an access-control
>   boundary**. Any agent with a shell can read what kaed refuses to serve.
>   Read [SECURITY.md](SECURITY.md) before deploying it anywhere you care
>   about — it says plainly what kaed does and does not defend against.
>
> If you run this and it eats something you needed, that is a risk you took.

---

## What works today

Twelve tools over streamable HTTP with per-agent bearer auth:

| tool | what it does |
|---|---|
| `roots` | the workspace roots this fleet serves — peers probed live, an unreachable host reported as data |
| `stat` | metadata + content version — the cheap staleness probe |
| `list` | directory entries, gitignore-aware, paginated |
| `read` | whole file, a line range, or a window around a line or unique anchor |
| `search` | ripgrep-grade, every hit carrying its file's version; root patterns (`*:*`) search the whole fleet in one call |
| `edit` | anchor/range replace + create + typed dotenv ops; multi-file, atomic, `dry_run` |
| `secret` | the secret lifecycle without disclosure: describe (a durable handle), generate, rotate, occurrences |
| `secret_reveal` | the escape hatch — its own tool so it can be permissioned separately; always journaled |
| `journal` | what happened here: applied writes, failed attempts, friction reports and secret-audit events, merged |
| `diff` | any two states of a file — a version, a transaction, or the working tree |
| `revert` | undo a transaction as a new transaction; never history rewriting |
| `feedback` | tell kaed it got in your way; one required field |

The bet underneath them is **verified writes**:

1. Every response carrying file content also carries a `version` — the BLAKE3
   hash of the bytes served.
2. Every mutation declares the version of each file it touches. If the file
   moved on, you get a structured `version_conflict` carrying a diff of what
   changed since you looked — never a wrong edit.
3. The response to a successful edit contains the unified diff that was
   applied. That diff is the proof. **No verification read.**

A version is a content address, not a session handle, so it never expires: an
agent resuming after a crash or a context compaction can edit straight from a
version it recorded an hour ago and either succeed or get a precise conflict.

Every applied transaction — and every *failed* attempt — is journaled with the
identity that made it and an optional `intent` note, and that history is
readable **through the same contract**: `journal` merges writes, failures and
friction reports into one stream, `diff` reconstructs any version still
retained, and `revert` undoes a transaction as a new transaction (running
through the same version checks, so it conflicts rather than forces). Each
`journal` response also states what the history cannot see — reads are not
journaled — because a partial answer that looks whole is worse than no answer.

Secret-bearing files (`.env` and friends) are **classified, not denied**:
`read` serves them redacted — each value becomes a sealed
`⟨kaed:KEY@digest⟩` placeholder, line-for-line with the raw file — and
`edit` takes typed env ops (`env_set`, `env_rename`, `env_delete`,
`env_reorder`) where passing a placeholder as a value writes the real value
back. The agent edits a secrets file without ever holding a secret. Diffs,
conflict deltas, search hits and journal blobs are redacted too; destroying
a value must be declared (`drop_keys`); and a gitignore-shaped
`.kaedignore` (or an in-file `# kaedignore` marker) opts paths out of kaed
entirely.

The `secret` tool runs the whole lifecycle on the same terms: kaed mints
values server-side from a closed shape grammar (`hex(64)`,
`base64url(43)`, `uuid4`, `prefixed(tag,inner)`), so an agent can create
and rotate a token it has never seen — and `describe` returns a durable
**handle** (root + path + key + content digest) that a later session, even
on another host, can hand to `edit` as `value_from` to write the value by
reference: across hosts the bytes move between kaed instances, never
through the agent's context. Every generate, rotate, reveal and transport
lands in a secrets audit stream (`journal` kind `"secret"`), which is what
makes "has any agent ever seen this token?" an answerable question.
Revealing plaintext exists but is deliberately its own tool, one key at a
time, with a required `intent` and a host-wide off switch.

Any instance can also be its fleet's **gateway**: declare peers with URLs
and per-identity tokens, and calls addressing another host's roots are
proxied there — as the caller, never as a shared "gateway" identity, so
journal attribution on the target is identical to a direct call. Errors pass
through verbatim (a `version_conflict` delta survives the hop), a peer that
stops answering becomes `status: "unreachable", since: …` — data, not a
connection failure — and each host's own URL keeps working as the fallback.
See "Gateway mode" in [docs/setup.md](docs/setup.md).

**Deliberately absent:** no exec/shell tool, and no git tool. An agent that
can already run commands does not need kaed to run them, and keeping them out
is what lets the security story be stated in one paragraph. See
[docs/overview.md](docs/overview.md) for the reasoning.

## Documentation

- **[docs/overview.md](docs/overview.md)** — why kaed exists, how it works,
  what is deliberately excluded, where it's going.
- **[docs/setup.md](docs/setup.md)** — deploying it yourself: build, config,
  token, systemd unit, remote access, client wiring, rotation. Includes a
  section you can hand to your own agent to do the install.
- **[deploy/](deploy/)** — the install itself: an idempotent `install.sh`
  (re-running it is the upgrade path — building from the checkout, or
  fetching a published, checksum-verified build with `--from-store`), the
  systemd unit, a config template, and token mint/rotate. It never overwrites
  a config and never touches a token.
- **[docs/kaed-explained.html](docs/kaed-explained.html)** — a single-page
  visual explainer
  ([rendered preview](https://htmlpreview.github.io/?https://github.com/kenhia/kaed/blob/main/docs/kaed-explained.html)).
  Self-contained; it renders offline straight from a checkout too.
- **[SECURITY.md](SECURITY.md)** — threat model, stated honestly.
- **[sprints/](sprints/)** — the development history: planning docs, the MCP
  contract, and one record per sprint.

## How this project is built

Worth stating up front, because it explains the shape of everything else in
this repo.

Ken and an AI agent have a design conversation — sometimes long, sometimes
adversarial — until the shape of the work is clear. Then Ken hands
implementation over. The agent makes the design and implementation calls,
writes the code and the tests, deploys it, verifies it against the live
service, and writes the sprint record explaining what it decided and why. Ken
advises and reviews; he is not the one typing.

So:

- **The sprint records are primary sources**, not summaries written after the
  fact. `sprints/NNN-name/` holds the reasoning as it happened, including
  options that were rejected and the occasional place where the plan turned
  out to be wrong. `decisions.md` files exist where a call was genuinely
  contested.
- **Commits are co-authored** by the model that wrote them.
- **The docs are written for an agent as much as for you.** The MCP tool
  descriptions and the server instructions are part of the product, and get
  reviewed as carefully as the code.

This is an experiment in giving an agent real ownership of a tool that agents
themselves use. The thing being built and the way it's being built are the
same bet.

## Development

Uses the [kproject](https://github.com/kenhia/kprojects) minimal harness.

```sh
just                          # list recipes
just check                    # CI gates: fmt --check, clippy -D warnings, tests
cargo run -- check-config     # validate config, print roots + deny/classify rules
cargo run -- serve            # run the daemon
```

kaed lives alongside the other homelab MCP services it was built next to
(klams for memory, korg for work items): same transport conventions, same
per-agent bearer-token auth.

## License

MIT — see [LICENSE](LICENSE).
