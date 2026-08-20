//! MCP wiring: tool registration, bearer auth, streamable HTTP.
//!
//! This layer stays thin — params in, one call into fsops/search/txn,
//! result or `KaedError` out. Failures the agent should see are `isError`
//! tool results carrying the R4 `{code, message, data}` object (as
//! structured content, with a text fallback); protocol-level `Err` is
//! reserved for infrastructure breakage.
//!
//! Auth is an axum middleware in front of the MCP service: a bearer token
//! resolves to an author identity or the request dies 401. The author
//! rides the request extensions into tool handlers, so every journal
//! entry is attributed. No anonymous mutation.

use crate::config::{
    self, AuthEntry, Identity, Limits, Peer, PeerStatus, Resolved, ResolvedRoot, ResolvedSecrets,
};
use crate::errors::KaedError;
use crate::fleet;
use crate::fsops::{self, ReadMode};
use crate::history::{self, FeedbackCategory, JournalQuery, RecordKind};
use crate::journal::{Journal, SecretEvent};
use crate::search;
use crate::secret_tool;
use crate::secrets;
use crate::txn::{self, BaseVersion, EditOp, EditRequest, ValueFrom};
use axum::extract::State;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::{Extension, ToolCallContext};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, Implementation,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ResultType, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use zeroize::Zeroizing;

const DEFAULT_LIST_MAX: usize = 500;
const SEARCH_MAX_RESULTS_CEILING: usize = 1000;

/// The newest MCP revision kaed implements — the ceiling `initialize` may
/// agree to, and the answer to a client that asks for something else.
///
/// rmcp defaults both the advertised set and the fallback to what the *SDK*
/// knows, which is a different claim about a different piece of software.
/// That default is how kaed once echoed 2026-07-28 back to a client while
/// omitting the SEP-2549 cache metadata that revision's `tools/list`
/// requires, leaving Claude Code connected with zero registered tools and no
/// disconnect to explain it (korg #1212, sprint 015). An advertised version
/// is a promise about response *shape*, so this constant is stated here and
/// moves when the implementation does — never as a side effect of an SDK
/// bump. It moved to 2026-07-28 in sprint 016, once [`KaedServer::list_tools`]
/// kept that promise (korg #1214).
const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion::V_2026_07_28;

/// Every revision kaed will negotiate down to, oldest first, ending at
/// [`PROTOCOL_VERSION`]. The older ones are a subset of the newest's shape,
/// so serving them costs nothing.
const SUPPORTED_PROTOCOL_VERSIONS: &[ProtocolVersion] = &[
    ProtocolVersion::V_2024_11_05,
    ProtocolVersion::V_2025_03_26,
    ProtocolVersion::V_2025_06_18,
    ProtocolVersion::V_2025_11_25,
    PROTOCOL_VERSION,
];

/// How long a client may treat `tools/list` as fresh (SEP-2549). kaed's tool
/// catalog is fixed at compile time, so the honest bound is not a duration
/// at all — it is "until this process restarts", and a restart means a new
/// build stamp and a new session. An hour is a conservative reading of that.
const TOOLS_TTL_MS: u64 = 60 * 60 * 1000;

/// What every root on this instance supports. Advertised per-root because
/// under peer mode (korg:1050) a fleet can be mid-upgrade, and the rule from
/// the gateway brainstorm is to advertise the **union** with per-root
/// capabilities — never the intersection, which silently hides working
/// features on the most-updated host.
///
/// `secrets` (008): classified files are served redacted and take env ops.
/// A host without it would refuse env ops at deserialization, so a
/// mid-upgrade fleet needs the flag to route by.
///
/// `history` (009): `journal`/`diff`/`revert`. `feedback` is listed too
/// even though it addresses no root — an agent asking "can I tell this
/// host something went wrong" reads this list, and answering per-root is
/// cheaper than a second discovery mechanism (`roots` is the only one).
/// `secret_lifecycle` (011): the `secret` tool (describe / generate /
/// rotate / occurrences), `secret_reveal`, `env_sync_example`, and
/// `value_from` on `env_set`. One flag for the whole surface: it is one
/// trust tier, and a peer without it maps to `unsupported_capability` at
/// call time (D-3 of 010).
/// `leak_detection` (012): the write path scans for secrets heading into
/// unclassified files (`secret_leak` refusals, `allow_secrets` override).
/// Advertised so a mid-upgrade fleet says honestly which hosts check.
const ROOT_CAPABILITIES: &[&str] = &[
    "stat",
    "list",
    "read",
    "search",
    "edit",
    "secrets",
    "secret_lifecycle",
    "leak_detection",
    "history",
    "feedback",
];

/// The authenticated author identity, set by the auth middleware and read
/// by mutating tools for journal attribution.
#[derive(Debug, Clone)]
pub struct Author(pub String);

pub struct AppState {
    /// This instance's fleet name; the prefix on every root it serves.
    pub host: String,
    pub roots: Vec<ResolvedRoot>,
    /// The declared fleet plus everything needed to route to it (010):
    /// sessions, per-author peer credentials, observed reachability.
    /// `declared() == None` is still the `never-declared` state.
    pub fleet: Arc<fleet::Peers>,
    pub limits: Limits,
    pub journal: Journal,
    /// `[secrets]`: named shapes and the reveal kill-switch (011).
    pub secrets: ResolvedSecrets,
}

impl AppState {
    fn root(&self, name: &str) -> Result<ResolvedRoot, KaedError> {
        if fleet::is_pattern(name) {
            // Patterns are search-only (D-5): search is read-only and its
            // merge has an honest truncation story; a pattern `edit` has no
            // honest atomicity story at all.
            return Err(KaedError::invalid_input(format!(
                "root {name:?} is a pattern; root patterns (`*:*`, `kai:*`, `*:src`) \
                 are only supported by `search`"
            )));
        }
        if let Some(root) = self.roots.iter().find(|r| r.name == name) {
            return Ok(root.clone());
        }
        Err(self.explain_unknown_root(name))
    }

    /// Why that root name did not resolve, and what to do instead.
    ///
    /// Four wrong `root` values have four different remedies, and a bare
    /// "unknown root" sends an agent to `roots` for all of them — including
    /// the two cases where the honest answer is "that host exists and this
    /// is not it" (D-2). This is korg #930 answered on the path a confused
    /// agent actually takes: it is the *error*, not the discovery call, that
    /// the session which assumed rather than asked ends up reading.
    fn explain_unknown_root(&self, name: &str) -> KaedError {
        let known: Vec<&str> = self.roots.iter().map(|r| r.name.as_str()).collect();
        // `this_host` and `target_host`, never a bare `host`: half these
        // payloads describe two different machines and the reader has to be
        // able to tell which is which without inferring it.
        let data = |extra: serde_json::Value| {
            let mut base =
                serde_json::json!({ "root": name, "this_host": self.host, "known_roots": known });
            if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
                b.extend(e.clone());
            }
            base
        };

        let Some((host, local)) = name.split_once(':') else {
            // Unqualified. If it names a root we serve, say so precisely —
            // this is the shape every pre-007 agent and doc still holds.
            let suggestion = self.roots.iter().find(|r| r.local_name == name);
            return match suggestion {
                Some(r) => KaedError::not_found(format!(
                    "root names are host-qualified since sprint 007: pass {:?}, not {name:?}",
                    r.name
                ))
                .with_data(data(serde_json::json!({
                    "reason": "unqualified_root",
                    "did_you_mean": r.name,
                }))),
                None => KaedError::not_found(format!(
                    "unknown root {name:?}; roots are named `host:root` (this host is \
                     {:?}) — call `roots` for the list",
                    self.host
                ))
                .with_data(data(serde_json::json!({ "reason": "unqualified_root" }))),
            };
        };

        if host == self.host {
            return KaedError::not_found(format!(
                "unknown root {local:?} on {host}; this host serves {known:?}"
            ))
            .with_data(data(
                serde_json::json!({ "reason": "unknown_root", "target_host": host }),
            ));
        }

        // A different host. Whether that host is *supposed* to exist is
        // exactly the question #930 was filed about, so answer it here.
        // (A routable peer never reaches this: `call_tool` proxies it.)
        let Some(peers) = self.fleet.declared() else {
            return KaedError::not_found(format!(
                "root {name:?} names host {host:?}, but this instance serves {:?} and \
                 declares no fleet — its config has no [peers] table, so kaed here \
                 knows nothing about {host:?} either way",
                self.host
            ))
            .with_data(data(
                serde_json::json!({ "reason": "fleet_undeclared", "target_host": host }),
            ));
        };

        match peers.iter().find(|p| p.host == host) {
            Some(p) => match p.status {
                PeerStatus::Deferred => KaedError::not_found(format!(
                    "host {host:?} deliberately does not run kaed ({}){} — this is a \
                     recorded decision, not a broken deploy, so do not install one there",
                    p.reference.as_deref().unwrap_or("no ref"),
                    p.note
                        .as_deref()
                        .map(|n| format!(": {n}"))
                        .unwrap_or_default(),
                ))
                .with_data(data(serde_json::json!({
                    "reason": "host_deferred",
                    "target_host": host,
                    "host_status": p.status,
                    "ref": p.reference,
                    "note": p.note,
                }))),
                PeerStatus::Unreachable => KaedError::not_found(format!(
                    "host {host:?} is declared part of the fleet but is not answering \
                     (since {})",
                    p.since.as_deref().unwrap_or("unknown"),
                ))
                .with_data(data(serde_json::json!({
                    "reason": "host_unreachable",
                    "target_host": host,
                    "host_status": p.status,
                    "since": p.since,
                }))),
                // A peer with a `url` is proxied before dispatch ever gets
                // here (010), so reaching this arm means routing was never
                // configured — the peer is declaration only.
                PeerStatus::Active => KaedError::not_found(format!(
                    "host {host:?} is declared active in this host's fleet, but no \
                     `url` is configured for it here, so kaed on {} cannot proxy to \
                     it — connect to {host}'s own kaed, or add `url` under \
                     [peers.{host}] in this host's config",
                    self.host
                ))
                .with_data(data(serde_json::json!({
                    "reason": "peer_routing_unavailable",
                    "target_host": host,
                    "host_status": p.status,
                    "url": p.url,
                }))),
            },
            None => KaedError::not_found(format!(
                "host {host:?} is not in this host's declared fleet; kaed on {} has no \
                 opinion about it",
                self.host
            ))
            .with_data(data(
                serde_json::json!({ "reason": "host_never_declared", "target_host": host }),
            )),
        }
    }

    /// This instance's own entry in the `fleet` block: the one host a
    /// response can always vouch for.
    fn self_fleet_entry(&self) -> FleetHostInfo {
        FleetHostInfo {
            host: self.host.clone(),
            status: "active".into(),
            is_self: true,
            verified: true,
            version: Some(crate::version::FULL.to_string()),
            roots: Some(self.roots.iter().map(|r| r.name.clone()).collect()),
            reference: None,
            note: None,
            since: None,
            url: None,
            probe: None,
        }
    }

    /// A peer's fleet entry when it was NOT probed on this call: the
    /// declaration, unverified, with `probe` saying why nothing was checked.
    fn declared_fleet_entry(p: &Peer, probe: Option<Value>) -> FleetHostInfo {
        FleetHostInfo {
            host: p.host.clone(),
            status: p.status.as_str().into(),
            is_self: false,
            verified: false,
            version: None,
            roots: None,
            reference: p.reference.clone(),
            note: p.note.clone(),
            since: p.since.clone(),
            url: p.url.clone(),
            probe,
        }
    }
}

