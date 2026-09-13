//! TOML config: roots, the identity allow-list, limits, journal, peers.
//!
//! Since sprint 023 `[auth]` is an **allow-list of declared identities**,
//! not a token table: a caller names itself in `X-Homelab-Agent` and an
//! unknown name is refused. Sprint 024 deleted the token fields that had
//! been kept through the transition window, so an entry is a name and at
//! most a `nodes` pin. `resolve()` is the validation gate: it
//! canonicalizes roots, rejects duplicates, and produces the runtime view
//! the server uses.
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
    /// The allow-list: identity name -> its (optional) node pin.
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
    #[serde(default)]
    pub whois: WhoisConfig,
    /// The declared fleet: every *other* host that should, or deliberately
    /// should not, run kaed. `None` — the table absent entirely — is the
    /// `never-declared` state, and is reported as such rather than being
    /// silently indistinguishable from "this host is the whole fleet".
    pub peers: Option<BTreeMap<String, PeerConfig>>,
}

/// The tailnet cross-check (023, WI 2392).
///
/// A top-level section rather than `[auth.whois]` — which is where klams
/// put the same settings — because kaed's `[auth]` is a *map of identity
/// names*, so a `whois` key inside it would be indistinguishable from an
/// identity called `whois`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhoisConfig {
    /// Resolve the caller's node at all. Off means every request records
    /// `unknown`, which is a supported deployment (a host with no
    /// `tailscale` binary), not a degraded one.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Refuse a request from a node an identity is not pinned to.
    /// **Off by default**, and deliberately so (D-5).
    #[serde(default)]
    pub enforce: bool,
    /// How long a resolved (or unresolved) address stays cached.
    #[serde(default = "default_whois_ttl_secs")]
    pub ttl_secs: u64,
    /// The `tailscale` binary, for a host that keeps it somewhere odd.
    #[serde(default = "default_tailscale_binary")]
    pub binary: String,
}

impl Default for WhoisConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            enforce: false,
            ttl_secs: default_whois_ttl_secs(),
            binary: default_tailscale_binary(),
        }
    }
}

fn default_whois_ttl_secs() -> u64 {
    300
}

fn default_tailscale_binary() -> String {
    "tailscale".to_string()
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
    /// Write-side leak detection strictness (012 D-5). `"refuse"`
    /// (default): known-digest / provider-prefix / private-key matches
    /// refuse with a named override, the entropy heuristic warns.
    /// `"flag"`: everything warns, nothing blocks. `"off"`: no scanning.
    #[serde(default)]
    pub leak_checks: crate::leak::LeakChecks,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            shapes: BTreeMap::new(),
            allow_reveal: true,
            leak_checks: crate::leak::LeakChecks::default(),
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

/// One allow-listed identity.
///
/// The ordinary entry is **empty** — `claude-kai = {}` — because an
/// identity is a declared name (`X-Homelab-Agent`), not a secret. Since
/// 024 that is the only shape there is: the `token_env` / `token_file` /
/// `prev_token_file` fields are gone, and [`retired_fields`] turns the
/// `deny_unknown_fields` refusal a stale config would hit into a message
/// naming them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthEntry {
    /// Tailnet nodes this identity may arrive from. Empty = unpinned.
    /// Consulted only when `[whois] enforce` is on (023 D-5).
    #[serde(default)]
    pub nodes: Vec<String>,
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
        if let Some(found) = retired_fields(&text) {
            bail!(
                "config {} still carries retired credential fields ({}). kaed has \
                 accepted only declared identities since sprint 023 and the fields \
                 were deleted in 024, so `deny_unknown_fields` would refuse this \
                 file with only a field name to go on. Delete them: an `[auth]` \
                 entry is now `<name> = {{}}`, and `[peers.<host>.tokens]` tables \
                 go entirely — the gateway forwards the caller's own name and holds \
                 no credential.",
                path.display(),
                found.join(", ")
            );
        }
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    /// Validate and produce the runtime view. Fails on bad roots or
    /// duplicate names.
    ///
    /// `config_path` is where this config was loaded from — its directory
    /// is refused unconditionally, so kaed can never serve its own config.
    /// Pass `None` only when there is no file (tests).
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
                leak_checks: self.secrets.leak_checks,
            });
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
                leak_checks: self.secrets.leak_checks,
            },
            whois: self.whois.clone(),
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
            peers.push(Peer {
                host: name.clone(),
                status: p.status,
                reference: p.reference.clone(),
                note: p.note.clone(),
                since: p.since.clone(),
                url: p.url.clone(),
            });
        }
        Ok(Some(peers))
    }
}

