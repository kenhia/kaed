//! TOML config: roots, auth token indirection, limits, journal, peers.
//!
//! Tokens never live in the file — `[auth]` maps an author identity to the
//! env var holding its bearer token, resolved at startup. `resolve()` is
//! the validation gate: it canonicalizes roots, rejects duplicates, and
//! reads token env vars, producing the runtime view the server uses.
//!
//! Since sprint 007 this file also carries the **declared fleet**. Roots
//! are declared by their local name and served under a host-qualified one
//! (`src` → `kai:src`), and `[peers]` names every host that should — or
//! deliberately should not — run kaed. `config.toml` is *installed* rather
//! than cloned, so it is the one place a declaration can live that is
//! present on clone-less hosts (PD-5, korg #930).

use crate::deny::{DEFAULT_DENY, DenyList};
use anyhow::{Context, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub roots: Vec<RootConfig>,
    /// author identity -> where its bearer token lives
    #[serde(default)]
    pub auth: BTreeMap<String, AuthEntry>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub journal: JournalConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub secrets: SecretsConfig,
    /// The declared fleet: every *other* host that should, or deliberately
    /// should not, run kaed. `None` — the table absent entirely — is the
    /// `never-declared` state, and is reported as such rather than being
    /// silently indistinguishable from "this host is the whole fleet".
    pub peers: Option<BTreeMap<String, PeerConfig>>,
}

/// The secret lifecycle (011): named shapes and the reveal kill-switch.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsConfig {
    /// Named shape entries: name → a spec in the closed grammar
    /// (`hex(N)`, `base64url(N)`, `uuid4`, `prefixed(tag,inner)`), e.g.
    /// `klams = "prefixed(klams-,hex(64))"`. `secret` / `generate` accepts
    /// either a name from here or a raw spec. Validated at startup.
    #[serde(default)]
    pub shapes: BTreeMap<String, String>,
    /// Refuse `secret_reveal` outright when false (011 D-1). The
    /// load-bearing gate is that `secret_reveal` is its own tool and the
    /// harness prompts per tool; this switch exists for hosts where even
    /// that surface is unwanted.
    #[serde(default = "default_true")]
    pub allow_reveal: bool,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            shapes: BTreeMap::new(),
            allow_reveal: true,
        }
    }
}

/// One declared fleet member. Statuses carry the evidence for themselves:
/// a `deferred` host without a `ref` is the documentation-that-lives-nowhere
/// problem PD-5 moved this declaration into `config.toml` to escape, and an
/// `unreachable` without a `since` starts lying about the present the moment
/// it is written. Both are refused at startup (D-5).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConfig {
    pub status: PeerStatus,
    /// Why this host is what it is — a korg reference, e.g. `korg:929`.
    /// Required for `deferred`.
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Free-text elaboration, surfaced verbatim in `roots`.
    pub note: Option<String>,
    /// When this host was last known good. Required for `unreachable`.
    pub since: Option<String>,
    /// Base URL of the peer's MCP endpoint, e.g.
    /// `https://kubs0.<tailnet>:4870/mcp`. With peer mode (010) this is
    /// what makes an `active` peer *routable*: calls addressing its roots
    /// are proxied there. Without it the peer is declaration only.
    pub url: Option<String>,
    /// Author → where that author's bearer token *for this peer* lives.
    /// The gateway proxies with the caller's own identity (PD-4): a call
    /// from `claude` rides the `claude` token configured here, and an
    /// author with no entry is refused (`no_peer_credential`) rather than
    /// ever borrowing another identity's credential.
    #[serde(default)]
    pub tokens: BTreeMap<String, PeerTokenEntry>,
}

/// Where one author's token for one peer lives. Exactly one source, same
/// rules as [`AuthEntry`] — except no grace slot: a grace window is a
/// *server-side* affordance, and this is the client half of the exchange.
/// `token_file` re-reads on SIGHUP (#914, extended to peer credentials in
/// 010); `token_env` is frozen at exec and needs a restart, same as always.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerTokenEntry {
    pub token_env: Option<String>,
    pub token_file: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerStatus {
    /// Declared to be running kaed. Whether it *is* is a separate question —
    /// see `FleetHost::verified`.
    Active,
    /// Deliberately not running kaed. Carries `ref` to the reasoning.
    Deferred,
    /// Should be running kaed and is known not to be answering.
    Unreachable,
}