#[derive(Clone)]
pub struct KaedServer {
    state: Arc<AppState>,
    tool_router: ToolRouter<Self>,
}

impl KaedServer {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            tool_router: Self::tool_router(),
        }
    }
}

// ------------------------------------------------------------ tool params

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatParams {
    /// Root name (see `roots`).
    pub root: String,
    /// Root-relative path; empty for the root itself.
    #[serde(default)]
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListParams {
    pub root: String,
    /// Directory to list, root-relative; empty for the root itself.
    #[serde(default)]
    pub path: String,
    /// Glob over root-relative paths, e.g. `**/*.rs`.
    pub glob: Option<String>,
    /// Recursion depth below `path` (default 1 = immediate children).
    #[serde(default = "default_depth")]
    pub depth: usize,
    /// Max entries returned (default 500); continue via `offset`.
    pub max: Option<usize>,
    #[serde(default)]
    pub offset: usize,
    /// Include gitignored entries.
    #[serde(default)]
    pub ignored: bool,
}

fn default_depth() -> usize {
    1
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RangeParam {
    /// 1-based first line, inclusive.
    pub start: usize,
    /// Last line, inclusive; clamped to EOF.
    pub end: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WindowParam {
    /// Center the window on this 1-based line…
    pub line: Option<usize>,
    /// …or on the unique occurrence of this text.
    pub anchor: Option<String>,
    /// Context lines on each side (default 10).
    #[serde(default = "default_context")]
    pub context: usize,
}

fn default_context() -> usize {
    10
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadParams {
    pub root: String,
    pub path: String,
    /// Explicit line range; mutually exclusive with `window`.
    pub range: Option<RangeParam>,
    /// N context lines around a line or unique anchor — the cheap
    /// "show me where I'm about to edit" read.
    pub window: Option<WindowParam>,
    /// Prefix each line with `<n>\t` (absolute file line numbers).
    #[serde(default)]
    pub numbered: bool,
    /// Response byte budget for this call (capped by the server limit).
    pub max_bytes: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchParams {
    pub root: String,
    pub pattern: String,
    /// `false` searches the pattern as a literal string (default true).
    #[serde(default = "default_true")]
    pub regex: bool,
    /// Glob over root-relative paths, e.g. `**/*.rs`.
    pub glob: Option<String>,
    /// Subtree or single file to search; empty = whole root.
    #[serde(default)]
    pub path: String,
    /// Context lines before/after each match (default 2).
    #[serde(default = "default_search_context")]
    pub context: usize,
    /// Cap on returned matches (default from server config).
    pub max_results: Option<usize>,
}

fn default_true() -> bool {
    true
}

fn default_search_context() -> usize {
    2
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditParams {
    pub root: String,
    /// Version of every file the ops touch (except pure creates), from a
    /// prior read/search/stat. Extra entries act as "assert unchanged".
    #[serde(default)]
    pub base: Vec<BaseVersion>,
    /// Applied in order against evolving buffers; all land or none do.
    pub ops: Vec<EditOp>,
    /// Validate and return the diff without touching disk.
    #[serde(default)]
    pub dry_run: bool,
    /// Include the unified diff (the proof of what changed; default true).
    #[serde(default = "default_true")]
    pub return_diff: bool,
    /// Journaled note on why this edit was made.
    pub intent: Option<String>,
    /// Keys in classified dotenv files whose values this edit may destroy.
    /// A write that makes a value vanish (delete, or overwrite by a new
    /// literal) refuses unless its key is named here — the value may never
    /// have been seen and cannot be restored.
    #[serde(default)]
    pub drop_keys: Vec<String>,
    /// Overrides for a `secret_leak` refusal: the exact `detail` strings it
    /// reported (a digest, a provider prefix, an armor label). Writing
    /// secret-matching content into an unclassified file refuses unless the
    /// match is named here.
    #[serde(default)]
    pub allow_secrets: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JournalParams {
    /// Filter to one root, as *journalled*. Optional: history is host-wide
    /// and legitimately names roots this host no longer serves, so an
    /// unresolvable value filters rather than fails.
    pub root: Option<String>,
    /// Root-relative path — exact, or a directory whose subtree matched.
    pub path: Option<String>,
    /// Journalled identity, e.g. `claude`.
    pub author: Option<String>,
    /// RFC 3339 lower bound on the record's time.
    pub since: Option<String>,
    /// Which record kinds to return; all three by default.
    #[serde(default)]
    pub kind: Vec<RecordKind>,
    /// Max entries (default 20, capped at 200).
    pub max: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiffParams {
    pub root: String,
    pub path: String,
    /// A 16-hex `version`, a `txn_id` (the state that transaction
    /// produced), or `"current"`.
    pub from: String,
    /// Defaults to `"current"`.
    pub to: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevertParams {
    pub root: String,
    /// The transaction to undo, from `journal`.
    pub txn_id: i64,
    #[serde(default)]
    pub dry_run: bool,
    /// Why. Journaled alongside the automatic "revert of txn N".
    pub intent: Option<String>,
    /// As on `edit`: a revert that re-introduces a secret the current
    /// content lacks is a re-leak, and refuses unless the match's `detail`
    /// is named here.
    #[serde(default)]
    pub allow_secrets: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretAction {
    /// Shape, length, digest, and the durable handle — never the value.
    /// This IS `load_secret`: the handle is what a cross-session handoff
    /// carries (011 D-2).
    Describe,
    /// Mint a fresh value server-side and write it. New keys only.
    Generate,
    /// Same shape, new entropy — the old and new value both stay unseen.
    Rotate,
    /// Every entry on this host sealing the same value (digest equality).
    Occurrences,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretParams {
    pub action: SecretAction,
    pub root: String,
    pub path: String,
    pub key: String,
    /// The file's current version — required by `generate` and `rotate`
    /// (R2: every mutation declares its base).
    pub version: Option<String>,
    /// `generate`: required — a name from this host's `[secrets] shapes`
    /// or a spec (`hex(64)`, `base64url(43)`, `uuid4`,
    /// `prefixed(tag,inner)`). `rotate`: optional override when the
    /// current value's shape cannot be detected.
    pub shape: Option<String>,
    /// `generate`: comment block above the new entry.
    pub comment: Option<String>,
    /// `rotate`: extra locations to write the same new value — feed this
    /// from `occurrences`. Targets on other hosts are written via peer
    /// mode, each reported separately (cross-host rotation is not atomic).
    #[serde(default)]
    pub also: Vec<secret_tool::AlsoTarget>,
    /// Journaled, on the transaction and the audit event.
    pub intent: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretRevealParams {
    pub root: String,
    pub path: String,
    pub key: String,
    /// Required — the audit row must say why plaintext was disclosed.
    pub intent: String,
    /// Exact-value semantics: refuse loudly if the value changed since
    /// this digest was taken. Omit for whatever-is-current.
    pub expected_digest: Option<String>,
    /// Names where the value is being carried when this reveal is the
    /// source half of a cross-host write; the audit row records it as a
    /// `transport` with that claimed destination. kaed sets this itself
    /// when resolving `value_from` across hosts.
    pub transport_destination: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackParams {
    /// One sentence. The only required field, deliberately: anything that
    /// costs a thinking step loses to finishing the task.
    pub summary: String,
    /// Defaults to `friction`.
    #[serde(default)]
    pub category: FeedbackCategory,
    /// Anything longer — what you tried, what you did instead.
    pub detail: Option<String>,
    /// What it was about: a txn id, an error code, the call you made.
    pub context: Option<String>,
}

#[derive(Serialize)]
struct RootsResult {
    /// The instance answering this call. Local root names are prefixed
    /// with it; peer roots carry their own hosts' prefixes.
    host: String,
    /// Local entries are [`RootInfo`]; peer entries are whatever the peer's
    /// own `roots` reported, verbatim (D-3) — with `status`/`since`
    /// overridden to `unreachable` when served from cache during an outage.
    roots: Vec<Value>,
    fleet: FleetInfo,
}

#[derive(Serialize)]
struct RootInfo {
    /// Host-qualified — pass this verbatim as `root`.
    name: String,
    host: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    status: &'static str,
    /// The tools that work on this root. Per-root, and the **union** across
    /// a fleet rather than the intersection, so a mid-upgrade fleet never
    /// hides a working feature on its most-updated host.
    capabilities: &'static [&'static str],
}

/// Which hosts should run kaed, and what is known about them — korg #930,
/// answered in the response every MCP client already fetches.
#[derive(Serialize)]
struct FleetInfo {
    /// `false` when this host's config has no `[peers]` table: the fleet is
    /// **undeclared**, `hosts` is this instance alone, and a host's absence
    /// from it means nothing. When `true`, an absent host is one kaed here
    /// has no opinion about — distinct from one declared `deferred`.
    declared: bool,
    hosts: Vec<FleetHostInfo>,
}

#[derive(Serialize)]
struct FleetHostInfo {
    host: String,
    /// `active` | `deferred` | `unreachable`. Observation when `verified`,
    /// declaration otherwise.
    status: String,
    /// True for the instance answering this call.
    #[serde(rename = "self")]
    is_self: bool,
    /// Whether what you see was *observed by this call* (a live probe under
    /// your own credential — 010) rather than read from config. An
    /// observed-down host is `verified: true, status: "unreachable"`: the
    /// check happened, that was its result.
    verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    roots: Option<Vec<String>>,
    /// Why this host is what it is — a korg reference. Always present on
    /// `deferred`: a deliberate absence with no reasoning behind it is the
    /// gap #930 was filed about.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// When an `unreachable` host was last known good — observed when this
    /// process watched it go down, declared otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    /// What happened when this call tried to check: `{status: "ok"}`,
    /// `{status: "failed", detail}`, or `{status: "skipped", detail}` (no
    /// url, or no credential for the calling author). Absent on `self` and
    /// on `deferred` peers, which are deliberately never probed.
    #[serde(skip_serializing_if = "Option::is_none")]
    probe: Option<Value>,
}

// ------------------------------------------------------------------ tools

#[tool_router]
impl KaedServer {
    #[tool(
        description = "List the workspace roots this fleet serves, and what is known about each host. Root names are host-qualified (`kai:src`) — pass one verbatim as `root`, with a root-relative `path`. Peers with configured routing are probed live under YOUR identity: their roots appear here and are directly addressable (calls are proxied, journaled on the target under your name), and a declared host that is not answering appears as status `unreachable` with `since` — a fact to reason about, not a wiring failure. A host declared `deferred` is deliberately not running kaed (`ref` says why) and must not be \"fixed\"; `fleet.declared: false` means this host declares no fleet at all, so absence from the list means nothing."
    )]
    async fn roots(
        &self,
        Extension(parts): Extension<http::request::Parts>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = author_of(&parts)?;
        let state = self.state.clone();

        let mut root_entries: Vec<Value> = Vec::new();
        for r in &state.roots {
            let info = RootInfo {
                name: r.name.clone(),
                host: r.host.clone(),
                path: r.path.display().to_string(),
                description: r.description.clone(),
                status: "active",
                capabilities: ROOT_CAPABILITIES,
            };
            root_entries.push(
                serde_json::to_value(info).map_err(|e| {
                    ErrorData::internal_error(format!("serializing roots: {e}"), None)
                })?,
            );
        }

        let mut hosts = vec![state.self_fleet_entry()];
        let declared = state.fleet.declared().map(<[Peer]>::to_vec);
        if let Some(peers) = &declared {
            // Probe every routable peer in parallel, under the caller's own
            // credential — the gateway never holds a shared identity (PD-4).
            let mut probes = tokio::task::JoinSet::new();
            for peer in peers {
                if peer.status == PeerStatus::Deferred
                    || peer.url.is_none()
                    || state.fleet.token_for(&peer.host, &author.0).is_none()
                {
                    continue;
                }
                let (fleet, peer, author) = (state.fleet.clone(), peer.clone(), author.0.clone());
                probes.spawn(async move {
                    let outcome = fleet.probe_roots(&peer, &author).await;
                    (peer.host, outcome)
                });
            }
            let mut outcomes: std::collections::HashMap<String, Result<Value, KaedError>> =
                std::collections::HashMap::new();
            while let Some(joined) = probes.join_next().await {
                if let Ok((host, outcome)) = joined {
                    outcomes.insert(host, outcome);
                }
            }

            for peer in peers {
                hosts.push(match outcomes.remove(&peer.host) {
                    // Not probed: deferred stays a bare declaration; the
                    // others say why nothing was checked.
                    None if peer.status == PeerStatus::Deferred => {
                        AppState::declared_fleet_entry(peer, None)
                    }
                    None if peer.url.is_none() => AppState::declared_fleet_entry(
                        peer,
                        Some(json!({"status": "skipped", "detail": "no url declared"})),
                    ),
                    None => AppState::declared_fleet_entry(
                        peer,
                        Some(json!({
                            "status": "skipped",
                            "detail": format!("no credential for author {:?}", author.0),
                        })),
                    ),
                    Some(Ok(payload)) => {
                        if peer.status == PeerStatus::Unreachable {
                            tracing::warn!(
                                peer = peer.host,
                                "declared unreachable but answering — update [peers] \
                                 in config.toml"
                            );
                        }
                        let names: Vec<String> = payload
                            .get("roots")
                            .and_then(Value::as_array)
                            .map(|rs| {
                                rs.iter()
                                    .filter_map(|r| r.get("name")?.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Some(rs) = payload.get("roots").and_then(Value::as_array) {
                            // The peer's entries verbatim — each carries its
                            // own capabilities, which is how the fleet
                            // advertises the union, never the intersection.
                            root_entries.extend(rs.iter().cloned());
                        }
                        FleetHostInfo {
                            host: peer.host.clone(),
                            status: "active".into(),
                            is_self: false,
                            verified: true,
                            version: state.fleet.sight_of(&peer.host).version,
                            roots: Some(names),
                            reference: peer.reference.clone(),
                            note: peer.note.clone(),
                            since: None,
                            url: peer.url.clone(),
                            probe: Some(json!({"status": "ok"})),
                        }
                    }
                    Some(Err(e)) if e.code == crate::errors::ErrorCode::NotFound => {
                        // Did not answer: unreachable, observed. Cached
                        // roots (if any) stay visible, marked as such —
                        // the namespace outlives the outage (D-4).
                        let sight = state.fleet.sight_of(&peer.host);
                        let since = sight
                            .down_since
                            .map(fleet::rfc3339)
                            .or_else(|| peer.since.clone());
                        let names = sight.roots.as_ref().map(|rs| {
                            rs.iter()
                                .filter_map(|r| r.get("name")?.as_str().map(String::from))
                                .collect()
                        });
                        for cached in sight.roots.iter().flatten() {
                            let mut entry = cached.clone();
                            if let Some(obj) = entry.as_object_mut() {
                                obj.insert("status".into(), json!("unreachable"));
                                obj.insert("since".into(), json!(since));
                            }
                            root_entries.push(entry);
                        }
                        FleetHostInfo {
                            host: peer.host.clone(),
                            status: "unreachable".into(),
                            is_self: false,
                            verified: true,
                            version: sight.version,
                            roots: names,
                            reference: peer.reference.clone(),
                            note: peer.note.clone(),
                            since,
                            url: peer.url.clone(),
                            probe: Some(json!({"status": "failed", "detail": e.message})),
                        }
                    }
                    Some(Err(e)) => {
                        // The host answered but the probe still failed
                        // (credential rejected, protocol error): reachable,
                        // just not checkable — the declaration stands.
                        AppState::declared_fleet_entry(
                            peer,
                            Some(json!({"status": "failed", "detail": e.message})),
                        )
                    }
                });
            }
        }

        ok(serde_json::to_value(RootsResult {
            host: state.host.clone(),
            roots: root_entries,
            fleet: FleetInfo {
                declared: declared.is_some(),
                hosts,
            },
        })
        .map_err(|e| ErrorData::internal_error(format!("serializing roots: {e}"), None))?)
    }

    #[tool(
        description = "Stat a file, directory, or symlink. For files returns the content `version` — the cheap staleness probe: compare against a version you hold to see if a re-read is needed."
    )]
    async fn stat(
        &self,
        Parameters(p): Parameters<StatParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            fsops::stat(&root, &p.path, &state.limits)
        })
        .await
    }

    #[tool(
        description = "List directory entries (gitignore-aware; `.git` always hidden). Budgeted: `truncated: true` + `next_offset` mean more entries exist — continue by passing `offset`. `denied_hidden` / `unreadable_hidden` count what policy and the OS kept out, so a filtered listing is never mistaken for the whole directory."
    )]
    async fn list(
        &self,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            fsops::list(
                &root,
                &fsops::ListParams {
                    path: &p.path,
                    glob: p.glob.as_deref(),
                    depth: p.depth,
                    max: p.max.unwrap_or(DEFAULT_LIST_MAX),
                    offset: p.offset,
                    ignored: p.ignored,
                },
            )
        })
        .await
    }

    #[tool(
        description = "Read a file: whole (capped), a line `range`, or a `window` of context around a line or unique anchor string. Always returns the whole file's `version` — usable directly as an edit base. Truncation is explicit (`truncated`, `next_offset`)."
    )]
    async fn read(
        &self,
        Parameters(p): Parameters<ReadParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            let mode = match (&p.range, &p.window) {
                (Some(_), Some(_)) => {
                    return Err(KaedError::invalid_input(
                        "pass `range` or `window`, not both",
                    ));
                }
                (Some(r), None) => ReadMode::Range {
                    start: r.start,
                    end: r.end,
                },
                (None, Some(w)) => match (&w.line, &w.anchor) {
                    (Some(line), None) => ReadMode::WindowLine {
                        line: *line,
                        context: w.context,
                    },
                    (None, Some(anchor)) => ReadMode::WindowAnchor {
                        anchor,
                        context: w.context,
                    },
                    _ => {
                        return Err(KaedError::invalid_input(
                            "window needs exactly one of `line` or `anchor`",
                        ));
                    }
                },
                (None, None) => ReadMode::Whole,
            };
            let result = fsops::read(
                &root,
                &p.path,
                &mode,
                p.numbered,
                p.max_bytes,
                &state.limits,
            )?;
            // A redacted read feeds the known-digest index (012 D-4): the
            // rendering already disclosed these digests, so indexing them
            // discloses nothing further — it is what lets the write path
            // recognize a hand-provisioned secret kaed never wrote. Not a
            // read journal: no author, no event, no timestamp beyond
            // first_seen (009 D-2 stays intact).
            if result.redacted {
                state.journal.record_digests(
                    &root.name,
                    &p.path,
                    &crate::secrets::extract_placeholder_digests(&result.content),
                );
            }
            Ok(result)
        })
        .await
    }

    #[tool(
        description = "Ripgrep-grade search. Each match carries its file's `version`, so search → edit is safe with no read in between (a stale hit becomes version_conflict, never a wrong edit). `root` also takes a PATTERN (`*:*`, `*:src`, `kai:*`) for fleet-wide search in one call: every matching root on every reachable host is searched in parallel under one `max_results` budget, matches gain a `root` tag, `fanout` reports each root's own files_searched/truncation (plus what the merge dropped), and hosts that could not be searched are listed in `hosts_unavailable` — never silently skipped (a pattern is always expanded by the host you asked, so `kubsdb:*` reports YOUR fleet's reachability, not the peer's). Entries the OS refuses — an undescendable directory, an unopenable file — are skipped and counted in `unreadable_hidden`, never fatal. The hidden counters describe what the search actually reached, so when `truncated` is true they are lower bounds: read them together."
    )]
    async fn search(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
        if fleet::is_pattern(&p.root) {
            let author = author_of(&parts)?;
            return self.fleet_search(author, p).await;
        }
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            let max = p
                .max_results
                .unwrap_or(state.limits.search_max_results)
                .min(SEARCH_MAX_RESULTS_CEILING);
            search::search(
                &root,
                &search::SearchParams {
                    pattern: &p.pattern,
                    regex: p.regex,
                    glob: p.glob.as_deref(),
                    path: &p.path,
                    context: p.context,
                    max_results: max,
                },
                &state.limits,
            )
        })
        .await
    }

    #[tool(
        description = "Transactional edit: anchor_replace / range_replace / create ops, plus env_set / env_rename / env_delete / env_reorder for dotenv-shaped files — multi-file, atomic, all land or none do. Every non-create path must appear in `base` with its version; a mismatch fails with version_conflict carrying a delta of what changed. The returned diff is proof of what was applied: no verification read needed. Classified (secret-bearing) files take only env ops; placeholders from a redacted read pass through as values verbatim and kaed substitutes the real value on write. A write that would destroy a value requires naming its key in `drop_keys`. Writes into UNclassified files are scanned for leaking secrets: content matching a known secret's digest, a provider token prefix, or a private-key block refuses with `reason: secret_leak` naming the exact `allow_secrets` override to pass if the write is deliberate (reference the variable instead of the value where you can); merely secret-shaped content applies with a warning. Supports dry_run — and dry_run models WRITABILITY, so a path the service identity cannot write refuses on the dry run too rather than returning a diff for a write that cannot land."
    )]
    async fn edit(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<EditParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = author_of(&parts)?;
        let state = self.state.clone();
        // Cross-root `value_from` resolves before the engine (011 D-5):
        // another local root by direct read, another host via that host's
        // secret_reveal under the caller's identity. Same-root references
        // stay for the engine, which resolves them against the evolving
        // buffers.
        let mut ops = p.ops;
        for op in &mut ops {
            if let EditOp::EnvSet {
                path,
                value,
                value_from,
                ..
            } = op
                && value_from
                    .as_ref()
                    .is_some_and(|vf| vf.root.as_deref().is_some_and(|r| r != p.root))
            {
                let vf = value_from.take().expect("checked above");
                let destination = format!("{}/{path}", p.root);
                match self
                    .fetch_secret_value(&author, &vf, &destination, false)
                    .await
                {
                    Ok(fetched) => *value = Some(fetched.to_string()),
                    Err(e) => return kaed_error_result(e),
                }
            }
        }
        run(move || {
            let root = state.root(&p.root)?;
            let req = EditRequest {
                base: p.base,
                ops,
                dry_run: p.dry_run,
                return_diff: p.return_diff,
                intent: p.intent,
                drop_keys: p.drop_keys,
                allow_secrets: p.allow_secrets,
            };
            txn::apply(&root, &req, &state.limits, &author.0, &state.journal)
        })
        .await
    }

    // -------------------------------------------- secret lifecycle (011)

    #[tool(
        description = "The secret lifecycle for dotenv-shaped files — four actions, none of which ever returns a value. `describe`: shape, length, digest and the durable HANDLE (host-qualified root + path + key + digest) — the reference a cross-session or cross-host handoff should carry instead of the value. `generate`: kaed mints a fresh value server-side (shape from a named registry or a closed spec grammar: hex(N), base64url(N), uuid4, prefixed(tag,inner)) and writes it; you get a placeholder, never the plaintext. `rotate`: same shape, new entropy — rotate a token without ever seeing old or new; `also` writes the same new value to more locations (found via `occurrences`), including on other hosts through the gateway. `occurrences`: every entry on this host holding the same value, by digest equality. generate/rotate require the file's current `version` and are journaled as ordinary transactions plus a secrets audit event; read the audit stream back with journal kind \"secret\"."
    )]
    async fn secret(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<SecretParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = author_of(&parts)?;
        if matches!(p.action, SecretAction::Rotate) {
            return self.secret_rotate(author, p).await;
        }
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            let ctx = secret_tool::Ctx {
                root: &root,
                roots: &state.roots,
                limits: &state.limits,
                author: &author.0,
                journal: &state.journal,
                secrets: &state.secrets,
            };
            match p.action {
                SecretAction::Describe => {
                    serde_json::to_value(secret_tool::describe(&ctx, &p.path, &p.key)?)
                }
                SecretAction::Occurrences => {
                    serde_json::to_value(secret_tool::occurrences(&ctx, &p.path, &p.key)?)
                }
                SecretAction::Generate => {
                    let version = p.version.as_deref().ok_or_else(|| {
                        KaedError::invalid_input(
                            "generate needs `version` — the file's current version, from a \
                             read or stat (R2: every mutation declares its base)",
                        )
                    })?;
                    let shape = p.shape.as_deref().ok_or_else(|| {
                        KaedError::invalid_input(
                            "generate needs `shape`: a [secrets] shapes name or a spec \
                             (hex(64), base64url(43), uuid4, prefixed(tag,inner))",
                        )
                    })?;
                    serde_json::to_value(secret_tool::generate(
                        &ctx,
                        &secret_tool::GenerateParams {
                            path: &p.path,
                            key: &p.key,
                            version,
                            shape,
                            comment: p.comment.as_deref(),
                            intent: p.intent.as_deref(),
                        },
                    )?)
                }
                SecretAction::Rotate => unreachable!("handled above"),
            }
            .map_err(|e| KaedError::internal(format!("serializing secret result: {e}")))
        })
        .await
    }

    #[tool(
        description = "Reveal one secret value in plaintext — the escape hatch, deliberately its own tool so it can be permissioned separately from everything else. Requires `intent` (why the disclosure is needed); every reveal is journaled in the secrets audit stream with your identity, and the response's `disclosed: true` is something to surface to the human you work for. You rarely need this: to USE a value in a shell, source the file (`set -a; . .env; set +a`); to copy or move it, env_set takes placeholders and value_from handles; to mint or replace it, the `secret` tool never shows you anything. Reveal is for values that must leave the kaed-writable world entirely (a client config on a host kaed does not serve, a vendor console). Pass `expected_digest` for exact-value semantics — a changed value then refuses loudly instead of revealing whatever is current."
    )]
    async fn secret_reveal(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<SecretRevealParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = author_of(&parts)?;
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            let ctx = secret_tool::Ctx {
                root: &root,
                roots: &state.roots,
                limits: &state.limits,
                author: &author.0,
                journal: &state.journal,
                secrets: &state.secrets,
            };
            secret_tool::reveal(
                &ctx,
                &secret_tool::RevealParams {
                    path: &p.path,
                    key: &p.key,
                    intent: &p.intent,
                    expected_digest: p.expected_digest.as_deref(),
                    transport_destination: p.transport_destination.as_deref(),
                },
            )
        })
        .await
    }

    // -------------------------------------------------------- history (R6)

    #[tool(
        description = "Read this host's durable history: applied write transactions, failed write attempts, and friction reports, merged newest-first. `root` is an optional FILTER, not an address — omit it for everything this host did. Every response carries `coverage`, which states what this history cannot see (reads are never journaled) and when failure records actually begin, so an empty window is never mistaken for a quiet one. Rows naming a root this host no longer serves are labelled `historical` rather than rewritten."
    )]
    async fn journal(
        &self,
        Parameters(p): Parameters<JournalParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        run(move || {
            history::journal(
                &state.journal,
                &state.roots,
                &state.host,
                &JournalQuery {
                    root: p.root.as_deref(),
                    path: p.path.as_deref(),
                    author: p.author.as_deref(),
                    since: p.since.as_deref(),
                    kinds: p.kind,
                    max: p.max.unwrap_or(history::DEFAULT_MAX_ENTRIES),
                },
            )
        })
        .await
    }

    #[tool(
        description = "Diff a file between two states. Each of `from`/`to` is a 16-hex `version`, a `txn_id` (the state that transaction produced — `journal` gives you a file's old_version and new_version if you want what one transaction did), or \"current\" (the default for `to`). Content comes from the journal's blob store, whose retention is finite: a version past the window is named but not renderable, and the error says so. Classified files diff as redacted renderings, always."
    )]
    async fn diff(
        &self,
        Parameters(p): Parameters<DiffParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            let from = history::parse_side(&p.from)?;
            let to = history::parse_side(p.to.as_deref().unwrap_or("current"))?;
            history::diff(&root, &p.path, &from, &to, &state.journal, &state.limits)
        })
        .await
    }

    #[tool(
        description = "Undo a journaled transaction by applying its pre-image as a NEW transaction — history is never rewritten, and the revert is itself journaled and itself revertible. It goes through the same versioning contract as any edit: if a file moved since, you get version_conflict with a delta, not a force-overwrite. Refuses, with a reason, when the transaction ran under a root this host no longer serves, when it created a file (undoing a create needs a delete op kaed does not have yet), or when the file is classified (its journaled pre-image is a redacted rendering, so kaed does not hold the bytes to restore). Supports dry_run."
    )]
    async fn revert(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<RevertParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = author_of(&parts)?;
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            history::revert(
                &root,
                &history::RevertRequest {
                    txn_id: p.txn_id,
                    dry_run: p.dry_run,
                    intent: p.intent.as_deref(),
                    author: &author.0,
                    allow_secrets: &p.allow_secrets,
                },
                &state.journal,
                &state.limits,
                &state.host,
                &state.roots,
            )
        })
        .await
    }

    #[tool(
        description = "Tell kaed it got in your way. One required field: `summary`, one sentence. Call it the moment something costs you a detour — a refusal with no way forward, an error whose data was not enough, a call that succeeded and gave you a wrong answer — and especially before you route around kaed via ssh, because that is the failure this channel exists to catch and the journal cannot see it. Filed into this host's journal beside the transactions and failures it is about; read it back with `journal`."
    )]
    async fn feedback(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<FeedbackParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = author_of(&parts)?;
        let state = self.state.clone();
        run(move || {
            history::feedback(
                &state.journal,
                &author.0,
                p.category,
                &p.summary,
                p.detail.as_deref(),
                p.context.as_deref(),
            )
        })
        .await
    }
}