/// Retired credential field names present in a raw config, if any (024 D-1).
///
/// The fields are gone from the structs, and `deny_unknown_fields` means a
/// host whose `config.toml` still names one will not start — which is the
/// intended outcome, since `install.sh` deliberately never rewrites a
/// config and a surviving token row would otherwise be invisible. What is
/// *not* intended is diagnosing that from serde's bare "unknown field"
/// error, so this runs first and names the field, the sprint and the fix.
///
/// Deliberately a text scan rather than a permissive parse: a second
/// deserialization shape for fields that no longer exist would be the very
/// thing being deleted, kept alive to describe its own absence.
fn retired_fields(text: &str) -> Option<Vec<String>> {
    // Longest first, and each match is consumed: `prev_token_file` contains
    // `token_file`, so a naive scan reports a field the file never named.
    const RETIRED: [&str; 3] = ["prev_token_file", "token_file", "token_env"];
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut code = line.split('#').next().unwrap_or("").to_string();
        for field in RETIRED {
            if code.contains(field) {
                code = code.replace(field, "");
                if !found.iter().any(|f| f == field) {
                    found.push(field.to_string());
                }
            }
        }
    }
    found.sort();
    (!found.is_empty()).then_some(found)
}

/// Resolve every allow-listed identity. Called at startup and again on
/// every SIGHUP — so this must stay pure over the config spec, holding no
/// state. Since 024 it reads nothing from disk: an identity *is* its name,
/// so the mapping is total and cannot fail.
///
/// It never filters. 023 D-6 is the reason: this function used to drop an
/// identity whose token would not resolve, which at the moment the cutover
/// deleted the token files would have deleted every agent on the fleet.
pub fn resolve_identities(auth: &BTreeMap<String, AuthEntry>) -> Vec<Identity> {
    auth.iter()
        .map(|(author, entry)| Identity {
            author: author.clone(),
            nodes: Arc::new(entry.nodes.clone()),
        })
        .collect()
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
        }
    }
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
    /// The `[auth]` spec as written, kept so SIGHUP can re-resolve the
    /// allow-list without reparsing the config file.
    pub auth: BTreeMap<String, AuthEntry>,
    pub secrets: ResolvedSecrets,
    pub whois: WhoisConfig,
}

/// `[secrets]` after validation: named shapes parsed, ready to mint.
#[derive(Debug, Clone)]
pub struct ResolvedSecrets {
    pub shapes: BTreeMap<String, crate::shapes::Shape>,
    pub allow_reveal: bool,
    pub leak_checks: crate::leak::LeakChecks,
}