impl PeerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PeerStatus::Active => "active",
            PeerStatus::Deferred => "deferred",
            PeerStatus::Unreachable => "unreachable",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Extra deny globs, matched against absolute paths. A path is refused
    /// if it or any ancestor matches, so `**/.ssh` covers the whole tree.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Apply `deny::DEFAULT_DENY` on top of `deny`. Turn off only to let
    /// kaed edit dotfiles it would otherwise refuse; kaed's own config and
    /// journal stay refused either way.
    #[serde(default = "default_true")]
    pub use_default_deny: bool,
    /// Extra classification globs: secret-bearing, but served redacted
    /// rather than refused (dotenv-shaped files get the typed surface;
    /// anything else refuses with `classified_opaque`). Only explicit
    /// `deny` hard-refuses — heuristics classify (D-1).
    #[serde(default)]
    pub classify: Vec<String>,
    /// Apply `policy::DEFAULT_CLASSIFY` on top of `classify`.
    #[serde(default = "default_true")]
    pub use_default_classify: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            deny: Vec::new(),
            use_default_deny: true,
            classify: Vec::new(),
            use_default_classify: true,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// Extra `Host` header values to accept, on top of loopback — set this
    /// to the tailnet hostname when fronted by `tailscale serve`.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// This instance's name in the fleet, and the prefix on every root name
    /// it serves (`src` → `kai:src`). Defaults to the short system
    /// hostname, which is what makes the 007 rename a zero-touch upgrade on
    /// hosts whose config `install.sh` will never overwrite (D-1).
    pub host: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            allowed_hosts: Vec::new(),
            host: None,
        }
    }
}

/// The short system hostname — the first label of `/etc/hostname`, falling
/// back to `$HOSTNAME`. An FQDN is truncated: `kai.example.ts.net` is still
/// `kai`, because the fleet names hosts the way `tailscale status` and the
/// deploy docs do.
///
/// `None` when neither source yields anything usable, which `resolve` turns
/// into a startup failure naming `[server] host`. Guessing here would put a
/// wrong prefix on every root name and every journal row.
pub fn system_host() -> Option<String> {
    let from_file = std::fs::read_to_string("/etc/hostname").ok();
    let from_env = std::env::var("HOSTNAME").ok();
    from_file
        .into_iter()
        .chain(from_env)
        .filter_map(|raw| {
            let short = raw.trim().split('.').next().unwrap_or("").trim().to_owned();
            (!short.is_empty()).then_some(short)
        })
        .next()
}

/// A host name has to survive being half of `host:root`, so it may not
/// contain the separator — nor a slash, which would let it forge a path.
fn check_host_name(what: &str, name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name.contains(':') || name.contains('/') {
        bail!("{what} {name:?} must be non-empty and free of ':' and '/'");
    }
    Ok(())
}

fn default_bind() -> SocketAddr {
    // homelab convention: same port on every host, loopback only
    "127.0.0.1:4870".parse().expect("valid default bind")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
}

/// Where one identity's bearer token lives. Exactly one of `token_env` or
/// `token_file` must be set.
///
/// `token_file` is what makes rotation non-breaking (#914): a process
/// cannot re-read its own systemd `EnvironmentFile` — those vars were
/// injected at exec and are frozen for its life — so reload requires the
/// token to be readable *at reload time*, which means from a file.
/// `token_env` still works and still needs a restart; that limitation is
/// inherent, not an oversight.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthEntry {
    pub token_env: Option<String>,
    /// File whose trimmed contents are the token. Re-read on SIGHUP.
    pub token_file: Option<String>,
    /// The previous token, honoured alongside the current one during a
    /// rotation. Lets clients pick up the new secret whenever they next
    /// restart instead of all at once.
    pub prev_token_file: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_max_read_bytes")]
    pub max_read_bytes: usize,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default = "default_search_max_results")]
    pub search_max_results: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_read_bytes: default_max_read_bytes(),
            max_file_bytes: default_max_file_bytes(),
            search_max_results: default_search_max_results(),
        }
    }
}

fn default_max_read_bytes() -> usize {
    262_144
}
fn default_max_file_bytes() -> u64 {
    8_388_608
}
fn default_search_max_results() -> usize {
    50
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalConfig {
    /// Defaults to `$XDG_DATA_HOME/kaed/journal.db` (or `~/.local/share/…`).
    pub path: Option<String>,
    /// Days of **blob content** kept. Transaction metadata is kept
    /// indefinitely; only the file content ages out, because that content
    /// is a copy of whatever the edited file held (#909).
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

// manual impl: a derived Default would zero retention_days when the whole
// [journal] table is absent, bypassing the serde field default
impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            path: None,
            retention_days: default_retention_days(),
        }
    }
}

// A week, not the 30 days sprint 001 shipped: with GC now actually running
// this is a real window of retained file content, and every conflict delta
// that has ever mattered here was hours old, not weeks.
fn default_retention_days() -> u32 {
    7
}