impl KaedServer {
    /// Is this call addressed to a root another host serves, with routing
    /// configured for it? Decided on the RAW argument (D-1): the typed
    /// param structs never see a proxied payload, so a newer peer's extra
    /// fields pass through instead of being rejected by an older gateway's
    /// schema.
    fn remote_target(&self, request: &CallToolRequestParams) -> Option<(Peer, String)> {
        let root = request.arguments.as_ref()?.get("root")?.as_str()?;
        // A PATTERN is always expanded by the instance that was asked
        // (014 D-5, korg #1089). `kubsdb:*` has host prefix `kubsdb`, so
        // D-1's routing rule used to forward the whole call — and the peer
        // ran the fan-out, returning ITS world-model (`hosts_unavailable`:
        // kai and kubs0 `no_credential`) to a caller for whom that is
        // false. Results were fine; the one field whose job is stopping an
        // agent forming a wrong picture was corrupted, in the direction of
        // claiming a gap that does not exist. `kai:*` and `*:*` were never
        // affected because they already expanded locally; this makes all
        // three the same call.
        if fleet::is_pattern(root) {
            return None;
        }
        let (host, _) = root.split_once(':')?;
        if host == self.state.host {
            return None;
        }
        let peer = self.state.fleet.routable(host)?.clone();
        Some((peer, root.to_string()))
    }

