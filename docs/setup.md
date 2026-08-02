# Deploying kaed

Getting kaed running on a host and wired to an agent. Written for a Linux host
with systemd; the daemon itself only needs a Unix-like OS and a writable
directory.

> **Before you start:** read the risk warning in
> [the README](../README.md) and the threat model in
> [SECURITY.md](../SECURITY.md). kaed is an early-beta network service that
> writes to your filesystem. The single most important decision below is
> [which roots you configure](#3-write-the-config) — get that wrong and you
> have handed an agent your entire home directory, which is exactly the
> mistake this project made on its first deploy.

If you would rather have an agent do this, skip to
[Handing this to an agent](#handing-this-to-an-agent).

---

## Prerequisites

- **Rust** (edition 2024 — 1.85 or newer; developed on 1.96).
- **A host you trust**, on a network you control. kaed binds loopback by
  default and expects a reverse proxy or tunnel to handle transport security.
- **systemd** if you want it to run as a service (optional but assumed here).
- **[Tailscale](https://tailscale.com)** if you want remote access the easy
  way. Any TLS-terminating reverse proxy works; `tailscale serve` is just the
  shortest path.

## 1. Build

```sh
git clone https://github.com/kenhia/kaed
cd kaed
cargo build --release
install -m755 target/release/kaed ~/.local/bin/kaed
kaed --version
```

Make sure `~/.local/bin` is on your `PATH`.

## 2. Create the token

One bearer token per agent identity. Generate a real one — this is the only
thing standing between the network and your files:

```sh
mkdir -p ~/.config/kaed
head -c 48 /dev/urandom | base64 | tr -d '/+=' | head -c 48 > ~/.config/kaed/token
chmod 600 ~/.config/kaed/token
```

The file's trimmed contents are the token. Never commit it, and keep it out of
any directory kaed serves — which kaed enforces for you, since its own config
directory is refused unconditionally.

## 3. Write the config

`~/.config/kaed/config.toml`:

```toml
[server]
bind = "127.0.0.1:4870"
# Host header values to accept beyond loopback. If a proxy fronts kaed, the
# proxy's hostname MUST be here — see the gotcha below.
allowed_hosts = []

# Roots are the only paths kaed can reach. Name them explicitly and narrowly.
[[roots]]
name = "src"
path = "/home/you/src"
description = "code repos"

[[roots]]
name = "scratch"
path = "/home/you/scratch"
description = "scratch space"

[auth]
# One entry per agent identity; the name is recorded on every journal entry.
claude = { token_file = "~/.config/kaed/token" }

[limits]
max_read_bytes = 262144       # per-response byte budget
max_file_bytes = 8388608      # refuse to read/edit files larger than this
search_max_results = 50       # default cap on search hits

[journal]
# Days of blob *content* retained. Transaction metadata is kept indefinitely.
retention_days = 7

[security]
# Extends the built-in defaults (.ssh, .gnupg, .aws, .env, *.pem, id_*, …).
# A path is refused if it or any ancestor matches.
deny = [
    "**/secrets/**",
    "**/*.key",
]
```

### Choosing roots — the one thing to get right

**Do not root at `$HOME`.** kaed's first deployment did, and the result was
that `read` on `~/.config/kaed/env` returned the bearer token that gated the
service, while `~/.ssh` sat one `edit` away from being rewritten. Name the
directories that hold work you actually want an agent editing.

The deny list is the second layer, not the first. It catches what slips
*inside* a root — a `.env` in a repo, a stray `*.pem` — and it protects kaed's
own config and journal directories no matter what. It is not a substitute for
drawing the roots narrowly.

### Config reference

| Key | Meaning |
|---|---|
| `server.bind` | Listen address. Keep it on loopback unless you know why not. |
| `server.allowed_hosts` | Extra `Host` header values accepted beyond loopback. **Required when proxied.** |
| `roots[]` | `name`, `path`, optional `description`. `path` may use `~/`. Canonicalized at startup; duplicates and denied roots are refused. |
| `auth.<identity>.token_file` | File holding the token. Re-read on `SIGHUP`. |
| `auth.<identity>.token_env` | Alternative: an env var. Works, but **cannot be reloaded** — see [rotation](#rotating-a-token). |
| `auth.<identity>.prev_token_file` | Grace-window token during a rotation. Requires `token_file`. |
| `limits.*` | Response and file size budgets. |
| `journal.path` | Defaults to `$XDG_DATA_HOME/kaed/journal.db`. |
| `journal.retention_days` | Blob content retention. Metadata is kept forever. |
| `security.deny` | Extra deny globs, matched against absolute paths. |
| `security.use_default_deny` | Set `false` to drop the built-in glob defaults. kaed's own directories stay refused regardless. |

### Validate it

```sh
kaed check-config
```

This prints the resolved roots, which identities resolved a token, the limits,
the journal path, and **every deny rule in force**. It exits non-zero on
anything invalid. Read the deny list it prints — that is the real one, not the
one you think you wrote.

## 4. Run it as a service

`~/.config/systemd/user/kaed.service`:

```ini
[Unit]
Description=kaed — agent editor MCP daemon
After=network.target

[Service]
ExecStart=%h/.local/bin/kaed serve
ExecReload=/bin/kill -HUP $MAINPID
Environment=RUST_LOG=kaed=info
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now kaed
systemctl --user status kaed
loginctl enable-linger "$USER"   # so it survives logout
```

`ExecReload` is what makes [token rotation](#rotating-a-token) non-breaking.
Include it.

## 5. Expose it (optional)

For an agent on another machine. With Tailscale:

```sh
sudo tailscale serve --bg --https=4870 localhost:4870
```

giving you `https://<host>.<tailnet>.ts.net:4870/mcp`.

> ### ⚠️ The gotcha that will cost you an hour
>
> The MCP server validates the `Host` header against an allowlist as a
> DNS-rebinding guard, and that allowlist is **loopback-only by default**.
> Behind a proxy the `Host` is the proxy's hostname, so requests are rejected
> **before auth even runs** — you get a 4xx that looks nothing like an auth
> problem.
>
> Put the external hostname in `server.allowed_hosts`, in **both** forms:
>
> ```toml
> allowed_hosts = [
>     "myhost.tailnet-name.ts.net",
>     "myhost.tailnet-name.ts.net:4870",
> ]
> ```

Any reverse proxy works the same way — the rule is that whatever ends up in
the `Host` header must be listed.

## 6. Verify

```sh
# no token → 401 with a bare challenge
curl -s -i -X POST http://127.0.0.1:4870/mcp -d '{}' | head -3

# wrong token → 401 that says what's actually wrong
curl -s -X POST http://127.0.0.1:4870/mcp \
  -H 'Authorization: Bearer nonsense' -d '{}'
# → unauthorized: token matches no configured identity; kaed tokens do not expire

# right token → anything but 401
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:4870/mcp \
  -H "Authorization: Bearer $(cat ~/.config/kaed/token)" -d '{}'
```

A `406` on that last one is success: you authenticated, and the MCP layer then
rejected the empty body. Anything other than `401` means auth worked.

Repeat against the external URL if you exposed it.

## 7. Wire up a client

Add kaed as a custom MCP server / connector:

- **URL:** `https://<host>.<tailnet>.ts.net:4870/mcp` (or
  `http://127.0.0.1:4870/mcp` when the agent runs on the same box)
- **Auth header:** `Authorization: Bearer <the token>`

For Claude Code, `.claude.json`:

```json
{
  "mcpServers": {
    "kaed": {
      "type": "http",
      "url": "https://myhost.tailnet-name.ts.net:4870/mcp",
      "headers": { "Authorization": "Bearer PASTE_TOKEN_HERE" }
    }
  }
}
```

If your client offers no way to set an auth header, it needs a bridge — and
that is worth telling us about.

**Clients load MCP config at session start.** After changing a token or URL,
restart the client.

---

## Rotating a token

kaed re-reads token files on `SIGHUP`, and can honour the previous token
during a grace window. Together those make rotation non-breaking on the server
side — no restart, so live sessions survive:

```sh
cd ~/.config/kaed

# 1. open the grace window: old token keeps working
cp token token.prev

# 2. mint and install the new one
head -c 48 /dev/urandom | base64 | tr -d '/+=' | head -c 48 > token
chmod 600 token token.prev

# 3. load both — no restart
systemctl --user reload kaed

# … update your clients; they pick it up when they next restart …

# 4. close the window
rm token.prev
systemctl --user reload kaed
```

While the window is open, every request still using the old token logs:

```
WARN kaed::server: authenticated with the PREVIOUS token; this client has not
picked up the new one yet author="claude"
```

That line is how you know it is safe to close the window. It is the only
signal available — 401s are rejected before the transaction layer, so the
journal never sees them.

**`token_env` cannot participate in this.** A process cannot re-read its own
systemd `EnvironmentFile`; those variables were set at exec and are frozen for
its lifetime. Env-var tokens still work, but changing one means restarting the
daemon, which drops every live session. Use `token_file`.

## Operating notes

**Tokens never expire.** A 401 means the presented token matches no configured
identity — wrong, or rotated. kaed says so in the `WWW-Authenticate` header
per RFC 6750, because clients otherwise render a bare 401 as "token expired"
and send you hunting for a TTL that does not exist.

**The journal is sensitive.** `~/.local/share/kaed/journal.db` holds the
content of files kaed has edited. It is created `0600` and blob content ages
out per `retention_days`, but treat it as being as sensitive as the most
sensitive file kaed is allowed to touch.

**Logs.** `journalctl --user -u kaed -f`. `RUST_LOG=kaed=debug` for more.

**Torn transactions.** If the process dies mid-transaction, the next startup
logs a warning naming the transaction and its files. Repair is currently
manual and deliberately so — automatic repair is not something to build before
seeing what torn states actually look like.

## Troubleshooting

| Symptom | Cause |
|---|---|
| 4xx before auth, only via the proxy | `Host` not in `server.allowed_hosts`. Add both bare and `:port` forms. |
| `401` with a token you believe is right | Client hasn't restarted since the token changed, or you're sending the old one. Tokens do not expire. |
| `denied` on a path you expected to read | The deny list. Run `kaed check-config` to see every active rule. This is permanent — no path correction will work. |
| `outside_root` | The path escapes its root, or is absolute. Paths are always root-relative. |
| `list`/`search` results look short | Check `denied_hidden` in the response — entries were filtered by the deny list. |
| Identity missing from `check-config` | Token file unreadable or empty. kaed warns and disables that identity rather than failing to start. |
| Service won't start | `kaed check-config` first; it fails loudly on bad roots, duplicate names, roots inside denied areas, and malformed `[auth]` entries. |

---

## Handing this to an agent

If you have an agent with shell access to the target host, the following is
enough for it to do the install. Paste it, filling in the two bracketed parts.

> Please install and configure kaed on this host, following
> `docs/setup.md` in <https://github.com/kenhia/kaed>.
>
> Specifics for my setup:
> - Roots I want reachable: **[list the directories]**
> - Remote access: **[e.g. "expose via tailscale serve" / "loopback only"]**
>
> Requirements, in priority order:
> 1. **Do not root at `$HOME`.** Use the directories I listed, nothing
>    broader. If one of them looks like it contains credentials, tell me
>    before proceeding rather than adding a deny rule and moving on.
> 2. Generate a fresh random token into `~/.config/kaed/token` with mode
>    0600. Do not print it. Tell me where it is so I can copy it into my
>    client myself.
> 3. Use `token_file`, not `token_env`, and include `ExecReload` in the
>    systemd unit — otherwise token rotation requires a restart that kills
>    live sessions.
> 4. Run `kaed check-config` and show me the output, especially the resolved
>    roots and the deny rules. I want to see what it can reach before it is
>    running.
> 5. Verify with curl that a missing token, a wrong token, and the real token
>    behave as the setup doc describes, and show me those three results.
>
> Do not commit anything, and do not put the token in any file under a
> configured root.

Two things worth knowing about that prompt. It asks the agent **not to print
the token** — the value would otherwise land in a transcript, which is exactly
how this project's first token had to be rotated. And it asks to see
`check-config` output **before** trusting the deploy, because the resolved
roots are the entire security boundary and they are easy to get wrong in a way
that looks fine.