impl Config {
    pub fn default_path() -> PathBuf {
        base_dir("XDG_CONFIG_HOME", ".config").join("kaed/config.toml")
    }

    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    /// Validate and produce the runtime view. Fails on bad roots or
    /// duplicate names; a missing token env var is a warning (the identity
    /// is skipped), because one host rarely defines every agent's token.
    ///
    /// `config_path` is where this config was loaded from — its directory
    /// is refused unconditionally, so kaed can never serve the token file
    /// that sits beside it. Pass `None` only when there is no file (tests).
    pub fn resolve(&self, config_path: Option<&Path>) -> anyhow::Result<Resolved> {
        let journal_path = match &self.journal.path {
            Some(p) => expand_home(p),
            None => base_dir("XDG_DATA_HOME", ".local/share").join("kaed/journal.db"),
        };

        // Built-ins first: they must hold whatever the roots turn out to be.
        let mut builtin = vec![(
            config_path
                .unwrap_or(&Config::default_path())
                .parent()
                .unwrap_or(Path::new("/"))
                .to_path_buf(),
            "kaed's own config directory",
        )];
        if let Some(dir) = journal_path.parent() {
            builtin.push((dir.to_path_buf(), "kaed's journal directory"));
        }
        let mut globs = self.security.deny.clone();
        if self.security.use_default_deny {
            globs.extend(DEFAULT_DENY.iter().map(|s| (*s).to_string()));
        }
        let deny =
            Arc::new(DenyList::new(builtin, &globs).context("building the security deny list")?);

        let mut classify_globs = self.security.classify.clone();
        if self.security.use_default_classify {
            classify_globs.extend(
                crate::policy::DEFAULT_CLASSIFY
                    .iter()
                    .map(|s| (*s).to_string()),
            );
        }
        let classify = Arc::new(
            crate::policy::Classifier::new(&classify_globs)
                .context("building the classification list")?,
        );

        let host = match &self.server.host {
            Some(h) => h.trim().to_owned(),
            None => system_host().unwrap_or_default(),
        };
        check_host_name("host", &host).context(
            "kaed could not determine this host's name, and every root name is \
             prefixed with it — set `host` under [server] in config.toml",
        )?;
        tracing::info!(
            host,
            configured = self.server.host.is_some(),
            "serving roots under this host name"
        );

        let mut roots = Vec::new();
        for r in &self.roots {
            if r.name.is_empty() || r.name.contains('/') || r.name.contains(':') {
                bail!(
                    "root name {:?} must be non-empty and free of '/' and ':' — declare \
                     the local name only; kaed prefixes it with the host",
                    r.name
                );
            }
            // Qualified from here on: this is what tools match, what `roots`
            // advertises, and what lands in the journal (D-6).
            let name = format!("{host}:{}", r.name);
            if roots.iter().any(|x: &ResolvedRoot| x.name == name) {
                bail!("duplicate root name {:?}", r.name);
            }
            let expanded = expand_home(&r.path);
            let canonical = std::fs::canonicalize(&expanded).with_context(|| {
                format!("root {:?}: canonicalizing {}", r.name, expanded.display())
            })?;
            if !canonical.is_dir() {
                bail!(
                    "root {:?}: {} is not a directory",
                    r.name,
                    canonical.display()
                );
            }
            // A root inside a denied area would be entirely unusable, and
            // silently so — that is a config bug worth failing loudly on.
            if let Some(rule) = deny.denied_by(&canonical) {
                bail!(
                    "root {:?}: {} is refused by the deny list (rule: {rule})",
                    r.name,
                    canonical.display()
                );
            }
            roots.push(ResolvedRoot {
                name,
                local_name: r.name.clone(),
                host: host.clone(),
                path: canonical,
                description: r.description.clone(),
                deny: deny.clone(),
                classify: classify.clone(),
            });
        }

        for (author, entry) in &self.auth {
            match (&entry.token_env, &entry.token_file) {
                (Some(_), Some(_)) => {
                    bail!("auth {author:?}: set token_env or token_file, not both")
                }
                (None, None) => bail!("auth {author:?}: needs token_env or token_file"),
                _ => {}
            }
            if entry.prev_token_file.is_some() && entry.token_file.is_none() {
                bail!(
                    "auth {author:?}: prev_token_file needs token_file — a grace window \
                     is only useful with reloadable tokens"
                );
            }
        }
        let identities = resolve_identities(&self.auth);
        let peers = self.resolve_peers(&host)?;

        // Named shapes are validated at startup: a bad spec is a config
        // bug, and failing the first `generate` instead would waste an
        // agent's turn on an operator's typo.
        let mut shapes = BTreeMap::new();
        for (name, spec) in &self.secrets.shapes {
            let shape = crate::shapes::parse_spec(spec)
                .map_err(|e| anyhow::anyhow!("[secrets] shapes.{name}: {}", e.message))?;
            shapes.insert(name.clone(), shape);
        }

        Ok(Resolved {
            bind: self.server.bind,
            allowed_hosts: self.server.allowed_hosts.clone(),
            host,
            roots,
            peers,
            identities,
            limits: self.limits,
            journal_path,
            journal_retention_days: self.journal.retention_days,
            deny,
            classify,
            auth: self.auth.clone(),
            secrets: ResolvedSecrets {
                shapes,
                allow_reveal: self.secrets.allow_reveal,
            },
        })
    }