    async fn proxy_to_peer(
        &self,
        peer: Peer,
        root: String,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(parts) = context.extensions.get::<http::request::Parts>() else {
            return Err(ErrorData::internal_error("no http parts on request", None));
        };
        let author = author_of(parts)?;
        tracing::debug!(
            tool = %request.name,
            root,
            peer = peer.host,
            author = author.0,
            "proxying to peer"
        );
        let mut result = match self
            .state
            .fleet
            .call(
                &peer,
                &author.0,
                &request.name,
                request.arguments,
                Some(&root),
            )
            .await
        {
            Ok(result) => result,
            Err(e) => kaed_error_result(e)?,
        };
        // A peer envelope is verbatim in its *content* (R10), not in its
        // protocol framing — the two ends of this hop can be on different
        // revisions, and translating between them is the gateway's job in
        // BOTH directions (016 D-4).
        //
        // rmcp strips `resultType: "complete"` for legacy peers, but only
        // for results it built itself; one that arrived from a peer already
        // deserialized is nobody's to stamp. So a peer asked at
        // `PEER_PROTOCOL_VERSION` correctly omits the field, and returning
        // that unchanged onto a 2026-07-28 session — where the field is
        // mandatory and the absent-means-complete bridge is explicitly
        // unavailable — makes the client reject the whole result, outcome
        // and all.
        //
        // Filling it in is faithful rather than inventive: absent *means*
        // complete at the revision the peer answered, and any other value
        // is gated to sessions the peer never negotiated. Only an absent
        // one is filled; a peer that names its own discriminator keeps it.
        if result.result_type.is_none()
            && context
                .protocol_version()
                .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str())
        {
            result.result_type = Some(ResultType::COMPLETE);
        }
        Ok(result)
    }

    /// `secret` / `rotate`: the primary and every same-root `also` target
    /// land in one local transaction; `also` targets on OTHER hosts are
    /// then written via peer mode under the caller's identity — PD-3's
    /// rotate-both-places path. Not atomic across hosts, and the response
    /// says per-target what landed.
    async fn secret_rotate(
        &self,
        author: Author,
        p: SecretParams,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        let Some(version) = p.version.clone() else {
            return kaed_error_result(KaedError::invalid_input(
                "rotate needs `version` — the file's current version, from a read or \
                 stat (R2: every mutation declares its base)",
            ));
        };
        // Three target classes, partitioned on the axis that decides how a
        // write lands (#1231 — this used to compare whole root strings, so
        // a same-host sibling root was classed remote and the peer path
        // asked the executing host for itself as a peer):
        //   same root             → joins the primary's atomic transaction;
        //   same host, other root → a local write in its OWN transaction
        //                           (single-root by schema, 020 D-3);
        //   other host            → proxied to that peer under the caller.
        let (local_also, elsewhere): (Vec<_>, Vec<_>) = p
            .also
            .into_iter()
            .partition(|t| t.root.as_deref().is_none_or(|r| r == p.root));
        let (same_host_also, remote_also): (Vec<_>, Vec<_>) =
            elsewhere.into_iter().partition(|t| {
                t.root
                    .as_deref()
                    .is_some_and(|r| r.split_once(':').is_some_and(|(h, _)| h == self.state.host))
            });

        let (mut result, value) = {
            let state = state.clone();
            let author = author.0.clone();
            let (root_name, path, key) = (p.root.clone(), p.path.clone(), p.key.clone());
            let (shape, intent) = (p.shape.clone(), p.intent.clone());
            let joined = tokio::task::spawn_blocking(move || {
                let root = state.root(&root_name)?;
                let ctx = secret_tool::Ctx {
                    root: &root,
                    roots: &state.roots,
                    limits: &state.limits,
                    author: &author,
                    journal: &state.journal,
                    secrets: &state.secrets,
                };
                secret_tool::rotate_local(
                    &ctx,
                    &secret_tool::RotateParams {
                        path: &path,
                        key: &key,
                        version: &version,
                        shape: shape.as_deref(),
                        also: &local_also,
                        intent: intent.as_deref(),
                    },
                )
            })
            .await
            .map_err(|e| ErrorData::internal_error(format!("task panicked: {e}"), None))?;
            match joined {
                Ok(v) => v,
                Err(e) => return kaed_error_result(e),
            }
        };

        for t in same_host_also {
            let target_root = t.root.clone().expect("same-host targets carry a root");
            let key = t.key.as_deref().unwrap_or(&p.key).to_owned();
            let joined = {
                let state = state.clone();
                let author = author.0.clone();
                let (root_name, path, key) = (target_root.clone(), t.path.clone(), key.clone());
                let (version, intent) = (t.version.clone(), p.intent.clone());
                let value = value.clone();
                tokio::task::spawn_blocking(move || {
                    let root = state.root(&root_name)?;
                    let ctx = secret_tool::Ctx {
                        root: &root,
                        roots: &state.roots,
                        limits: &state.limits,
                        author: &author,
                        journal: &state.journal,
                        secrets: &state.secrets,
                    };
                    secret_tool::write_value_local(
                        &ctx,
                        &path,
                        &key,
                        &version,
                        &value,
                        intent.as_deref(),
                    )
                })
                .await
                .map_err(|e| ErrorData::internal_error(format!("task panicked: {e}"), None))?
            };
            let (applied, error, txn_id) = match joined {
                Ok(txn_id) => {
                    // A second `rotate` row on this host, never `transport`:
                    // the value never left it (020 D-2).
                    let _ = state.journal.add_secret_event(&SecretEvent {
                        author: &author.0,
                        action: "rotate",
                        root: &target_root,
                        path: &t.path,
                        key: &key,
                        old_digest: result.old_digest.as_deref(),
                        new_digest: result.new_digest.as_deref(),
                        disclosed: false,
                        destination: None,
                        txn_id,
                        intent: p
                            .intent
                            .as_deref()
                            .map(secrets::redact_free_text)
                            .as_deref(),
                    });
                    (true, None, txn_id)
                }
                Err(e) => (
                    false,
                    Some(serde_json::to_value(&e).unwrap_or_default()),
                    None,
                ),
            };
            result.targets.push(secret_tool::RotatedTarget {
                root: target_root,
                path: t.path,
                key,
                applied,
                error,
                txn_id,
            });
        }

        for t in remote_also {
            let target_root = t.root.clone().expect("remote targets carry a root");
            let key = t.key.as_deref().unwrap_or(&p.key).to_owned();
            let outcome = self
                .write_value_to_peer(
                    &author,
                    &target_root,
                    &t.path,
                    &key,
                    &t.version,
                    &value,
                    p.intent.as_deref(),
                )
                .await;
            let (applied, error) = match outcome {
                Ok(()) => {
                    // The value left this host toward that one: a
                    // transport event on the sender, the redacted write
                    // journaled on the target (D-5, D-6).
                    let _ = state.journal.add_secret_event(&SecretEvent {
                        author: &author.0,
                        action: "transport",
                        root: &result.handle.root,
                        path: &p.path,
                        key: &p.key,
                        old_digest: result.old_digest.as_deref(),
                        new_digest: result.new_digest.as_deref(),
                        disclosed: true,
                        destination: Some(&format!("{target_root}/{}", t.path)),
                        txn_id: result.txn_id,
                        intent: p.intent.as_deref(),
                    });
                    (true, None)
                }
                Err(e) => (false, Some(serde_json::to_value(&e).unwrap_or_default())),
            };
            result.targets.push(secret_tool::RotatedTarget {
                root: target_root,
                path: t.path,
                key,
                applied,
                error,
                txn_id: None, // journaled on the target's own host
            });
        }
        drop(value); // zeroized (PD-3)

        ok(serde_json::to_value(result)
            .map_err(|e| ErrorData::internal_error(format!("serializing rotate: {e}"), None))?)
    }

    /// Write one value into a peer's dotenv file as an ordinary proxied
    /// `edit` under the caller's identity — the receiving host journals a
    /// redacted transaction exactly as if the agent had called it there.
    #[allow(clippy::too_many_arguments)]
    async fn write_value_to_peer(
        &self,
        author: &Author,
        target_root: &str,
        path: &str,
        key: &str,
        version: &str,
        value: &str,
        intent: Option<&str>,
    ) -> Result<(), KaedError> {
        let Some((host, _)) = target_root.split_once(':') else {
            return Err(self.state.explain_unknown_root(target_root));
        };
        let Some(peer) = self.state.fleet.routable(host).cloned() else {
            return Err(self.state.explain_unknown_root(target_root));
        };
        let mut args = serde_json::Map::new();
        args.insert("root".into(), json!(target_root));
        args.insert("base".into(), json!([{"path": path, "version": version}]));
        args.insert(
            "ops".into(),
            json!([{"op": "env_set", "path": path, "key": key, "value": value}]),
        );
        args.insert("drop_keys".into(), json!([key]));
        args.insert(
            "intent".into(),
            json!(
                intent.map(str::to_owned).unwrap_or_else(|| format!(
                    "rotate {key} (propagated from {})",
                    self.state.host
                ))
            ),
        );
        let result = self
            .state
            .fleet
            .call(&peer, &author.0, "edit", Some(args), Some(target_root))
            .await?;
        if result.is_error == Some(true) {
            return Err(kaed_error_from_value(
                result.structured_content.unwrap_or_default(),
            ));
        }
        Ok(())
    }

    /// Fetch the value a cross-root `value_from` names (011 D-5). A local
    /// root is read directly through every policy layer; a peer root goes
    /// through that host's `secret_reveal` under the caller's identity, so
    /// the SOURCE host journals the disclosure. `leaves_host` = the write
    /// target is on another host, in which case a locally-sourced value
    /// journals a `transport` event here before it goes.
    async fn fetch_secret_value(
        &self,
        author: &Author,
        vf: &ValueFrom,
        destination: &str,
        leaves_host: bool,
    ) -> Result<Zeroizing<String>, KaedError> {
        let src_root = vf.root.clone().expect("caller checked value_from.root");
        if let Some(root) = self
            .state
            .roots
            .iter()
            .find(|r| r.name == src_root)
            .cloned()
        {
            let state = self.state.clone();
            let (path, key, digest) = (vf.path.clone(), vf.key.clone(), vf.digest.clone());
            let value = tokio::task::spawn_blocking(move || -> Result<_, KaedError> {
                let loaded = fsops::load_text(&root, &path, &state.limits)?;
                let file = match loaded.secrecy {
                    fsops::Secrecy::ClassifiedDotenv { file, .. } => file,
                    fsops::Secrecy::Plain => {
                        crate::dotenv::parse(&loaded.content).ok_or_else(|| {
                            KaedError::invalid_input(format!(
                                "value_from source {path} is not dotenv-shaped"
                            ))
                        })?
                    }
                };
                let entry = file.get(&key).ok_or_else(|| {
                    KaedError::not_found(format!("value_from source {path} has no key {key:?}"))
                })?;
                if let Some(d) = &digest
                    && secrets::digest_of(&entry.value) != *d
                {
                    return Err(KaedError::invalid_input(format!(
                        "value_from digest mismatch for {key:?} in {path} — the value \
                         changed since that handle was taken; re-describe it (omit \
                         `digest` for current-value semantics)"
                    ))
                    .with_data(serde_json::json!({
                        "reason": "digest_mismatch",
                        "path": path,
                        "key": key,
                        "expected_digest": d,
                    })));
                }
                Ok(Zeroizing::new(entry.value.clone()))
            })
            .await
            .map_err(|e| KaedError::internal(format!("task panicked: {e}")))??;
            if leaves_host {
                let _ = self.state.journal.add_secret_event(&SecretEvent {
                    author: &author.0,
                    action: "transport",
                    root: &src_root,
                    path: &vf.path,
                    key: &vf.key,
                    old_digest: None,
                    new_digest: secrets::clears_floor(&value)
                        .then(|| secrets::digest_of(&value))
                        .as_deref(),
                    disclosed: true,
                    destination: Some(destination),
                    txn_id: None,
                    intent: None,
                });
            }
            return Ok(value);
        }

        // Not a local root: the source host reveals it to kaed (never to
        // the agent) and journals the transport on its side (PD-3, PD-4).
        let Some((host, _)) = src_root.split_once(':') else {
            return Err(self.state.explain_unknown_root(&src_root));
        };
        let Some(peer) = self.state.fleet.routable(host).cloned() else {
            return Err(self.state.explain_unknown_root(&src_root));
        };
        let mut args = serde_json::Map::new();
        args.insert("root".into(), json!(src_root));
        args.insert("path".into(), json!(vf.path));
        args.insert("key".into(), json!(vf.key));
        args.insert(
            "intent".into(),
            json!(format!("cross-host value_from toward {destination}")),
        );
        if let Some(d) = &vf.digest {
            args.insert("expected_digest".into(), json!(d));
        }
        args.insert("transport_destination".into(), json!(destination));
        let result = self
            .state
            .fleet
            .call(
                &peer,
                &author.0,
                "secret_reveal",
                Some(args),
                Some(&src_root),
            )
            .await?;
        if result.is_error == Some(true) {
            return Err(kaed_error_from_value(
                result.structured_content.unwrap_or_default(),
            ));
        }
        let value = result
            .structured_content
            .as_ref()
            .and_then(|v| v.get("value"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                KaedError::internal(format!(
                    "peer {host:?} answered secret_reveal without a value"
                ))
            })?;
        Ok(Zeroizing::new(value.to_owned()))
    }

    /// The proxy-path half of D-5: before forwarding an `edit` to a peer,
    /// resolve every `value_from` naming a root that peer does not serve —
    /// surgically, on the raw JSON (D-1 of 010: everything else passes
    /// byte-for-byte), substituting the fetched value in memory.
    async fn resolve_proxied_value_froms(
        &self,
        author: &Author,
        target_host: &str,
        target_root: &str,
        args: &mut rmcp::model::JsonObject,
    ) -> Result<(), KaedError> {
        let Some(ops) = args.get_mut("ops").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        for op in ops {
            let Some(obj) = op.as_object_mut() else {
                continue;
            };
            if obj.get("op").and_then(Value::as_str) != Some("env_set") {
                continue;
            }
            let Some(vf_value) = obj.get("value_from") else {
                continue;
            };
            let src_root = vf_value.get("root").and_then(Value::as_str);
            // No root, or a root on the target host: the peer resolves it.
            if src_root.is_none_or(|r| r.split_once(':').is_some_and(|(h, _)| h == target_host)) {
                continue;
            }
            let vf: ValueFrom = serde_json::from_value(vf_value.clone())
                .map_err(|e| KaedError::invalid_input(format!("malformed value_from: {e}")))?;
            let op_path = obj
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let destination = format!("{target_root}/{op_path}");
            let fetched = self
                .fetch_secret_value(author, &vf, &destination, true)
                .await?;
            obj.insert("value".into(), json!(*fetched));
            obj.remove("value_from");
        }
        Ok(())
    }

    /// Fleet-wide search (D-5): expand the root pattern over this host's
    /// roots plus every reachable peer's (live probe, caller's credential),
    /// search each concrete root in parallel, merge under one budget with
    /// per-root reporting.
    async fn fleet_search(
        &self,
        author: Author,
        p: SearchParams,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.state.clone();
        let max = p
            .max_results
            .unwrap_or(state.limits.search_max_results)
            .min(SEARCH_MAX_RESULTS_CEILING);
        let matcher = match globset::Glob::new(&p.root) {
            Ok(g) => g.compile_matcher(),
            Err(e) => {
                return kaed_error_result(KaedError::invalid_input(format!(
                    "root pattern {:?} is not a valid glob: {e}",
                    p.root
                )));
            }
        };

        let local: Vec<ResolvedRoot> = state
            .roots
            .iter()
            .filter(|r| matcher.is_match(&r.name))
            .cloned()
            .collect();
        let mut known: Vec<String> = state.roots.iter().map(|r| r.name.clone()).collect();
        let mut unavailable: Vec<Value> = Vec::new();
        let mut remote: Vec<(Peer, String)> = Vec::new();

        if let Some(peers) = state.fleet.declared() {
            let mut probes = tokio::task::JoinSet::new();
            for peer in peers {
                // A host the pattern excludes by name is not part of this
                // search, so it is neither probed nor reported: listing
                // `kubs0: no_credential` under `kubsdb:*` is the same lie
                // #1089 filed, just told by the gateway instead of the peer.
                if !fleet::pattern_admits_host(&p.root, &peer.host) {
                    continue;
                }
                if peer.status == PeerStatus::Deferred {
                    // Deliberately without an instance: listed so "the fleet
                    // was searched" is never silently narrower than the
                    // fleet, but no probe — there is nothing to probe.
                    unavailable.push(json!({
                        "host": peer.host,
                        "status": "deferred",
                        "ref": peer.reference,
                    }));
                    continue;
                }
                if peer.url.is_none() {
                    unavailable.push(json!({"host": peer.host, "status": "no_url"}));
                    continue;
                }
                if state.fleet.token_for(&peer.host, &author.0).is_none() {
                    unavailable.push(json!({
                        "host": peer.host,
                        "status": "no_credential",
                        "author": author.0,
                    }));
                    continue;
                }
                let (fleet, peer, author) = (state.fleet.clone(), peer.clone(), author.0.clone());
                probes.spawn(async move {
                    let outcome = fleet.probe_roots(&peer, &author).await;
                    (peer, outcome)
                });
            }
            while let Some(joined) = probes.join_next().await {
                let Ok((peer, outcome)) = joined else {
                    continue;
                };
                match outcome {
                    Ok(payload) => {
                        for name in payload
                            .get("roots")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|r| r.get("name")?.as_str().map(String::from))
                        {
                            if matcher.is_match(&name) {
                                remote.push((peer.clone(), name.clone()));
                            }
                            known.push(name);
                        }
                    }
                    Err(e) => {
                        let since = state
                            .fleet
                            .sight_of(&peer.host)
                            .down_since
                            .map(fleet::rfc3339);
                        unavailable.push(json!({
                            "host": peer.host,
                            "status": "unreachable",
                            "since": since,
                            "detail": e.message,
                        }));
                    }
                }
            }
        }
        // Stable order: this host's roots first, then peers as declared.
        remote.sort_by(|a, b| (&a.0.host, &a.1).cmp(&(&b.0.host, &b.1)));

        let mut searches = tokio::task::JoinSet::new();
        let base_remote_idx = local.len();
        for (idx, root) in local.into_iter().enumerate() {
            let (pattern, glob, path) = (p.pattern.clone(), p.glob.clone(), p.path.clone());
            let (regex, context, limits) = (p.regex, p.context, state.limits);
            searches.spawn_blocking(move || {
                let outcome = search::search(
                    &root,
                    &search::SearchParams {
                        pattern: &pattern,
                        regex,
                        glob: glob.as_deref(),
                        path: &path,
                        context,
                        max_results: max,
                    },
                    &limits,
                )
                .map_err(KaedError::with_feedback_invite);
                let json_of = |v: Result<search::SearchResult, KaedError>| match v {
                    Ok(out) => Ok(serde_json::to_value(out).unwrap_or_default()),
                    Err(e) => Err(serde_json::to_value(e).unwrap_or_default()),
                };
                (idx, root.host.clone(), root.name.clone(), json_of(outcome))
            });
        }
        for (offset, (peer, name)) in remote.into_iter().enumerate() {
            let fleet = state.fleet.clone();
            let author = author.0.clone();
            let mut args = serde_json::Map::new();
            args.insert("root".into(), json!(name));
            args.insert("pattern".into(), json!(p.pattern));
            args.insert("regex".into(), json!(p.regex));
            if let Some(g) = &p.glob {
                args.insert("glob".into(), json!(g));
            }
            if !p.path.is_empty() {
                args.insert("path".into(), json!(p.path));
            }
            args.insert("context".into(), json!(p.context));
            args.insert("max_results".into(), json!(max));
            searches.spawn(async move {
                let outcome = match fleet
                    .call(&peer, &author, "search", Some(args), Some(&name))
                    .await
                {
                    Ok(result) => {
                        let payload = result.structured_content.unwrap_or_default();
                        if result.is_error == Some(true) {
                            Err(payload)
                        } else {
                            Ok(payload)
                        }
                    }
                    Err(e) => {
                        Err(serde_json::to_value(e.with_feedback_invite()).unwrap_or_default())
                    }
                };
                (base_remote_idx + offset, peer.host, name, outcome)
            });
        }

        let mut pieces: Vec<(usize, fleet::SearchFanout)> = Vec::new();
        while let Some(joined) = searches.join_next().await {
            let Ok((idx, host, root, outcome)) = joined else {
                continue;
            };
            pieces.push((
                idx,
                fleet::SearchFanout {
                    root,
                    host,
                    outcome,
                },
            ));
        }
        pieces.sort_by_key(|(idx, _)| *idx);
        let fanout = pieces.into_iter().map(|(_, f)| f).collect();

        ok(fleet::merge_search(
            &p.root,
            fanout,
            unavailable,
            max,
            &known,
        ))
    }
}

