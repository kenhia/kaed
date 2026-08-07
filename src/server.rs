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

use crate::config::{self, AuthEntry, Identity, Limits, Peer, PeerStatus, Resolved, ResolvedRoot};
use crate::errors::KaedError;
use crate::fsops::{self, ReadMode};
use crate::journal::Journal;
use crate::search;
use crate::txn::{self, BaseVersion, EditOp, EditRequest};
use axum::extract::State;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

const DEFAULT_LIST_MAX: usize = 500;
const SEARCH_MAX_RESULTS_CEILING: usize = 1000;

/// What every root on this instance supports. Advertised per-root because
/// under peer mode (korg:1050) a fleet can be mid-upgrade, and the rule from
/// the gateway brainstorm is to advertise the **union** with per-root
/// capabilities — never the intersection, which silently hides working
/// features on the most-updated host.
const ROOT_CAPABILITIES: &[&str] = &["stat", "list", "read", "search", "edit"];

/// The authenticated author identity, set by the auth middleware and read
/// by mutating tools for journal attribution.
#[derive(Debug, Clone)]
pub struct Author(pub String);

pub struct AppState {
    /// This instance's fleet name; the prefix on every root it serves.
    pub host: String,
    pub roots: Vec<ResolvedRoot>,
    /// The declared fleet minus this host. `None` = no `[peers]` table at
    /// all, which is the `never-declared` state and is reported as such.
    pub peers: Option<Vec<Peer>>,
    pub limits: Limits,
    pub journal: Journal,
}