    /// Validate `[peers]` into the declared fleet. `None` in, `None` out:
    /// an absent table is the `never-declared` state and must stay
    /// distinguishable from an empty one, which asserts that this host is
    /// the whole fleet (D-4).
    fn resolve_peers(&self, host: &str) -> anyhow::Result<Option<Vec<Peer>>> {
        let Some(declared) = &self.peers else {
            tracing::warn!(
                "no [peers] table in config: this host declares no fleet, so `roots` \
                 reports fleet.declared = false and an absent host means nothing"
            );
            return Ok(None);
        };
        let mut peers = Vec::new();
        for (name, p) in declared {
            check_host_name("peer name", name)?;
            if name == host {
                bail!(
                    "peer {name:?} is this host: [peers] declares the *rest* of the \
                     fleet, and this instance's own entry is derived from what it serves"
                );
            }
            match p.status {
                PeerStatus::Deferred if p.reference.is_none() => bail!(
                    "peer {name:?}: status \"deferred\" needs `ref` — a host declared \
                     deliberately absent with no pointer to the reasoning is the gap \
                     korg #930 was filed about"
                ),
                PeerStatus::Unreachable if p.since.is_none() => bail!(
                    "peer {name:?}: status \"unreachable\" needs `since` — an undated \
                     outage cannot be told apart from a stale declaration"
                ),
                _ => {}
            }
            for (author, entry) in &p.tokens {
                match (&entry.token_env, &entry.token_file) {
                    (Some(_), Some(_)) => bail!(
                        "peer {name:?} token for {author:?}: set token_env or \
                         token_file, not both"
                    ),
                    (None, None) => bail!(
                        "peer {name:?} token for {author:?}: needs token_env or \
                         token_file"
                    ),
                    _ => {}
                }
                if !self.auth.contains_key(author) {
                    tracing::warn!(
                        peer = name,
                        author,
                        "peer token for an author this host's [auth] does not know — \
                         nobody can authenticate here as that identity, so the token \
                         is unusable (typo?)"
                    );
                }
            }
            if !p.tokens.is_empty() && p.status == PeerStatus::Deferred {
                tracing::warn!(
                    peer = name,
                    "tokens configured for a deferred peer: it deliberately runs no \
                     kaed, so these credentials route nowhere"
                );
            }
            if !p.tokens.is_empty() && p.url.is_none() {
                tracing::warn!(
                    peer = name,
                    "peer tokens without a `url`: nothing can be proxied there until \
                     one is declared"
                );
            }
            peers.push(Peer {
                host: name.clone(),
                status: p.status,
                reference: p.reference.clone(),
                note: p.note.clone(),
                since: p.since.clone(),
                url: p.url.clone(),
                tokens: p.tokens.clone(),
            });
        }
        Ok(Some(peers))
    }
}

/// Read every identity's current (and grace) token from wherever it lives.
/// Called at startup and again on every SIGHUP — so this must stay pure
/// I/O over the config spec, holding no state of its own.
///
/// A missing or empty token disables that identity with a warning rather
/// than failing: one host rarely defines every agent's token, and a reload
/// that killed the server over an unrelated identity would be worse than
/// the problem it was fixing.
pub fn resolve_identities(auth: &BTreeMap<String, AuthEntry>) -> Vec<Identity> {
    let mut identities = Vec::new();
    for (author, entry) in auth {
        let Some(token) = entry.current_token() else {
            tracing::warn!(author, "token unset or empty; identity disabled");
            continue;
        };
        // An absent prev file is the normal state — it exists only while a
        // rotation is in flight — so its absence is not worth a warning.
        let prev_token = entry
            .prev_token_file
            .as_ref()
            .and_then(|p| read_token_quiet(p));
        identities.push(Identity {
            author: author.clone(),
            token,
            prev_token,
        });
    }
    identities
}

impl AuthEntry {
    fn current_token(&self) -> Option<String> {
        match (&self.token_file, &self.token_env) {
            (Some(path), _) => read_token(path),
            (None, Some(var)) => std::env::var(var).ok().filter(|t| !t.is_empty()),
            (None, None) => None,
        }
    }
}

fn read_token(path: &str) -> Option<String> {
    match std::fs::read_to_string(expand_home(path)) {
        Ok(s) => Some(s.trim().to_string()).filter(|t| !t.is_empty()),
        Err(e) => {
            tracing::warn!(path, error = %e, "could not read token file");
            None
        }
    }
}

/// Like `read_token`, but a missing file is expected rather than reported.
/// Anything else that goes wrong still warns — an unreadable-but-present
/// token file is a real problem.
fn read_token_quiet(path: &str) -> Option<String> {
    let expanded = expand_home(path);
    if !expanded.exists() {
        return None;
    }
    read_token(path)
}