/// Rebuild a `KaedError` from a peer's R4 error payload, so a failure on
/// the far side of a fetch keeps its code, message and data through this
/// hop instead of collapsing into `internal`.
fn kaed_error_from_value(v: Value) -> KaedError {
    let code = v
        .get("code")
        .cloned()
        .and_then(|c| serde_json::from_value(c).ok())
        .unwrap_or(crate::errors::ErrorCode::Internal);
    let message = v
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("peer returned an error without a message")
        .to_owned();
    let e = KaedError::new(code, message);
    match v.get("data").cloned() {
        Some(data) => e.with_data(data),
        None => e,
    }
}

/// Serialize a `KaedError` into the R4 `isError` tool result, friction
/// invitation attached — the one shape every failure leaves through.
fn kaed_error_result(e: KaedError) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::structured_error(
        serde_json::to_value(e.with_feedback_invite())
            .map_err(|se| ErrorData::internal_error(format!("serializing error: {se}"), None))?,
    ))
}

/// The identity the auth middleware bound to this request. Every write —
/// and every friction report — is attributed (R6); no anonymous mutation.
fn author_of(parts: &http::request::Parts) -> Result<Author, ErrorData> {
    parts
        .extensions
        .get::<Author>()
        .cloned()
        .ok_or_else(|| ErrorData::internal_error("no author on request", None))
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KaedServer {
    /// Hand-written so peer routing happens on the RAW argument object,
    /// before any typed dispatch (D-1) — `#[tool_handler]` generates this
    /// only when absent, so the router plumbing is otherwise unchanged. A
    /// call addressing a routable peer's root is forwarded verbatim and its
    /// result returned verbatim; everything else dispatches locally exactly
    /// as before.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if let Some((peer, root)) = self.remote_target(&request) {
            let mut request = request;
            // The one exception to verbatim passthrough, and a narrow one
            // (011 D-5): an `edit` op whose `value_from` names a root the
            // TARGET host does not serve is resolved here — the secret
            // moves through this process's memory, never the agent's
            // context — and everything else forwards byte-for-byte.
            if request.name == "edit" {
                let Some(parts) = context.extensions.get::<http::request::Parts>() else {
                    return Err(ErrorData::internal_error("no http parts on request", None));
                };
                let author = author_of(parts)?;
                if let Some(args) = request.arguments.as_mut()
                    && let Err(e) = self
                        .resolve_proxied_value_froms(&author, &peer.host, &root, args)
                        .await
                {
                    return Ok(kaed_error_result(e)?.into());
                }
            }
            let result = self.proxy_to_peer(peer, root, request, &context).await?;
            return Ok(result.into());
        }
        let tcc = ToolCallContext::new(self, request, context);
        self.tool_router.call(tcc).await
    }

    /// Hand-written for the SEP-2549 cache metadata (#1214) — the same trick
    /// `call_tool` uses above: `#[tool_handler]` generates this only when it
    /// is absent, so the router plumbing is otherwise unchanged. rmcp's
    /// generated body sets `ttl_ms`/`cache_scope` to `None`, which is
    /// precisely what made a 2026-07-28 client reject the result and register
    /// zero tools.
    ///
    /// The fields are emitted **only** for peers that negotiated a revision
    /// that defines them (D-1), mirroring rmcp's own
    /// `strip_result_type_for_legacy_peer`: a 2025-11-25 client is answered in
    /// the shape 2025-11-25 describes.
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut result = ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.tool_router.list_all(),
            meta: None,
            next_cursor: None,
            ttl_ms: None,
            cache_scope: None,
        };
        if context
            .protocol_version()
            .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str())
        {
            result.ttl_ms = Some(TOOLS_TTL_MS);
            // Public, not private: the catalog is compiled in and identical
            // for every author this instance serves, so nothing about it is
            // scoped to a credential. Peer roots change what a *call* can
            // reach, never which tools exist.
            result.cache_scope = Some(CacheScope::Public);
        }
        Ok(result)
    }

    /// Narrow rmcp's default — every revision the SDK knows — to the ones
    /// kaed's responses actually satisfy (#1212). This bounds what
    /// `initialize` may agree to and what `discover` advertises.
    fn supported_protocol_versions(&self) -> std::borrow::Cow<'static, [ProtocolVersion]> {
        std::borrow::Cow::Borrowed(SUPPORTED_PROTOCOL_VERSIONS)
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        // Not rmcp's default either: that is the SDK's newest, which is the
        // fallback handed to a client whose requested version we don't know.
        info.protocol_version = PROTOCOL_VERSION;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        // The build stamp, not the crate version: an agent asking "which
        // kaed am I talking to" needs the commit, and `0.1.0` has been the
        // answer since sprint 001 (korg #924).
        info.server_info =
            Implementation::new("kaed", crate::version::FULL).with_title("kaed — agent editor");
        info.instructions = Some(
            "kaed edits files on this host with verified writes. Roots are \
             host-qualified — `kai:src`, never `src` — so call `roots` first: \
             it returns the names to pass and a `fleet` block saying which \
             hosts should run kaed. A fleet host marked `deferred` is \
             deliberately without an instance and its `ref` says why; that is \
             a decision, not a broken deploy, so never install one to \"fix\" \
             it. Loop: \
             search or read (both return a `version`) → edit declaring that \
             version in `base` → the response diff is your proof, no re-read \
             needed. On version_conflict the error data carries \
             `actual_version` (the file's current version — use it as your \
             next base) and `delta` (what changed since you looked): \
             re-anchor from the delta and retry, no re-read needed. A \
             `version` is a content address, not a session handle: it never \
             expires and stays valid across your restarts, reconnects and \
             token rotations, so a version you recorded long ago is still a \
             usable base — no defensive re-read. Prefer `window` reads and \
             anchors over whole-file reads. Some paths are refused: `denied` \
             is permanent, don't retry it — its data carries a `reason` \
             (server_denylist, kaedignore, in_file_marker, classified_opaque) \
             and a `hint` naming what to do instead. The OS refuses too, \
             under the same code: not_readable_by_service_identity / \
             not_writable_by_service_identity mean unix ownership, not kaed \
             policy, and the data names the owner, the uid kaed runs as, and \
             this root's own advisory about where such files are managed \
             from. kaed writes by staging a temp file and renaming, so \
             writability is a property of the DIRECTORY, not the file's \
             mode — and `dry_run` probes it, so a dry run that returns a \
             diff is a write that can actually land. `denied_hidden`, \
             `classified_hidden` and `unreadable_hidden` on list/search \
             count what policy, classification and the OS kept out; an \
             unreadable directory is skipped and counted, never fatal. Secret-bearing files \
             (.env and friends) are *classified*, not denied: `read` serves \
             them redacted — values become sealed `⟨kaed:KEY@digest⟩` \
             placeholders, line-for-line with the raw file — and `edit` \
             takes typed env ops (env_set/env_rename/env_delete/env_reorder) \
             where a placeholder passed as a value writes the real value \
             back. You rarely need plaintext: to *use* a value in a shell, \
             `set -a; . .env; set +a` and reference $KEY. Destroying a value \
             requires naming its key in `drop_keys`. The write path also \
             watches the other direction: putting a secret INTO a file \
             nothing classifies (a README, a fixture, a doc) is refused \
             with `reason: secret_leak` when the content matches a known \
             secret's digest, a provider prefix, or a private-key block — \
             reference the variable instead of the value, or pass the \
             named `allow_secrets` override if writing it is deliberate; \
             merely secret-shaped content applies with a warning. \
             The `secret` tool runs the whole lifecycle without disclosure: \
             `describe` returns a durable HANDLE (root + path + key + \
             digest) — carry THAT in a handoff, never a value; `generate` \
             mints a fresh token server-side (you get a placeholder); \
             `rotate` re-mints in place and `also` writes the same new \
             value to other locations, even on other hosts; `occurrences` \
             finds every copy on the host by digest. An `edit` env_set \
             takes `value_from: {root, path, key, digest?}` to write a \
             value by reference — across hosts, the bytes move between \
             kaed instances and never through your context, and the source \
             host journals the transfer. `env_sync_example` regenerates \
             `.env.example` (keys and comments, values stubbed) safely. \
             `secret_reveal` is the separately-permissioned escape hatch: \
             one key, `intent` required, always journaled — read the audit \
             trail back with journal kind \"secret\". \
             `list` and `search` also report `files_searched`: zero there \
             means your `glob`/`path` selected nothing, not that the pattern \
             is absent — `glob` matches root-relative paths, so it is not \
             re-anchored by `path`. \
             Every edit is journaled under your identity; pass `intent` so \
             successors understand it. Read that history back with \
             `journal` — it merges applied transactions, failed attempts \
             and friction reports, and its `coverage` block says what it \
             cannot see. `diff` reconstructs any version the journal still \
             retains; `revert` undoes a transaction as a new one. \
             And when kaed gets in your way — a refusal with no way \
             forward, an error whose data was not enough, a call that \
             succeeded and answered wrong — call `feedback` with a \
             one-sentence `summary`. Especially before you give up and use \
             ssh instead: that is the failure this exists to catch, and it \
             is the one the journal can never see on its own. \
             This instance may also be a gateway to its fleet: `roots` can \
             list roots served by OTHER hosts (e.g. `kubs0:src`), and you \
             address them exactly like local ones — the call is proxied \
             under your own identity and journaled on that host under your \
             name, errors passing through verbatim. A fleet host that is \
             not answering shows up as status `unreachable` with `since`: \
             that is data to report, not a wiring failure to debug. \
             `search` also takes a root pattern (`*:*`, `*:src`, `kai:*`) \
             for fleet-wide search in one call — one budget, per-root \
             results in `fanout`, unsearchable hosts in \
             `hosts_unavailable`, never silently skipped. `journal` with a \
             peer's root as filter reads THAT host's journal. If the \
             gateway itself is down, every host's own kaed URL keeps \
             working — direct connection is the documented fallback."
                .into(),
        );
        info
    }
}