impl Default for ResolvedSecrets {
    fn default() -> Self {
        Self {
            shapes: BTreeMap::new(),
            allow_reveal: true,
            leak_checks: crate::leak::LeakChecks::default(),
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
    /// Host-wide write-side leak strictness (012 D-5), stamped per root
    /// because the engine sees the root, not the config.
    pub leak_checks: crate::leak::LeakChecks,
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
            leak_checks: crate::leak::LeakChecks::default(),
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
    /// Tailnet nodes this identity may arrive from. Empty = unpinned.
    /// Consulted only when `[whois] enforce` is on (D-5).
    pub nodes: Arc<Vec<String>>,
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
            claude = {}

            [limits]
            max_read_bytes = 1024

            [journal]
            retention_days = 7
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.bind.port(), 4999);
        assert_eq!(cfg.roots[0].name, "home");
        assert!(cfg.auth.contains_key("claude"));
        assert_eq!(cfg.limits.max_read_bytes, 1024);
        // unspecified limits keep their defaults
        assert_eq!(cfg.limits.max_file_bytes, 8_388_608);
        assert_eq!(cfg.journal.retention_days, 7);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[server]\nbindd = \"x\"").is_err());
    }

    /// An identity IS its name: an empty table is a complete entry, and
    /// since 024 it is the only entry there is.
    #[test]
    fn an_identity_is_just_its_name() {
        let cfg: Config = toml::from_str("[auth]\nclaude-kai = {}\n").unwrap();
        let ids = cfg.resolve(None).unwrap().identities;
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].author, "claude-kai");
        assert!(ids[0].nodes.is_empty(), "unpinned by default");
    }

    /// 023 D-6, and the reason `resolve_identities` has no filter at all:
    /// it once dropped an identity whose token would not resolve, which at
    /// the moment the cutover deleted the token files would have deleted
    /// every agent on the fleet. Nothing may reintroduce a condition here.
    #[test]
    fn every_allow_listed_name_resolves_and_none_is_ever_dropped() {
        let cfg: Config =
            toml::from_str("[auth]\na = {}\nb = { nodes = [\"kai\"] }\nc = {}\n").unwrap();
        let ids = resolve_identities(&cfg.auth);
        assert_eq!(
            ids.iter().map(|i| i.author.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    /// The token fields are GONE from the structs (024), so a stale config
    /// must not start — `install.sh` never rewrites one, and a surviving
    /// token row would otherwise be invisible. What this pins is that the
    /// refusal is *legible*: it names the field, the sprint and the fix,
    /// rather than leaving serde's bare "unknown field" to be diagnosed.
    #[test]
    fn a_config_still_carrying_a_retired_token_field_is_refused_by_name() {
        for spec in [
            "[auth]\nc = { token_file = \"/t\" }\n",
            "[auth]\nc = { token_env = \"E\" }\n",
            "[auth]\nc = { prev_token_file = \"/t.prev\" }\n",
            "[peers.kubs0]\nstatus = \"active\"\n[peers.kubs0.tokens]\nc = { token_file = \"/t\" }\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            std::fs::write(&path, spec).unwrap();
            let err = Config::load(&path).unwrap_err().to_string();
            assert!(err.contains("retired credential fields"), "{err}");
            assert!(err.contains("sprint 023"), "{err}");
        }
    }

    /// Only the field the file actually names. `prev_token_file` contains
    /// `token_file` as a substring, and a naive scan reports both — which
    /// would send an operator looking for a field that is not there.
    #[test]
    fn the_retired_field_scan_names_only_what_is_present() {
        assert_eq!(
            retired_fields("c = { prev_token_file = \"/t.prev\" }").unwrap(),
            ["prev_token_file"]
        );
        assert_eq!(
            retired_fields("c = { token_file = \"/t\" }").unwrap(),
            ["token_file"]
        );
    }

    /// A *comment* mentioning a retired field is not a retired field. The
    /// fleet's own configs carry prose about the cutover, and refusing to
    /// start over a comment would be the scan failing the hosts it exists
    /// to protect.
    #[test]
    fn a_retired_field_named_only_in_a_comment_is_not_a_refusal() {
        assert!(retired_fields("# token_file was deleted in 024\nc = {}\n").is_none());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[auth]\n# no prev_token_file here any more (023 retired it)\nc = {}\n",
        )
        .unwrap();
        assert!(Config::load(&path).is_ok());
    }

    #[test]
    fn node_pinning_is_parsed_and_empty_by_default() {
        let cfg: Config =
            toml::from_str("[auth]\npinned = { nodes = [\"kai\"] }\nopen = {}\n").unwrap();
        assert_eq!(cfg.auth["pinned"].nodes, ["kai"]);
        assert!(cfg.auth["open"].nodes.is_empty());
    }

    /// Enforcement is off unless a host says otherwise (D-5), and whois
    /// itself is on: recording costs nothing and answers "where did this
    /// come from" after the fact.
    #[test]
    fn whois_defaults_record_without_enforcing() {
        let cfg: Config = toml::from_str("[auth]\nc = {}\n").unwrap();
        assert!(cfg.whois.enabled);
        assert!(!cfg.whois.enforce);
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

    /// A routable peer is a URL and nothing else: the gateway forwards the
    /// caller's declared name and holds no credential of its own, so there
    /// is no per-author table to resolve, rotate or get stale (PD-10).
    #[test]
    fn a_routable_peer_is_a_url_and_no_credential() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            host = "kai"
            [peers.kubs0]
            status = "active"
            url = "https://kubs0.example:4870/mcp"
            "#,
        )
        .unwrap();
        let peers = cfg.resolve(None).unwrap().peers.unwrap();
        assert_eq!(peers[0].host, "kubs0");
        assert_eq!(
            peers[0].url.as_deref(),
            Some("https://kubs0.example:4870/mcp")
        );
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
    fn resolve_canonicalizes_roots_and_keeps_every_identity() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_str().unwrap();
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[roots]]
            name = "t"
            path = "{p}"
            [auth]
            claude = {{}}
            ghost = {{}}
            "#
        ))
        .unwrap();
        let resolved = cfg.resolve(None).unwrap();
        assert!(resolved.roots[0].path.is_absolute());
        // Both are identities, and nothing about the host can change that:
        // a name on the allow-list is the whole credential, so resolution
        // reads no files and cannot partially fail.
        assert_eq!(
            resolved
                .identities
                .iter()
                .map(|i| i.author.as_str())
                .collect::<Vec<_>>(),
            ["claude", "ghost"]
        );
    }
}