/// One validated fleet member. Still the *declaration*: whether the host
/// is actually serving is observed at call time by `fleet::Peers` (010)
/// and reported per-call, never written back here — conflating the two
/// would rebuild #930.
#[derive(Debug, Clone)]
pub struct Peer {
    pub host: String,
    pub status: PeerStatus,
    pub reference: Option<String>,
    pub note: Option<String>,
    pub since: Option<String>,
    pub url: Option<String>,
    /// Author → token source for proxying to this peer (PD-4). The spec,
    /// not the secret: values are resolved by [`resolve_peer_tokens`] at
    /// startup and on every SIGHUP.
    pub tokens: BTreeMap<String, PeerTokenEntry>,
}

impl Peer {
    /// Declaration-only peer with no credentials — the 007 shape, used by
    /// tests that exercise the fleet table without routing anywhere.
    pub fn declared(host: impl Into<String>, status: PeerStatus) -> Peer {
        Peer {
            host: host.into(),
            status,
            reference: None,
            note: None,
            since: None,
            url: None,
            tokens: BTreeMap::new(),
        }
    }
}

/// Read every peer token from wherever it lives: `(peer host, author)` →
/// bearer token. Pure I/O over the config spec, like [`resolve_identities`]
/// — called at startup and again on every SIGHUP, so a rotated peer token
/// file takes effect without a restart (#914 extended, 010 D-2). A missing
/// or empty token just leaves that (peer, author) pair unroutable, with a
/// warning; refusing to serve over it would let one stale file take down
/// every local tool.
pub fn resolve_peer_tokens(peers: &[Peer]) -> BTreeMap<(String, String), String> {
    let mut tokens = BTreeMap::new();
    for peer in peers {
        for (author, entry) in &peer.tokens {
            let token = match (&entry.token_file, &entry.token_env) {
                (Some(path), _) => read_token(path),
                (None, Some(var)) => std::env::var(var).ok().filter(|t| !t.is_empty()),
                (None, None) => None,
            };
            match token {
                Some(t) => {
                    tokens.insert((peer.host.clone(), author.clone()), t);
                }
                None => tracing::warn!(
                    peer = peer.host,
                    author,
                    "peer token unset or empty; this identity cannot be proxied there"
                ),
            }
        }
    }
    tokens
}

/// The validated runtime view of the config.
#[derive(Debug)]
pub struct Resolved {
    pub bind: SocketAddr,
    pub allowed_hosts: Vec<String>,
    /// This instance's fleet name; the prefix on every root name it serves.
    pub host: String,
    pub roots: Vec<ResolvedRoot>,
    /// The declared fleet, minus this host. `None` means no `[peers]` table
    /// at all — never-declared, not "no peers".
    pub peers: Option<Vec<Peer>>,
    pub identities: Vec<Identity>,
    pub limits: Limits,
    pub journal_path: PathBuf,
    pub journal_retention_days: u32,
    pub deny: Arc<DenyList>,
    pub classify: Arc<crate::policy::Classifier>,
    /// Where each identity's token lives, kept so SIGHUP can re-read them
    /// without reparsing the config file.
    pub auth: BTreeMap<String, AuthEntry>,
    pub secrets: ResolvedSecrets,
}

/// `[secrets]` after validation: named shapes parsed, ready to mint.
#[derive(Debug, Clone)]
pub struct ResolvedSecrets {
    pub shapes: BTreeMap<String, crate::shapes::Shape>,
    pub allow_reveal: bool,
}