/// Run a blocking kaed operation and shape the outcome per R4: success and
/// agent-visible failure are both tool results; `Err` is infrastructure only.
///
/// This is also where the friction prompt is attached (D-5): one place, so
/// every tool gets it and no tool has to remember to. The report worth
/// having comes from the session that hit a wall, and this is the only
/// moment kaed can ask for it.
async fn run<T: Serialize + Send + 'static>(
    f: impl FnOnce() -> Result<T, KaedError> + Send + 'static,
) -> Result<CallToolResult, ErrorData> {
    let outcome = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorData::internal_error(format!("task panicked: {e}"), None))?;
    match outcome {
        Ok(v) => ok(serde_json::to_value(v)
            .map_err(|e| ErrorData::internal_error(format!("serializing result: {e}"), None))?),
        Err(e) => kaed_error_result(e),
    }
}

fn ok(value: serde_json::Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::structured(value))
}

// ------------------------------------------------------------------- auth

/// The live credential tables — inbound identities and outbound peer
/// tokens — swappable behind locks so SIGHUP can install new tokens without
/// restarting: the restart is what drops every live session, and it does so
/// whether or not tokens are involved (#914; extended to peer credentials
/// in 010, D-2).
pub struct AuthState {
    identities: RwLock<Vec<Identity>>,
    /// Where the tokens live, so a reload can go back and re-read them.
    spec: BTreeMap<String, AuthEntry>,
    /// The outbound half: this instance's credentials *for its peers*,
    /// shared with `fleet::Peers` so proxy sessions see rotations.
    peer_tokens: Arc<fleet::PeerTokens>,
    peers_spec: Vec<Peer>,
}