impl AppState {
    fn root(&self, name: &str) -> Result<ResolvedRoot, KaedError> {
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
        let Some(peers) = &self.peers else {
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
                PeerStatus::Active => KaedError::not_found(format!(
                    "host {host:?} is in this host's declared fleet, but {} does not \
                     proxy to peers yet (korg:1050) — connect to {host}'s own kaed",
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

    /// The declared fleet as `roots` reports it: this instance first, then
    /// every peer. Only the first entry is `verified` — this sprint probes
    /// nothing, and reporting a config-declared peer as observed-active
    /// would be a fresh instance of the bug it is fixing (D-4).
    fn fleet(&self) -> FleetInfo {
        let mut hosts = vec![FleetHostInfo {
            host: self.host.clone(),
            status: "active",
            is_self: true,
            verified: true,
            version: Some(crate::version::FULL.to_string()),
            roots: Some(self.roots.iter().map(|r| r.name.clone()).collect()),
            reference: None,
            note: None,
            since: None,
            url: None,
        }];
        for p in self.peers.iter().flatten() {
            hosts.push(FleetHostInfo {
                host: p.host.clone(),
                status: p.status.as_str(),
                is_self: false,
                verified: false,
                version: None,
                roots: None,
                reference: p.reference.clone(),
                note: p.note.clone(),
                since: p.since.clone(),
                url: p.url.clone(),
            });
        }
        FleetInfo {
            declared: self.peers.is_some(),
            hosts,
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
}

#[derive(Serialize, JsonSchema)]
struct RootsResult {
    /// The instance answering this call. Every root name below is prefixed
    /// with it.
    host: String,
    roots: Vec<RootInfo>,
    fleet: FleetInfo,
}

#[derive(Serialize, JsonSchema)]
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
#[derive(Serialize, JsonSchema)]
struct FleetInfo {
    /// `false` when this host's config has no `[peers]` table: the fleet is
    /// **undeclared**, `hosts` is this instance alone, and a host's absence
    /// from it means nothing. When `true`, an absent host is one kaed here
    /// has no opinion about — distinct from one declared `deferred`.
    declared: bool,
    hosts: Vec<FleetHostInfo>,
}

#[derive(Serialize, JsonSchema)]
struct FleetHostInfo {
    host: String,
    /// `active` | `deferred` | `unreachable`. For every entry but `self`
    /// this is the *declaration*, not an observation — see `verified`.
    status: &'static str,
    /// True for the instance answering this call.
    #[serde(rename = "self")]
    is_self: bool,
    /// Whether this instance checked, as opposed to reading config. Only
    /// `self` is verified until peer mode lands (korg:1050).
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
    /// When an `unreachable` host was last known good.
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

// ------------------------------------------------------------------ tools

#[tool_router]
impl KaedServer {
    #[tool(
        description = "List the workspace roots this instance serves, and the declared fleet. Root names are host-qualified (`kai:src`) — pass one verbatim as `root`, with a root-relative `path`. `fleet` answers which hosts should run kaed and what is known about each: a host declared `deferred` is deliberately not running one (`ref` says why) and must not be \"fixed\"; `fleet.declared: false` means this host declares no fleet at all, so absence from the list means nothing."
    )]
    async fn roots(&self) -> Result<CallToolResult, ErrorData> {
        let roots: Vec<RootInfo> = self
            .state
            .roots
            .iter()
            .map(|r| RootInfo {
                name: r.name.clone(),
                host: r.host.clone(),
                path: r.path.display().to_string(),
                description: r.description.clone(),
                status: "active",
                capabilities: ROOT_CAPABILITIES,
            })
            .collect();
        ok(serde_json::to_value(RootsResult {
            host: self.state.host.clone(),
            roots,
            fleet: self.state.fleet(),
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
        description = "List directory entries (gitignore-aware; `.git` always hidden). Budgeted: `truncated: true` + `next_offset` mean more entries exist — continue by passing `offset`."
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
            fsops::read(
                &root,
                &p.path,
                &mode,
                p.numbered,
                p.max_bytes,
                &state.limits,
            )
        })
        .await
    }

    #[tool(
        description = "Ripgrep-grade search. Each match carries its file's `version`, so search → edit is safe with no read in between (a stale hit becomes version_conflict, never a wrong edit)."
    )]
    async fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, ErrorData> {
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
        description = "Transactional edit: anchor_replace / range_replace / create ops, multi-file, atomic — all land or none do. Every non-create path must appear in `base` with its version; a mismatch fails with version_conflict carrying a delta of what changed. The returned diff is proof of what was applied: no verification read needed. Supports dry_run."
    )]
    async fn edit(
        &self,
        Extension(parts): Extension<http::request::Parts>,
        Parameters(p): Parameters<EditParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let author = parts
            .extensions
            .get::<Author>()
            .cloned()
            .ok_or_else(|| ErrorData::internal_error("no author on request", None))?;
        let state = self.state.clone();
        run(move || {
            let root = state.root(&p.root)?;
            let req = EditRequest {
                base: p.base,
                ops: p.ops,
                dry_run: p.dry_run,
                return_diff: p.return_diff,
                intent: p.intent,
            };
            txn::apply(&root, &req, &state.limits, &author.0, &state.journal)
        })
        .await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KaedServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
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
             anchors over whole-file reads. Some paths are refused by the \
             server's deny list: `denied` is permanent, don't retry it, and \
             `denied_hidden` on list/search counts what was filtered out. \
             `list` and `search` also report `files_searched`: zero there \
             means your `glob`/`path` selected nothing, not that the pattern \
             is absent — `glob` matches root-relative paths, so it is not \
             re-anchored by `path`. \
             Every edit is journaled under your identity; pass `intent` so \
             successors understand it."
                .into(),
        );
        info
    }
}

/// Run a blocking kaed operation and shape the outcome per R4: success and
/// agent-visible failure are both tool results; `Err` is infrastructure only.
async fn run<T: Serialize + Send + 'static>(
    f: impl FnOnce() -> Result<T, KaedError> + Send + 'static,
) -> Result<CallToolResult, ErrorData> {
    let outcome = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ErrorData::internal_error(format!("task panicked: {e}"), None))?;
    match outcome {
        Ok(v) => ok(serde_json::to_value(v)
            .map_err(|e| ErrorData::internal_error(format!("serializing result: {e}"), None))?),
        Err(e) => Ok(CallToolResult::structured_error(
            serde_json::to_value(&e).map_err(|se| {
                ErrorData::internal_error(format!("serializing error: {se}"), None)
            })?,
        )),
    }
}

fn ok(value: serde_json::Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::structured(value))
}

// ------------------------------------------------------------------- auth

/// The live identity table. Swappable behind a lock so SIGHUP can install
/// new tokens without restarting: the restart is what drops every live
/// session, and it does so whether or not tokens are involved (#914).
pub struct AuthState {
    identities: RwLock<Vec<Identity>>,
    /// Where the tokens live, so a reload can go back and re-read them.
    spec: BTreeMap<String, AuthEntry>,
}

impl AuthState {
    /// Re-read every token from its file. Env-var tokens come back
    /// unchanged — a process cannot re-read its own `EnvironmentFile`, so
    /// those still need a restart.
    pub fn reload(&self) {
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

    let state = Arc::new(AppState {
        host: resolved.host,
        roots: resolved.roots,
        peers: resolved.peers,
        limits: resolved.limits,
        journal,
    });
    let auth = Arc::new(AuthState {
        identities: RwLock::new(resolved.identities),
        spec: resolved.auth,
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