impl Default for ResolvedSecrets {
    fn default() -> Self {
        Self {
            shapes: BTreeMap::new(),
            allow_reveal: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    /// Host-qualified — `kai:src`. This is what tools take, what `roots`
    /// advertises, and what the journal records.
    pub name: String,
    /// The name as declared in `config.toml`, without the host. Kept so an
    /// agent that passed the old unqualified form gets told what to pass
    /// instead (D-2) rather than a bare `not_found`.
    pub local_name: String,
    pub host: String,
    /// Canonicalized; the jail boundary for every path under this root.
    pub path: PathBuf,
    pub description: Option<String>,
    /// Shared across every root: paths refused inside the jail, too.
    pub deny: Arc<DenyList>,
    /// Shared across every root: paths served redacted, not plain.
    pub classify: Arc<crate::policy::Classifier>,
}

impl ResolvedRoot {
    /// A root that denies nothing — for tests and for callers that build a
    /// root outside `Config::resolve`. `name` is taken as already qualified
    /// if it carries a host, and qualified under `local` otherwise.
    pub fn unrestricted(name: impl Into<String>, path: PathBuf) -> ResolvedRoot {
        let name = name.into();
        let (host, local_name) = match name.split_once(':') {
            Some((h, l)) => (h.to_owned(), l.to_owned()),
            None => ("local".to_owned(), name.clone()),
        };
        ResolvedRoot {
            name,
            local_name,
            host,
            path,
            description: None,
            deny: Arc::new(DenyList::empty()),
            classify: Arc::new(crate::policy::Classifier::empty()),
        }
    }

    /// `unrestricted`, but with the default classification rules — the
    /// test-side stand-in for a `Config::resolve`d root.
    pub fn with_default_classify(name: impl Into<String>, path: PathBuf) -> ResolvedRoot {
        ResolvedRoot {
            classify: Arc::new(
                crate::policy::Classifier::new(
                    &crate::policy::DEFAULT_CLASSIFY
                        .iter()
                        .map(|s| (*s).to_string())
                        .collect::<Vec<_>>(),
                )
                .expect("default classify globs build"),
            ),
            ..ResolvedRoot::unrestricted(name, path)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub author: String,
    pub token: String,
    /// Accepted alongside `token` during a rotation; its use is logged, so
    /// "has every client picked up the new secret yet?" has an answer.
    pub prev_token: Option<String>,
}

fn base_dir(xdg_var: &str, home_fallback: &str) -> PathBuf {
    if let Ok(dir) = std::env::var(xdg_var)
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    home().join(home_fallback)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_gets_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.server.bind, default_bind());
        assert_eq!(cfg.limits.max_read_bytes, 262_144);
        assert_eq!(cfg.limits.max_file_bytes, 8_388_608);
        assert_eq!(cfg.limits.search_max_results, 50);
        assert_eq!(cfg.journal.retention_days, 7);
        assert!(cfg.roots.is_empty());
    }

    #[test]
    fn full_config_parses() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            bind = "127.0.0.1:4999"

            [[roots]]
            name = "home"
            path = "/home/ken"
            description = "everything under ~"

            [auth]
            claude = { token_env = "KAED_TOKEN_CLAUDE" }

            [limits]
            max_read_bytes = 1024

            [journal]
            retention_days = 7
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.bind.port(), 4999);
        assert_eq!(cfg.roots[0].name, "home");
        assert_eq!(
            cfg.auth["claude"].token_env.as_deref(),
            Some("KAED_TOKEN_CLAUDE")
        );
        assert_eq!(cfg.limits.max_read_bytes, 1024);
        // unspecified limits keep their defaults
        assert_eq!(cfg.limits.max_file_bytes, 8_388_608);
        assert_eq!(cfg.journal.retention_days, 7);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[server]\nbindd = \"x\"").is_err());
    }

    #[test]
    fn tokens_come_from_a_file_and_re_reading_picks_up_a_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let cur = dir.path().join("claude.token");
        let prev = dir.path().join("claude.token.prev");
        std::fs::write(&cur, "  v1\n").unwrap(); // trimmed on read
        let cfg: Config = toml::from_str(&format!(
            "[auth]\nclaude = {{ token_file = \"{}\", prev_token_file = \"{}\" }}\n",
            cur.display(),
            prev.display()
        ))
        .unwrap();

        let first = cfg.resolve(None).unwrap().identities;
        assert_eq!(first[0].token, "v1");
        assert_eq!(first[0].prev_token, None); // no prev file yet

        // rotate on disk, then re-resolve the way SIGHUP does
        std::fs::write(&cur, "v2\n").unwrap();
        std::fs::write(&prev, "v1\n").unwrap();
        let after = resolve_identities(&cfg.auth);
        assert_eq!(after[0].token, "v2");
        assert_eq!(after[0].prev_token.as_deref(), Some("v1"));
    }

    #[test]
    fn auth_entries_must_name_exactly_one_token_source() {
        let both: Config =
            toml::from_str("[auth]\nc = { token_env = \"E\", token_file = \"/f\" }\n").unwrap();
        assert!(
            both.resolve(None)
                .unwrap_err()
                .to_string()
                .contains("not both")
        );

        let neither: Config = toml::from_str("[auth]\nc = {}\n").unwrap();
        assert!(
            neither
                .resolve(None)
                .unwrap_err()
                .to_string()
                .contains("needs token_env or token_file")
        );

        // a grace window with no reloadable token would never take effect
        let dangling: Config =
            toml::from_str("[auth]\nc = { token_env = \"E\", prev_token_file = \"/p\" }\n")
                .unwrap();
        assert!(
            dangling
                .resolve(None)
                .unwrap_err()
                .to_string()
                .contains("prev_token_file needs token_file")
        );
    }

    #[test]
    fn resolve_denies_kaeds_own_config_and_journal_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("kaed-conf/config.toml");
        let cfg: Config = toml::from_str(&format!(
            "[journal]\npath = \"{}/state/journal.db\"\n",
            dir.path().display()
        ))
        .unwrap();
        let resolved = cfg.resolve(Some(&cfg_path)).unwrap();
        assert!(resolved.deny.is_denied(&dir.path().join("kaed-conf/env")));
        assert!(
            resolved
                .deny
                .is_denied(&dir.path().join("state/journal.db"))
        );
        assert!(!resolved.deny.is_denied(&dir.path().join("src/main.rs")));
    }

    #[test]
    fn resolve_applies_default_denies_unless_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let on: Config = toml::from_str("").unwrap();
        assert!(
            on.resolve(None)
                .unwrap()
                .deny
                .is_denied(&dir.path().join(".ssh/id_ed25519"))
        );

        let off: Config = toml::from_str("[security]\nuse_default_deny = false\n").unwrap();
        assert!(
            !off.resolve(None)
                .unwrap()
                .deny
                .is_denied(&dir.path().join(".ssh/id_ed25519"))
        );
    }

    #[test]
    fn resolve_rejects_a_root_inside_a_denied_area() {
        // a root nobody could read from is a config bug, not a quiet no-op
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[[roots]]\nname = \"k\"\npath = \"{}\"\n",
            ssh.display()
        ))
        .unwrap();
        let err = cfg.resolve(None).unwrap_err();
        assert!(
            err.to_string().contains("refused by the deny list"),
            "{err}"
        );
    }

    /// The 007 rename: config declares the local name, the server serves it
    /// host-qualified, and both halves stay addressable for error messages.
    #[test]
    fn roots_are_served_host_qualified() {
        let dir = tempfile::tempdir().unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[server]\nhost = \"kai\"\n[[roots]]\nname = \"src\"\npath = \"{}\"\n",
            dir.path().display()
        ))
        .unwrap();
        let r = cfg.resolve(None).unwrap();
        assert_eq!(r.host, "kai");
        assert_eq!(r.roots[0].name, "kai:src");
        assert_eq!(r.roots[0].local_name, "src");
        assert_eq!(r.roots[0].host, "kai");
    }

    #[test]
    fn an_unset_host_falls_back_to_the_short_system_hostname() {
        // Zero-touch upgrade (D-1): kai and kubs0 gain qualified names on
        // restart without anyone editing a config install.sh won't overwrite.
        let Some(sys) = system_host() else {
            return; // no /etc/hostname and no $HOSTNAME: nothing to assert
        };
        assert!(!sys.contains('.'), "{sys:?} should be the short form");
        let dir = tempfile::tempdir().unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[[roots]]\nname = \"src\"\npath = \"{}\"\n",
            dir.path().display()
        ))
        .unwrap();
        let r = cfg.resolve(None).unwrap();
        assert_eq!(r.host, sys);
        assert_eq!(r.roots[0].name, format!("{sys}:src"));
    }

    #[test]
    fn a_root_name_may_not_carry_its_own_host() {
        let dir = tempfile::tempdir().unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[server]\nhost = \"kai\"\n[[roots]]\nname = \"kai:src\"\npath = \"{}\"\n",
            dir.path().display()
        ))
        .unwrap();
        let err = cfg.resolve(None).unwrap_err();
        assert!(
            err.to_string().contains("declare the local name only"),
            "{err}"
        );
    }

    #[test]
    fn an_unusable_host_name_is_refused_rather_than_prefixed_onto_everything() {
        for bad in ["", "  ", "kai:2", "a/b"] {
            let cfg: Config = toml::from_str(&format!("[server]\nhost = \"{bad}\"\n")).unwrap();
            let err = cfg.resolve(None).unwrap_err();
            assert!(
                format!("{err:#}").contains("[server]") || format!("{err:#}").contains("host"),
                "host {bad:?}: {err:#}"
            );
        }
    }

    /// PD-5's three states start here: an absent `[peers]` table is
    /// `never-declared` and must not read as "the fleet is just me".
    #[test]
    fn an_absent_peers_table_is_never_declared_not_an_empty_fleet() {
        let no_table: Config = toml::from_str("[server]\nhost = \"kai\"\n").unwrap();
        assert!(no_table.resolve(None).unwrap().peers.is_none());

        let empty: Config = toml::from_str("[server]\nhost = \"kai\"\n[peers]\n").unwrap();
        assert_eq!(empty.resolve(None).unwrap().peers.unwrap().len(), 0);
    }

    #[test]
    fn peers_parse_into_the_declared_fleet() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            host = "kai"

            [peers.kubs0]
            status = "active"
            url = "https://kubs0.example:4870/mcp"

            [peers.kubsdb]
            status = "deferred"
            ref = "korg:929"
            note = "broad-access design not settled"
            "#,
        )
        .unwrap();
        let peers = cfg.resolve(None).unwrap().peers.unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].host, "kubs0");
        assert_eq!(peers[0].status, PeerStatus::Active);
        assert_eq!(peers[1].status, PeerStatus::Deferred);
        assert_eq!(peers[1].reference.as_deref(), Some("korg:929"));
        assert_eq!(
            peers[1].note.as_deref(),
            Some("broad-access design not settled")
        );
    }

    /// D-5: a status has to carry its own evidence, or the declaration is
    /// just the doc-that-lives-nowhere problem in a new file.
    #[test]
    fn deferred_needs_a_ref_and_unreachable_needs_a_since() {
        let no_ref: Config =
            toml::from_str("[server]\nhost = \"kai\"\n[peers.x]\nstatus = \"deferred\"\n").unwrap();
        let err = no_ref.resolve(None).unwrap_err();
        assert!(err.to_string().contains("needs `ref`"), "{err}");

        let no_since: Config =
            toml::from_str("[server]\nhost = \"kai\"\n[peers.x]\nstatus = \"unreachable\"\n")
                .unwrap();
        let err = no_since.resolve(None).unwrap_err();
        assert!(err.to_string().contains("needs `since`"), "{err}");

        let ok: Config = toml::from_str(
            "[server]\nhost = \"kai\"\n[peers.x]\nstatus = \"unreachable\"\nsince = \"2026-08-07\"\n",
        )
        .unwrap();
        assert!(ok.resolve(None).is_ok());
    }

    /// D-2 (010): peer credentials resolve from files and re-resolve the
    /// way SIGHUP does, so a rotation needs no restart — #914's promise,
    /// extended to the outbound direction.
    #[test]
    fn peer_tokens_parse_resolve_and_rotate() {
        let dir = tempfile::tempdir().unwrap();
        let tok = dir.path().join("kubs0-claude.token");
        std::fs::write(&tok, "  peer-v1\n").unwrap(); // trimmed on read
        let cfg: Config = toml::from_str(&format!(
            r#"
            [server]
            host = "kai"
            [peers.kubs0]
            status = "active"
            url = "https://kubs0.example:4870/mcp"
            [peers.kubs0.tokens]
            claude = {{ token_file = "{}" }}
            "#,
            tok.display()
        ))
        .unwrap();
        let peers = cfg.resolve(None).unwrap().peers.unwrap();
        assert_eq!(peers[0].tokens.len(), 1);

        let key = ("kubs0".to_string(), "claude".to_string());
        let first = resolve_peer_tokens(&peers);
        assert_eq!(first[&key], "peer-v1");

        std::fs::write(&tok, "peer-v2\n").unwrap();
        let after = resolve_peer_tokens(&peers);
        assert_eq!(after[&key], "peer-v2");

        // an unreadable token disables that pair, it does not fail the host
        std::fs::remove_file(&tok).unwrap();
        assert!(!resolve_peer_tokens(&peers).contains_key(&key));
    }

    #[test]
    fn a_peer_token_needs_exactly_one_source() {
        for (spec, msg) in [
            (
                "claude = { token_env = \"E\", token_file = \"/f\" }",
                "not both",
            ),
            ("claude = { }", "needs token_env or token_file"),
        ] {
            let cfg: Config = toml::from_str(&format!(
                "[server]\nhost = \"kai\"\n[peers.kubs0]\nstatus = \"active\"\n\
                 [peers.kubs0.tokens]\n{spec}\n"
            ))
            .unwrap();
            let err = cfg.resolve(None).unwrap_err();
            assert!(err.to_string().contains(msg), "{err}");
        }
    }

    #[test]
    fn a_peer_may_not_be_this_host() {
        let cfg: Config =
            toml::from_str("[server]\nhost = \"kai\"\n[peers.kai]\nstatus = \"active\"\n").unwrap();
        let err = cfg.resolve(None).unwrap_err();
        assert!(err.to_string().contains("is this host"), "{err}");
    }

    #[test]
    fn resolve_rejects_duplicate_root_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_str().unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[[roots]]\nname = \"a\"\npath = \"{p}\"\n[[roots]]\nname = \"a\"\npath = \"{p}\"\n"
        ))
        .unwrap();
        let err = cfg.resolve(None).unwrap_err();
        assert!(err.to_string().contains("duplicate root name"));
    }

    #[test]
    fn resolve_rejects_missing_root() {
        let cfg: Config =
            toml::from_str("[[roots]]\nname = \"a\"\npath = \"/nonexistent/kaed-test\"\n").unwrap();
        assert!(cfg.resolve(None).is_err());
    }

    #[test]
    fn resolve_canonicalizes_roots_and_resolves_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_str().unwrap();
        // unique env var per test: parallel tests share the process env
        unsafe { std::env::set_var("KAED_TEST_TOKEN_RESOLVE", "sekrit") };
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[roots]]
            name = "t"
            path = "{p}"
            [auth]
            claude = {{ token_env = "KAED_TEST_TOKEN_RESOLVE" }}
            ghost = {{ token_env = "KAED_TEST_TOKEN_UNSET_XYZ" }}
            "#
        ))
        .unwrap();
        let resolved = cfg.resolve(None).unwrap();
        assert!(resolved.roots[0].path.is_absolute());
        assert_eq!(resolved.identities.len(), 1);
        assert_eq!(resolved.identities[0].author, "claude");
        assert_eq!(resolved.identities[0].token, "sekrit");
    }
}