impl AuthState {
    /// Re-read every token — inbound and peer — from its file. Env-var
    /// tokens come back unchanged — a process cannot re-read its own
    /// `EnvironmentFile`, so those still need a restart.
    pub fn reload(&self) {
        self.peer_tokens.reload(&self.peers_spec);
        let fresh = config::resolve_identities(&self.spec);
        if fresh.is_empty() {
            tracing::error!("reload resolved no identities; keeping the current set");
            return;
        }
        let authors: Vec<&str> = fresh.iter().map(|i| i.author.as_str()).collect();
        tracing::info!(identities = ?authors, "reloaded auth tokens");
        *self.identities.write().expect("auth lock never poisoned") = fresh;
    }
}

/// Constant-time token comparison; length is the only thing an attacker
/// can learn.
fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

async fn auth_middleware(
    State(auth): State<Arc<AuthState>>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    let bearer = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let author = bearer.and_then(|token| {
        let identities = auth.identities.read().expect("auth lock never poisoned");
        for id in identities.iter() {
            if token_eq(&id.token, token) {
                return Some(id.author.clone());
            }
            if id.prev_token.as_deref().is_some_and(|p| token_eq(p, token)) {
                // The one signal that answers "is this rotation finished?" —
                // 401s never reach the journal, so this is where it lives.
                tracing::warn!(
                    author = id.author,
                    "authenticated with the PREVIOUS token; this client has not \
                     picked up the new one yet"
                );
                return Some(id.author.clone());
            }
        }
        None
    });
    match author {
        Some(author) => {
            req.extensions_mut().insert(Author(author));
            next.run(req).await
        }
        None => {
            tracing::warn!(
                presented = bearer.is_some(),
                "401: no identity for the presented credential"
            );
            unauthorized(bearer.is_some())
        }
    }
}

