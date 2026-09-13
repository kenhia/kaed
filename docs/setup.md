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

## The short version

```sh
git clone https://github.com/kenhia/kaed
cd kaed
./deploy/install.sh          # binary, systemd user unit, starter config
$EDITOR ~/.config/kaed/config.toml     # roots, and the [auth] allow-list
kaed check-config            # read the roots and deny rules it prints
systemctl --user start kaed
```

There is no token to mint. A caller declares its identity in a header and
kaed checks the name against `[auth]`; see
[Identities](#2-allow-list-your-identities) for what that does and does not
protect.

`install.sh` is idempotent, and re-running it is how you upgrade. It never
overwrites an existing config — the one thing you cannot safely regenerate
underneath a running host. `--dry-run` prints what it would do.

If you keep published builds somewhere, the same script can install one
instead of building — no clone and no Rust toolchain on the target host. See
[Installing a published build](#installing-a-published-build-no-checkout-no-toolchain).

The rest of this page is what those five commands do, and why each choice
is the way it is. Read at least [Choosing roots](#choosing-roots--the-one-thing-to-get-right)
and [the `Host` header gotcha](#5-expose-it-optional) before trusting a
deploy.

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

`--version` reports the commit it was built from, e.g.
`kaed 0.1.0 (966367c 2026-08-02)` — a `-dirty` suffix means the working tree
had uncommitted changes. That string is what tells one host's binary from
another's, and the MCP handshake reports the same one, so a connected agent
and a shell on the box agree about which build is running. Building from a
tarball with no `.git` gives `0.1.0 (unknown)` rather than failing.

### Installing a published build (no checkout, no toolchain)

Building on every host you deploy to means every host needs a clone and a
Rust toolchain, and "which one is stale?" becomes a question you have to keep
answering. `install.sh --from-store` installs a **published** build instead:

```sh
export KAED_STORE_URL=https://your-store.example:4880
./deploy/install.sh --from-store              # latest, or --version <ver>
```

It expects a plain static file tree — anything that serves files over HTTP
will do:

```
artifacts/kaed/latest                        # text: the current version
artifacts/kaed/<version>/kaed-x86_64-linux   # the binary, named for its arch
artifacts/kaed/<version>/install.sh
artifacts/kaed/<version>/kaed.service
artifacts/kaed/<version>/config.example.toml
artifacts/kaed/<version>/SHA256SUMS
```

Every file is verified against `SHA256SUMS` before anything is installed, and
the binary must report the version it was published under — a mislabelled
build is refused rather than installed to lie about itself afterwards. Under
`--dry-run` the fetch and both checks still run; only the install is skipped.

**On a host with no checkout**, fetch the installer out of the bundle and
check it before running it, rather than piping `curl` into `sh`:

```sh
base="$KAED_STORE_URL/artifacts/kaed"; v=$(curl -fsS "$base/latest")
curl -fsS -O "$base/$v/install.sh"
curl -fsS "$base/$v/SHA256SUMS" | grep ' install.sh$' | sha256sum -c -
sh install.sh --from-store --version "$v"
```

There is no default store URL: pass `--store` or set `KAED_STORE_URL`, or the
script stops and says so. Rolling back is `--version <older>` — which is why
publishing is worth the trouble at all.

`just publish` builds a release and pushes that layout to this project's own
store; it refuses a dirty tree, and reads the version out of the binary it
just built so the label cannot drift from the stamp.

## 2. Allow-list your identities

A caller tells kaed who it is:

```
X-Homelab-Agent: claude-kai
```

kaed looks that name up in `[auth]`. A name that is there is accepted and
recorded on every journal entry it writes; a name that is not is refused
with `401`. There is no token, no expiry, and nothing to rotate.

```toml
[auth]
claude       = {}
claude-kai   = {}
claude-kubs0 = {}
```

**Be clear about what this is.** A declared name is *attribution*, not
authentication — anything that can reach kaed's port can claim any name on
that list. The access-control boundary is the network: bind loopback, front
it with something that restricts who can connect (`tailscale serve` in the
reference deployment), and treat the set of hosts that can reach the port as
the set of hosts you would hand a shell to. [SECURITY.md](../SECURITY.md)
sets out the trade in full.

kaed also records the caller's tailnet node beside the declared name, so a
name claimed from an unexpected machine is visible in the journal afterwards
— see [whois](#tailnet-whois-and-node-pinning).

Adding or removing an identity changes the config *shape*, so it needs a
**restart**, not `SIGHUP`. The symptom of forgetting is a `401` that reads
like a wrong credential.

One identity per client **machine**, not per harness or per human: the thing
you revoke is a line in a file on a host. A backend behind a gateway must
list the identities that reach it *proxied*, even though they never dial it
directly.

### A declared name is the only credential

An unknown name is **refused**, never quietly resolved to something else:
authenticating a caller as something it did not claim to be would be the
least honest outcome available, and there is no second mechanism it could
fall through to. Sprint 023 introduced declared identity alongside a bearer
token; sprint 024 deleted the bearer, so `token_file`, `token_env` and
`prev_token_file` no longer exist.

A config still naming one of those fields will **not start** — deliberately,
because `install.sh` never rewrites a config and a surviving credential row
would otherwise be invisible. kaed says so by name rather than leaving you
with a bare parse error:

```
Error: config ~/.config/kaed/config.toml still carries retired credential
fields (token_file). kaed has accepted only declared identities since sprint
023 … Delete them: an `[auth]` entry is now `<name> = {}` …
```

The fix is the one the message names: delete the field. An entry is a name.

## 3. Write the config

`install.sh` drops [`deploy/config.example.toml`](../deploy/config.example.toml)
at `~/.config/kaed/config.toml` if you have none. Its roots are placeholders,
and kaed refuses to start on a root that does not exist — so a fresh install
is enabled but deliberately not started until you have edited this file.

`~/.config/kaed/config.toml`:

```toml
[server]
bind = "127.0.0.1:4870"
# Host header values to accept beyond loopback. If a proxy fronts kaed, the
# proxy's hostname MUST be here — see the gotcha below.
allowed_hosts = []
# This instance's name in the fleet, and the prefix on every root name it
# serves. Defaults to the short system hostname; set it only if this host
# goes by another name.
# host = "myhost"

# Roots are the only paths kaed can reach. Name them explicitly and narrowly.
# Declare the LOCAL name; kaed serves it host-qualified, so `src` below is
# addressed as `myhost:src`.
[[roots]]
name = "src"
path = "/home/you/src"
description = "code repos"

[[roots]]
name = "scratch"
path = "/home/you/scratch"
description = "scratch space"

# Optional: the declared fleet — every OTHER host that should, or
# deliberately should not, run kaed. Omit the table entirely and kaed says
# so (`fleet.declared: false`) rather than implying this host is all there
# is. See "Declaring the fleet" below.
# [peers.otherhost]
# status = "active"
# url    = "https://otherhost.example.ts.net:4870/mcp"

[auth]
# An allow-list of identities. The name arrives in `X-Homelab-Agent`, and is
# recorded on every journal entry. No token; see "Allow-list your
# identities" above for what that does and does not protect.
claude = {}

[whois]
# Record the caller's tailnet node beside the identity. Data, not a check.
enabled = true
enforce = false

[limits]
max_read_bytes = 262144       # per-response byte budget
max_file_bytes = 8388608      # refuse to read/edit files larger than this
search_max_results = 50       # default cap on search hits

[journal]
# Days of blob *content* retained. Transaction metadata is kept indefinitely.
retention_days = 7

[security]
# Extends the built-in deny defaults, which cover credential *stores*
# (.ssh, .gnupg, .aws, …). Secret-bearing *files* (.env, *.env, *.pem,
# id_*, …) are handled by the classify list instead: served redacted
# rather than refused (see the reference table below). A path is refused
# if it or any ancestor matches.
#
# Write the bare form (`**/secrets`), not `**/secrets/**` — ancestor
# matching makes the bare form cover the directory and everything under
# it, while the `/**` form leaves the directory itself listable.
# Absolute patterns are legal too, and `*` crosses `/`: on a host keeping
# live state in per-service data directories, "/srv/*/data" denies every
# one of them, including ones created after the rule was written.
deny = [
    "**/secrets",
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

### Root names are host-qualified

You declare `name = "src"`; kaed serves it as `myhost:src`, and that is what
tools take as `root`. The unqualified form is not accepted — one root, one
spelling — but passing it gets an error naming the replacement, not a bare
"unknown root".

This costs nothing on one host and is what makes a fleet addressable later
without renaming anything: `root` was always the indirection every tool
already went through.

### Declaring the fleet

If you run kaed on more than one machine, `[peers]` is where you say so —
and, more usefully, where you say which machines are **deliberately without
an instance**:

```toml
[peers.buildbox]
status = "active"
url    = "https://buildbox.example.ts.net:4870/mcp"

[peers.dbhost]
status = "deferred"
ref    = "issue-929"
note   = "broad-access design not settled; sits next to the datastore"
```

`roots` returns this to every client, so an agent that finds no kaed on
`dbhost` learns it is a decision rather than a broken rollout — which is the
mistake that put this here. Statuses carry their own evidence: `deferred`
requires `ref`, `unreachable` requires `since`, and kaed refuses to start
without them.

Leave `[peers]` out and kaed reports `fleet.declared: false` — honest about
claiming nothing, so a host's absence proves nothing either way. Write it,
and absence starts to mean something.

### Gateway mode: routing to peers

A peer entry with a `url` is more than a declaration — the instance will
**proxy** calls addressing that peer's roots, so a client wired to one host
can edit files on all of them. What makes this safe to audit is identity:
the gateway **forwards the caller's own declared name** to the peer, so an
edit on the target is journaled under the same author as a direct call. It
never substitutes a name of its own.

```toml
[peers.buildbox]
status = "active"
url    = "https://buildbox.example.ts.net:4870/mcp"
```

That is the whole of it — the gateway holds no credential for its peers and
needs none. A backend that does not allow-list the forwarded name refuses
the call (`peer_credential_rejected`, naming the identity and the fix),
which is the right answer: a misattributed journal row on the backend would
be worse than a refused call.

**Give each machine its own identity, not each person or each tool.** Once
more than one client exists, a shared identity means the journal can only
say "some agent somewhere" — and the point of the journal is to answer
"which machine changed this". Name the identity after the host it is
configured on (`claude-buildbox`), since that is the thing you can actually
revoke.

Every backend must list the identities that reach it **proxied**, even
though they never dial it directly. That is the one configuration step the
gateway adds, and forgetting it produces a `401` at the backend that
surfaces as `peer_credential_rejected` naming the missing name. Because the
allow-list is config *shape*, adding it is a restart, not a `SIGHUP`.

This used to cost far more. Before sprint 023 the gateway held a bearer
token per (identity, peer) pair — two identities across a gateway and two
backends was six token files, on top of each backend's own — and that
multiplication is what retired the scheme. Forwarding the caller's name
buys the same attribution for nothing.

With routing configured,
`roots` **probes** peers live (under the caller's identity): an answering
peer shows up `verified: true` with its version and its roots merged in; one
that stopped answering becomes `status: "unreachable"` with the observed
`since`, its last-known roots still listed and labelled. `search` accepts a
root pattern (`*:*`, `*:src`) for a fleet-wide search in one call.

Peers without a `url` stay what they were in the previous section: reported
declarations, `verified: false`. A declaration is not an observation, and
reporting one as the other is the bug this feature exists to prevent.

One failure domain to name: if every client points at one gateway, that
gateway going down takes the fleet with it. Each host's own URL keeps
working — keep the direct wiring documented (and occasionally used) as the
fallback.

### Config reference

| Key | Meaning |
|---|---|
| `server.bind` | Listen address. Keep it on loopback unless you know why not. |
| `server.allowed_hosts` | Extra `Host` header values accepted beyond loopback. **Required when proxied.** |
| `server.host` | This instance's fleet name and the prefix on every root name. Defaults to the short system hostname; startup fails if neither yields one. |
| `roots[]` | `name` (local, no `:` or `/`), `path`, optional `description`. `path` may use `~/`. Canonicalized at startup; duplicates and denied roots are refused. |
| `peers.<host>` | Declared fleet member. `status` = `active` \| `deferred` (needs `ref`) \| `unreachable` (needs `since`), plus optional `note` and `url`. With a `url`, calls addressing that host's roots are proxied there, under the caller's own forwarded identity. Omit the whole table to declare nothing. |
| `auth.<identity>` | An allow-listed identity, ordinarily an empty table (`claude-kai = {}`). The name arrives in `X-Homelab-Agent`; an unknown one is `401`. **Adding or removing one needs a restart** — see below. |
| `auth.<identity>.nodes` | Tailnet nodes this identity may arrive from. Empty (default) = unpinned. Recorded either way; only *enforced* when `whois.enforce` is on. |
| `whois.enabled` | Resolve the caller's tailnet node (default `true`). `false` records `unknown` for everything — a supported deployment, e.g. a host with no `tailscale` binary. |
| `whois.enforce` | Refuse a caller whose node is not in its identity's `nodes` (default `false`). An unresolvable node counts as "not in the list". |
| `whois.ttl_secs` | Cache lifetime, negative answers included (default 300). |
| `whois.binary` | Path to `tailscale` (default `tailscale`). |
| `limits.*` | Response and file size budgets. |
| `journal.path` | Defaults to `$XDG_DATA_HOME/kaed/journal.db`. |
| `journal.retention_days` | Blob content retention. Metadata is kept forever — so past this window `journal` still shows what changed and when, while `diff`/`revert` can name a version they can no longer reconstruct (they say so, with the window, rather than returning an empty diff). |
| `security.deny` | Extra deny globs, matched against absolute paths. |
| `security.use_default_deny` | Set `false` to drop the built-in glob defaults. kaed's own directories stay refused regardless. |
| `security.classify` | Extra *classification* globs: matching files are secret-bearing but served **redacted** (dotenv-shaped files get placeholders and typed env ops) rather than refused. |
| `security.use_default_classify` | Set `false` to drop the built-in classify defaults (`.env*`, `*.env`, `*.pem`, `id_*`, `credentials*`, `*.kdbx`). |
| `secrets.shapes.<name>` | Named entry for the `secret` tool's shape registry — a spec from the closed grammar (`hex(N)`, `base64url(N)`, `uuid4`, `prefixed(tag,inner)`), e.g. `klams = "prefixed(klams-,hex(64))"`. Validated at startup. |
| `secrets.allow_reveal` | Set `false` to refuse `secret_reveal` on this host entirely (structured `reveal_disabled` refusal). The default is `true`: the tool being separately permissioned at the harness is the primary gate. |
| `secrets.leak_checks` | Write-side leak detection strictness. `"refuse"` (default): writing content that matches a known secret's digest, a provider token prefix, or a private-key block into an **unclassified** file refuses with a named `allow_secrets` override, while merely secret-shaped content applies with a warning. `"flag"`: everything warns, nothing blocks. `"off"`: no scanning. |

### What reloads, and what needs a restart

`SIGHUP` re-resolves the identity allow-list from the spec the process
captured at startup. It does not re-read the config file, so **every**
change below needs `systemctl --user restart kaed`:

| Change | `systemctl --user reload` is enough |
|---|---|
| New `[auth]` identity | no — restart |
| Removing an `[auth]` identity | no — restart |
| Pinning an identity to `nodes` | no — restart |
| Any `[whois]` setting | no — restart |
| New peer, or a changed peer `url` | no — restart |

Since 024 there is nothing a reload can usefully change: the credential
files it existed to re-read are gone. It is kept because `SIGHUP` is a
documented signal and removing it silently would be a contract change — but
if you are reaching for it, the answer is almost certainly a restart.

Since 023 almost everything here is a restart, because almost everything
here is now config shape rather than file contents — which is the direct
consequence of there being no credential left to reload.

The failure this prevents is mildly misleading if you hit it: SIGHUP
succeeds, the new identity never appears, and the client gets a `401` —
which reads as "wrong credential" rather than "the server never loaded your
identity."

### Tailnet whois and node pinning

A declared name says *who*; it says nothing about *where from*. kaed asks
`tailscale whois` for the caller's node and records it beside the identity
on every journaled mutation and every secrets audit row, so "why did a write
from `claude` show up here" is answerable after the fact.

Behind `tailscale serve` the socket peer is always loopback, so the address
comes from the first entry of `X-Forwarded-For`; a direct deployment falls
back to the socket peer.

It is **data, not a check**. tailscaled down, no `tailscale` binary, an
address the tailnet does not know, and `enabled = false` all record the same
thing — `unknown` — and none of them refuses a request.

If you want it to be a check, pin an identity and turn enforcement on:

```toml
[auth]
claude-buildbox = { nodes = ["buildbox"] }

[whois]
enforce = true
```

Only identities that declare `nodes` are constrained; everything else stays
unpinned. A pinned identity whose node cannot be resolved is refused, since
"unknown" is not in any allow-list. Note that a call arriving through a
gateway resolves to the *gateway's* node, not the original client's — so
pin proxied identities to the gateway as well as their own host.

### Validate it

```sh
kaed check-config
```

This prints the host name it resolved, the resolved roots (host-qualified, as
tools will address them), the declared fleet, the allow-listed identities
(with any `nodes` pin), the limits, the journal path, and **every deny rule in force**. It
exits non-zero on anything invalid. Read the deny list it prints — that is the
real one, not the one you think you wrote.

## 4. Run it as a service

`install.sh` installs [`deploy/kaed.service`](../deploy/kaed.service),
`daemon-reload`s, enables it and turns on linger. For reference, or to do it
by hand, that unit is:

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

`ExecReload` wires `SIGHUP` to the allow-list re-resolve. Since 024 that
changes nothing on its own — see [what reloads](#what-reloads-and-what-needs-a-restart)
— but keep it: the unit is installed, not generated, and a host that loses it
loses the signal rather than gaining anything.

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

Three probes, and the two refusals are not optional: a single accepted call
passes against a server that accepts anything, so without the controls it
only proves the port is open.

```sh
# no credential → 401 with a bare challenge
curl -s -i -X POST http://127.0.0.1:4870/mcp -d '{}' | head -3

# a stale bearer → 401 naming the header to send and the one to drop
curl -s -X POST http://127.0.0.1:4870/mcp \
  -H 'Authorization: Bearer nonsense' -d '{}'
# → unauthorized: kaed no longer accepts bearer tokens (sprint 024): send
#   X-Homelab-Agent: <your identity> instead, and drop the Authorization header

# an unknown name → 401, never a downgrade to something else
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:4870/mcp \
  -H 'X-Homelab-Agent: nobody' -d '{}'

# an allow-listed name → anything but 401
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:4870/mcp \
  -H 'X-Homelab-Agent: claude-thishost' -d '{}'
```

A `406` on that last one is success: you authenticated, and the MCP layer then
rejected the empty body. Anything other than `401` means auth worked.

Repeat against the external URL if you exposed it.

## 7. Wire up a client

**How many connections?** One per *entry point*, not one per host. A single
instance means one connection, and that is the whole answer. A fleet whose
gateway has peer routing configured (see [Gateway mode](#gateway-mode-routing-to-peers))
also means **one connection — to the gateway**: its `roots` response carries
every host's roots, calls addressing another host route there under your own
identity, and the gateway-only client additionally gets fleet-wide search and
unreachable-hosts-as-data. A second, permanent connection to a host the
gateway already reaches is not a backup, it is strictly worse for the session
that picks it — a half-fleet `roots` view, a duplicate set of tool schemas,
no fleet search. Keep the backend's direct URL *documented* (it is the
gateway-down fallback, and it should stay exercised), but wire it only when
you need it.

Add kaed as a custom MCP server / connector:

- **URL:** `https://<host>.<tailnet>.ts.net:4870/mcp` (or
  `http://127.0.0.1:4870/mcp` when the agent runs on the same box)
- **Auth header:** `X-Homelab-Agent: <your identity>` — the name you
  allow-listed for *this machine*. There is no token; an `Authorization`
  header is ignored and, on its own, gets a `401` that says so.

**Prefer the client's own tooling — `claude mcp add` for Claude Code.** It
owns that file's format and encoding. Hand-editing it is where the accidents
happen, and one of them cost this project every MCP server on a machine; see
[the warning below](#name-every-entry-after-its-host).

Name the entry after the host it connects to: `kaed-kai` — never a bare
`kaed`. The name says which machine answers, which matters the moment a
second connection exists for any reason (a fallback wired during an outage,
a host not behind the gateway), and a wrong-machine edit still returns a
successful-looking diff. Note the name is the *connection's* target, not the
fleet: a gateway entry named `kaed-kai` still serves `kubs0:*` roots, and
that is fine — the roots themselves carry their host.

For Claude Code, `.claude.json`:

```json
{
  "mcpServers": {
    "kaed-myhost": {
      "type": "http",
      "url": "https://myhost.tailnet-name.ts.net:4870/mcp",
      "headers": { "Authorization": "Bearer PASTE_TOKEN_HERE" }
    }
  }
}
```

If your client offers no way to set an auth header, it needs a bridge — and
that is worth telling us about.

**Clients load MCP config at session start.** After changing the identity or
the URL, restart the client — a running session keeps the config it loaded,
so it will go on sending the old header and getting `401` until you do.

### Name every entry after its host

> #### ⚠️ Editing a client config from PowerShell will eat it
>
> Wiring a second host, this project rewrote `~/.claude.json` from PowerShell
> with `Set-Content -Encoding UTF8`. On **Windows PowerShell 5.1 that means
> UTF-8 *with* BOM**, and it wrote CRLF endings too. `JSON.parse` rejects a
> leading BOM, so Claude Code could not read its own config, moved it aside
> to `.claude.json.backup`, and regenerated a default.
>
> That silently removed **every** configured MCP server — including two that
> had nothing to do with kaed and had been working fine. Nothing warned at
> write time; the client just reported no MCP servers configured.
>
> If you must write such a file directly:
>
> ```powershell
> $json = $obj | ConvertTo-Json -Depth 100
> $json = $json -replace "`r`n", "`n"                      # LF, like the client writes
> [IO.File]::WriteAllText($p, $json,
>     (New-Object System.Text.UTF8Encoding $false))        # UTF-8, NO BOM
> ```
>
> `WriteAllText` with an explicit `UTF8Encoding($false)` behaves the same on
> PowerShell 5.1 and 7. `Set-Content` does not: on 5.1 its default is ANSI and
> its `-Encoding UTF8` is the BOM variant.
>
> Then **verify by reading the file back and parsing it**, which is the step
> that would have caught this before a restart made it visible:
>
> ```powershell
> .\deploy\check-client-config.ps1 -Expect kaed-kai
> ```
>
> [`deploy/check-client-config.ps1`](../deploy/check-client-config.ps1) checks
> for a BOM, CRLF, that the file parses, and that the servers you expect are
> actually present. It exits non-zero otherwise. Note that a config PowerShell
> mangles can still *look* fine to PowerShell — it parsed happily in every
> check except the byte-level one.
>
> The same trap applies to `claude_desktop_config.json` and to any other
> application-owned JSON you are tempted to edit from a script.

---

## Revoking an identity

There is no rotation, because there is no secret. To stop an identity being
accepted, delete its line from `[auth]` and restart:

```sh
$EDITOR ~/.config/kaed/config.toml     # remove `claude-oldbox = {}`
systemctl --user restart kaed
```

A restart rather than `SIGHUP`, because the allow-list is config *shape*; a
reload would succeed and change nothing, and the identity would keep
working. That is the one failure mode worth remembering here.

Revocation is per host. An identity that reaches a backend *through* a
gateway is allow-listed on the backend, so removing it there is what stops
it — the gateway holds nothing to take away.

**What this does not do** is keep out whoever could reach the port. Deleting
a name stops that name being *recorded*, and stops a caller using it by
accident; it is not a security boundary against anything already inside the
perimeter. If that is what you need, the fix is at the network layer. See
[SECURITY.md](../SECURITY.md).

## Operating notes

**A 401 says which of three things is wrong.** An `X-Homelab-Agent` naming
no configured identity; an `Authorization` header and **no** name — the shape
of a client that missed the cutover, which the body names explicitly along
with the header to send and the one to drop; or no credential at all. kaed
distinguishes them in the `WWW-Authenticate` header and the body per RFC
6750, because clients otherwise render a bare 401 as "token expired" and send
you hunting for a TTL that does not exist. A *declared* name that is unknown
is refused outright, because authenticating a caller as something it did not
claim to be would be worse than refusing it.

The challenge still names the `Bearer` scheme even though no bearer is
accepted: a 401 must carry `WWW-Authenticate` (RFC 9110 §15.5.2) and no
registered scheme describes "declare a name in a header", so the scheme is
the envelope and `error_description` carries the truth.

**The journal is sensitive.** `~/.local/share/kaed/journal.db` holds the
content of files kaed has edited. It is created `0600` and blob content ages
out per `retention_days`, but treat it as being as sensitive as the most
sensitive file kaed is allowed to touch.

Agents can now read that history back (`journal`, `diff`, `revert`), which
raises the stakes on the file mode rather than lowering them: content from a
classified file is stored and served as a redacted rendering, and a blob
written before classification covered its path is redacted on the way out —
but the store still holds everything else verbatim. The deny list is what
keeps that set small.

**Logs.** `journalctl --user -u kaed -f`. `RUST_LOG=kaed=debug` for more.

**Torn transactions.** If the process dies mid-transaction, the next startup
logs a warning naming the transaction and its files. Repair is currently
manual and deliberately so — automatic repair is not something to build before
seeing what torn states actually look like.

## Troubleshooting

| Symptom | Cause |
|---|---|
| 4xx before auth, only via the proxy | `Host` not in `server.allowed_hosts`. Add both bare and `:port` forms. |
| `401` with an identity you believe is right | If you just *added* it, the daemon needs a **restart**, not a reload — SIGHUP does not re-read `[auth]`, so the name is genuinely not in the running allow-list. Otherwise the client hasn't restarted since its config changed. Check the response body: it distinguishes an unknown name from a client still sending a bearer. |
| `denied` on a path you expected to read | The deny list. Run `kaed check-config` to see every active rule. This is permanent — no path correction will work. |
| `outside_root` | The path escapes its root, or is absolute. Paths are always root-relative. |
| `list`/`search` results look short | Check `denied_hidden` (the deny list), `classified_hidden` (secret-bearing files with no redacted surface) and `unreadable_hidden` (the OS refused) in the response. |
| `denied` with `reason: not_readable_by_service_identity` | Unix ownership, not kaed's config — the data names the file's owner and mode and the uid kaed runs as. `chmod`/`chgrp` on the host, or read it as an identity that can. |
| A write refuses with `not_writable_by_service_identity` | kaed writes by staging a temp file and renaming, so it needs write+execute on the **containing directory**; the file's own mode is not the obstacle. If that root's `description` names where the file is managed from, the refusal repeats it as `root_advisory`. |
| A `dry_run` that used to pass now refuses | Since sprint 014 `dry_run` probes writability instead of only checking the content. The write it was predicting could never have landed. |
| `search` found nothing and you expected hits | Check `files_searched`. Zero means your `glob`/`path` selected nothing, not that the pattern is absent — `glob` matches **root-relative** paths and is not re-anchored by `path`. The `reason` in the response names the fix. |
| `not_found` on a root you are sure exists | Root names are host-qualified: `myhost:src`, not `src`. The error carries `data.did_you_mean`. |
| `not_found` naming another host | Read `data.reason`. `host_deferred` means that host deliberately has no instance (`data.ref` says why) — do not install one. `host_never_declared` means this host's config says nothing about it. |
| Identity missing from `check-config` | It is not in `[auth]`. Nothing else can hide one: a name on the allow-list resolves unconditionally, reading no files. |
| `still carries retired credential fields` | This config predates sprint 024 and names `token_file` / `token_env` / `prev_token_file`. Delete them; an `[auth]` entry is `<name> = {}` and `[peers.<host>.tokens]` tables go entirely. |
| Service won't start | `kaed check-config` first; it fails loudly on bad roots, duplicate names, roots inside denied areas, malformed `[auth]` entries, and `[peers]` statuses missing their required `ref`/`since`. |

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
> 2. Allow-list **one identity per client machine**, named after the host it
>    is configured on. There is no credential to generate: a caller declares
>    its name in `X-Homelab-Agent` and an unknown name is refused.
> 3. Run `kaed check-config` and show me the output, especially the resolved
>    roots and the deny rules. I want to see what it can reach before it is
>    running.
> 4. Verify with curl that the declared name is accepted, and that **no
>    credential** and an **unknown name** are both refused. Show me all three
>    results — the two refusals are the controls, and without them the first
>    result only proves the port is open.
>
> Do not commit anything.

Two things worth knowing about that prompt. It asks for the two **refusals**
as well as the success: a probe that only shows the accepted case passes
against a server that accepts everything, which is a real failure mode and not
a hypothetical one. And it asks to see `check-config` output **before**
trusting the deploy, because the resolved roots are the entire security
boundary and they are easy to get wrong in a way that looks fine.