/// A 401 that says what is actually wrong (RFC 6750 §3). A bare 401 gets
/// rendered with the client's own generic story — cleo's said "token
/// expired", which sent the live test hunting for a TTL kaed has never
/// had. The description goes in the header *and* the body, so a client
/// that surfaces either one stops guessing.
///
/// Per §3.1 a request carrying no credential at all gets the challenge
/// without an error code: there is no invalid token to describe, and the
/// client may simply not have known auth was needed.
fn unauthorized(had_token: bool) -> Response {
    const NO_EXPIRY: &str = "token matches no configured identity; kaed tokens do not expire";
    let (challenge, body) = if had_token {
        (
            format!(
                "Bearer realm=\"kaed\", error=\"invalid_token\", error_description=\"{NO_EXPIRY}\""
            ),
            format!("unauthorized: {NO_EXPIRY}\n"),
        )
    } else {
        (
            "Bearer realm=\"kaed\"".to_string(),
            "unauthorized: no bearer token presented\n".to_string(),
        )
    };
    (
        http::StatusCode::UNAUTHORIZED,
        [(http::header::WWW_AUTHENTICATE, challenge)],
        body,
    )
        .into_response()
}

// --------------------------------------------------------------- assembly

/// Build the full HTTP app: auth middleware in front of the MCP service at
/// `/mcp`. Separated from `serve` so tests can drive it on an ephemeral
/// listener. The returned `AuthState` is the reload handle.
pub fn build_app(resolved: Resolved) -> anyhow::Result<(axum::Router, Arc<AuthState>)> {
    anyhow::ensure!(
        !resolved.roots.is_empty(),
        "no roots configured; nothing to serve"
    );
    anyhow::ensure!(
        !resolved.identities.is_empty(),
        "no auth identities resolved; set the token env vars from [auth]"
    );

    let journal = Journal::open(&resolved.journal_path, resolved.journal_retention_days)?;
    for torn in journal.scan_pending().map_err(|e| anyhow::anyhow!("{e}"))? {
        tracing::warn!(
            txn = torn.id,
            author = %torn.author,
            started = %torn.started_at,
            files = ?torn.files,
            "torn transaction: begun but never completed — files may need inspection"
        );
    }

    // Peer credentials resolve once here and re-resolve on SIGHUP; the
    // same Arc feeds both the reload handle and the routing layer, so a
    // rotation is visible to in-flight session checkout immediately.
    let peers_spec: Vec<Peer> = resolved.peers.clone().unwrap_or_default();
    let peer_tokens = Arc::new(fleet::PeerTokens::new(config::resolve_peer_tokens(
        &peers_spec,
    )));
    let state = Arc::new(AppState {
        host: resolved.host.clone(),
        roots: resolved.roots,
        fleet: Arc::new(fleet::Peers::new(
            resolved.host,
            resolved.peers,
            peer_tokens.clone(),
        )),
        limits: resolved.limits,
        journal,
        secrets: resolved.secrets,
    });
    let auth = Arc::new(AuthState {
        identities: RwLock::new(resolved.identities),
        spec: resolved.auth,
        peer_tokens,
        peers_spec,
    });

    let mut allowed_hosts: Vec<String> = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    allowed_hosts.extend(resolved.allowed_hosts);

    let mcp = StreamableHttpService::new(
        move || Ok(KaedServer::new(state.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default().with_allowed_hosts(allowed_hosts),
    );

    let router =
        axum::Router::new()
            .nest_service("/mcp", mcp)
            .layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                auth_middleware,
            ));
    Ok((router, auth))
}

pub async fn serve(resolved: Resolved) -> anyhow::Result<()> {
    let bind = resolved.bind;
    let (app, auth) = build_app(resolved)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;

    // SIGHUP re-reads the token files in place. `systemctl --user reload
    // kaed` maps onto this with one unit line, and live sessions survive —
    // which is the whole point, since it is the restart, not the token,
    // that breaks them.
    let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    tokio::spawn(async move {
        while hup.recv().await.is_some() {
            tracing::info!("SIGHUP: reloading auth tokens");
            auth.reload();
        }
    });

    tracing::info!(%bind, "kaed serving MCP at /mcp");
    axum::serve(listener, app).await?;
    Ok(())
}
