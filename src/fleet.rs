//! Peer routing — the gateway half of kaed (sprint 010, korg #1050).
//!
//! Any instance with `[peers.<host>]` URLs and per-author tokens can proxy
//! calls addressing a peer's roots; whichever instance a client points at
//! becomes the gateway. Three rules govern everything here:
//!
//! - **Identity survives the hop (PD-4).** A call is proxied with the
//!   *caller's* token for that backend, never a shared gateway credential.
//!   No token for that (peer, author) pair → refuse, don't substitute.
//! - **Passthrough is verbatim (D-3).** Arguments go out byte-for-byte and
//!   results come back untouched; the one addition is a top-level `root`
//!   tag on proxied *error* objects, so fan-out consumers can attribute
//!   them. The gateway never applies its own schema to a proxied payload.
//! - **Reachability is data (D-4).** A peer that does not answer becomes
//!   `{status: "unreachable", since}` in `roots` and a structured
//!   `host_unreachable` on the call path — never a transport failure the
//!   agent cannot reason about.

use crate::config::{Peer, PeerStatus};
use crate::errors::KaedError;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientInfo, Implementation, JsonObject};
use rmcp::service::{RoleClient, RunningService, ServiceError};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime};

/// Establishing a session to a peer. A connect failure means the peer never
/// saw anything — safe to report unreachable and safe to retry later.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// An in-flight proxied call. Expiry here is NOT a connect failure: the
/// peer may have applied the call, and the error says so (D-8).
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

type Session = RunningService<RoleClient, ClientInfo>;

/// Root-name glob? (`*:*`, `kai:*`, `*:src`). Patterns are only meaningful
/// to `search`; every other tool refuses them by name.
pub fn is_pattern(root: &str) -> bool {
    root.contains(['*', '?', '['])
}

/// The resolved (peer host, author) → bearer token table, swappable behind
/// a lock so SIGHUP can rotate peer credentials without a restart — the
/// same shape, and the same reason, as the inbound identity table (#914).
pub struct PeerTokens {
    map: RwLock<BTreeMap<(String, String), String>>,
}

impl PeerTokens {
    pub fn new(map: BTreeMap<(String, String), String>) -> Self {
        Self {
            map: RwLock::new(map),
        }
    }

    /// Re-read every peer token from its configured source.
    pub fn reload(&self, peers: &[Peer]) {
        let fresh = crate::config::resolve_peer_tokens(peers);
        *self.map.write().expect("peer token lock never poisoned") = fresh;
    }

    fn get(&self, peer: &str, author: &str) -> Option<String> {
        self.map
            .read()
            .expect("peer token lock never poisoned")
            .get(&(peer.to_string(), author.to_string()))
            .cloned()
    }
}

struct CachedSession {
    svc: Arc<Session>,
    /// The token this session was built with. A SIGHUP that rotates the
    /// configured token makes this stale, and checkout rebuilds (D-2).
    token: String,
}

/// What this process has *observed* about one peer — never persisted, and
/// never written back into the declaration (D-4).
#[derive(Default, Clone)]
pub struct PeerSight {
    /// First failure of the current outage; `None` while the peer answers.
    pub down_since: Option<SystemTime>,
    /// Server version reported when a session was last established.
    pub version: Option<String>,
    /// The peer's `roots` entries from the last successful probe, verbatim.
    /// Served with `status: "unreachable"` while the peer is down, so the
    /// namespace stays visible — win #1's exact artifact.
    pub roots: Option<Vec<Value>>,
}

/// The declared fleet plus everything needed to route to it: session pool,
/// per-author peer credentials, observed health.
pub struct Peers {
    this_host: String,
    /// `None` = no `[peers]` table — never-declared, not an empty fleet.
    declared: Option<Vec<Peer>>,
    tokens: Arc<PeerTokens>,
    sessions: tokio::sync::Mutex<HashMap<(String, String), CachedSession>>,
    sight: RwLock<HashMap<String, PeerSight>>,
}

impl Peers {
    pub fn new(this_host: String, declared: Option<Vec<Peer>>, tokens: Arc<PeerTokens>) -> Self {
        Self {
            this_host,
            declared,
            tokens,
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            sight: RwLock::new(HashMap::new()),
        }
    }

    pub fn declared(&self) -> Option<&[Peer]> {
        self.declared.as_deref()
    }

    pub fn find(&self, host: &str) -> Option<&Peer> {
        self.declared()
            .and_then(|ps| ps.iter().find(|p| p.host == host))
    }

    /// A peer calls can be routed to: declared, not deferred, has a URL.
    /// Declared-`unreachable` counts — observation beats declaration, so a
    /// host that came back is reachable the moment it answers.
    pub fn routable(&self, host: &str) -> Option<&Peer> {
        self.find(host)
            .filter(|p| p.status != PeerStatus::Deferred && p.url.is_some())
    }

    pub fn token_for(&self, peer_host: &str, author: &str) -> Option<String> {
        self.tokens.get(peer_host, author)
    }

    pub fn sight_of(&self, host: &str) -> PeerSight {
        self.sight
            .read()
            .expect("sight lock never poisoned")
            .get(host)
            .cloned()
            .unwrap_or_default()
    }

    fn mark_up(&self, host: &str) {
        let mut sight = self.sight.write().expect("sight lock never poisoned");
        let entry = sight.entry(host.to_string()).or_default();
        if let Some(since) = entry.down_since.take() {
            tracing::info!(
                peer = host,
                down_since = %rfc3339(since),
                "peer is answering again"
            );
        }
    }

    fn mark_down(&self, host: &str) {
        let mut sight = self.sight.write().expect("sight lock never poisoned");
        let entry = sight.entry(host.to_string()).or_default();
        entry.down_since.get_or_insert_with(SystemTime::now);
    }

    fn record_version(&self, host: &str, version: Option<String>) {
        if let Some(v) = version {
            let mut sight = self.sight.write().expect("sight lock never poisoned");
            sight.entry(host.to_string()).or_default().version = Some(v);
        }
    }

    fn record_roots(&self, host: &str, roots: Vec<Value>) {
        let mut sight = self.sight.write().expect("sight lock never poisoned");
        sight.entry(host.to_string()).or_default().roots = Some(roots);
    }

    /// Proxy one tool call to a peer under the caller's identity. `root`
    /// is the addressed root, used only to tag proxied error objects.
    pub async fn call(
        &self,
        peer: &Peer,
        author: &str,
        tool: &str,
        arguments: Option<JsonObject>,
        root: Option<&str>,
    ) -> Result<CallToolResult, KaedError> {
        let Some(token) = self.token_for(&peer.host, author) else {
            return Err(self.no_credential(peer, author));
        };
        // One retry, and only for failures provably before the peer
        // processed anything: session establishment, or the transport
        // closing on send. An in-flight failure is never replayed (D-8).
        for attempt in 0..2 {
            let svc = self.checkout(peer, author, &token).await?;
            let params = CallToolRequestParams::new(tool.to_string())
                .with_arguments(arguments.clone().unwrap_or_default());
            match tokio::time::timeout(CALL_TIMEOUT, svc.call_tool(params)).await {
                Err(_elapsed) => return Err(self.in_flight_timeout(peer, tool)),
                Ok(Ok(mut result)) => {
                    self.mark_up(&peer.host);
                    if result.is_error == Some(true)
                        && let Some(root) = root
                        && let Some(Value::Object(obj)) = result.structured_content.as_mut()
                    {
                        // The one addition passthrough allows (D-3).
                        obj.insert("root".into(), json!(root));
                    }
                    return Ok(result);
                }
                Ok(Err(e)) => match e {
                    ServiceError::TransportClosed | ServiceError::TransportSend(_)
                        if attempt == 0 =>
                    {
                        self.drop_session(&peer.host, author).await;
                        continue;
                    }
                    other => return Err(self.map_call_error(peer, author, tool, other).await),
                },
            }
        }
        unreachable!("the retry loop always returns");
    }

    /// Fetch a peer's `roots` and cache what it reports. The payload is the
    /// peer's response verbatim.
    pub async fn probe_roots(&self, peer: &Peer, author: &str) -> Result<Value, KaedError> {
        let result = self.call(peer, author, "roots", None, None).await?;
        if result.is_error == Some(true) {
            return Err(KaedError::internal(format!(
                "peer {:?} answered its own `roots` with an error: {:?}",
                peer.host, result.structured_content
            )));
        }
        let payload = result.structured_content.ok_or_else(|| {
            KaedError::internal(format!(
                "peer {:?} returned `roots` without structured content",
                peer.host
            ))
        })?;
        if let Some(roots) = payload.get("roots").and_then(Value::as_array) {
            self.record_roots(&peer.host, roots.clone());
        }
        Ok(payload)
    }

    async fn checkout(
        &self,
        peer: &Peer,
        author: &str,
        token: &str,
    ) -> Result<Arc<Session>, KaedError> {
        let key = (peer.host.clone(), author.to_string());
        {
            let sessions = self.sessions.lock().await;
            if let Some(cached) = sessions.get(&key)
                && cached.token == token
            {
                return Ok(cached.svc.clone());
            }
        }
        let url = peer.url.as_deref().expect("routable peers carry a url");
        let transport = StreamableHttpClientTransport::from_config(
            StreamableHttpClientTransportConfig::with_uri(url.to_string()).auth_header(token),
        );
        let mut info = ClientInfo::default();
        info.client_info = Implementation::new("kaed", crate::version::FULL);
        let svc = match tokio::time::timeout(CONNECT_TIMEOUT, info.serve(transport)).await {
            Err(_elapsed) => {
                self.mark_down(&peer.host);
                return Err(self.unreachable(peer, "connect timed out"));
            }
            Ok(Err(e)) => {
                let detail = e.to_string();
                if looks_like_auth_rejection(&detail) {
                    return Err(self.credential_rejected(peer, author, &detail));
                }
                self.mark_down(&peer.host);
                return Err(self.unreachable(peer, &detail));
            }
            Ok(Ok(svc)) => svc,
        };
        self.mark_up(&peer.host);
        self.record_version(
            &peer.host,
            svc.peer_info()
                .and_then(|pi| pi.server_info.as_ref().map(|si| si.version.to_string())),
        );
        let svc = Arc::new(svc);
        self.sessions.lock().await.insert(
            key,
            CachedSession {
                svc: svc.clone(),
                token: token.to_string(),
            },
        );
        Ok(svc)
    }

    async fn drop_session(&self, host: &str, author: &str) {
        self.sessions
            .lock()
            .await
            .remove(&(host.to_string(), author.to_string()));
    }

    async fn map_call_error(
        &self,
        peer: &Peer,
        author: &str,
        tool: &str,
        e: ServiceError,
    ) -> KaedError {
        match e {
            ServiceError::McpError(ed) => {
                // The peer answered — it is up; it just could not serve
                // this. "tool not found" is the mid-upgrade fleet case the
                // union-of-capabilities rule promises to name honestly.
                self.mark_up(&peer.host);
                if ed.message.contains("tool not found") {
                    KaedError::unsupported_capability(format!(
                        "host {:?} does not support `{tool}` (its kaed predates it); \
                         the fleet advertises the union of capabilities, and this \
                         host lacks this one",
                        peer.host
                    ))
                    .with_data(json!({
                        "tool": tool,
                        "this_host": self.this_host,
                        "target_host": peer.host,
                        "target_version": self.sight_of(&peer.host).version,
                    }))
                } else {
                    KaedError::internal(format!(
                        "peer {:?} rejected the call at the protocol level: {}",
                        peer.host, ed.message
                    ))
                }
            }
            ServiceError::TransportClosed | ServiceError::TransportSend(_) => {
                let detail = e.to_string();
                self.drop_session(&peer.host, author).await;
                if looks_like_auth_rejection(&detail) {
                    self.credential_rejected(peer, author, &detail)
                } else {
                    self.mark_down(&peer.host);
                    self.unreachable(peer, &detail)
                }
            }
            other => KaedError::internal(format!("proxying to {:?} failed: {other}", peer.host)),
        }
    }

    fn no_credential(&self, peer: &Peer, author: &str) -> KaedError {
        KaedError::new(
            crate::errors::ErrorCode::Denied,
            format!(
                "no {author:?} credential for host {:?}: kaed on {:?} proxies with \
                 the caller's own identity (never a shared one), and it holds no \
                 token for you there",
                peer.host, self.this_host
            ),
        )
        .with_data(json!({
            "reason": "no_peer_credential",
            "this_host": self.this_host,
            "target_host": peer.host,
            "author": author,
            "hint": format!(
                "add [peers.{}.tokens] {author} = {{ token_file = \"…\" }} to kaed's \
                 config on {}, or connect to {}'s own kaed directly",
                peer.host, self.this_host, peer.host
            ),
        }))
    }

    fn credential_rejected(&self, peer: &Peer, author: &str, detail: &str) -> KaedError {
        KaedError::new(
            crate::errors::ErrorCode::Denied,
            format!(
                "host {:?} rejected the {author:?} credential this gateway holds \
                 for it — likely rotated on the backend but not here",
                peer.host
            ),
        )
        .with_data(json!({
            "reason": "peer_credential_rejected",
            "this_host": self.this_host,
            "target_host": peer.host,
            "author": author,
            "detail": detail,
            "hint": format!(
                "rotate the token in [peers.{}.tokens] on {} (SIGHUP reloads it), \
                 or connect to {}'s own kaed directly",
                peer.host, self.this_host, peer.host
            ),
        }))
    }

    fn unreachable(&self, peer: &Peer, detail: &str) -> KaedError {
        let since = self
            .sight_of(&peer.host)
            .down_since
            .map(rfc3339)
            .or_else(|| peer.since.clone());
        KaedError::not_found(format!(
            "host {:?} is declared part of the fleet but did not answer ({detail})",
            peer.host
        ))
        .with_data(json!({
            "reason": "host_unreachable",
            "this_host": self.this_host,
            "target_host": peer.host,
            "since": since,
            "url": peer.url,
            "detail": detail,
        }))
    }

    fn in_flight_timeout(&self, peer: &Peer, tool: &str) -> KaedError {
        // NOT host_unreachable: the peer accepted the call and may have
        // applied it. Blurring this into a connect failure is how a slow
        // edit becomes a double edit (D-8).
        KaedError::internal(format!(
            "`{tool}` on host {:?} did not answer within {}s — the call MAY HAVE \
             APPLIED there; check that host's journal before retrying",
            peer.host,
            CALL_TIMEOUT.as_secs()
        ))
        .with_data(json!({
            "reason": "peer_timeout",
            "this_host": self.this_host,
            "target_host": peer.host,
            "timeout_secs": CALL_TIMEOUT.as_secs(),
        }))
    }
}

/// Best-effort recognition of a 401/403 from the peer inside the client
/// transport's error chain. rmcp's streamable HTTP client surfaces both as
/// `AuthRequired` ("Auth required"); the string forms are a fallback for
/// other layers. Misclassifying auth-rejection as unreachable would send
/// whoever reads the error to debug the network instead of rotating the
/// token, so this is matched before any `host_unreachable` verdict.
fn looks_like_auth_rejection(detail: &str) -> bool {
    detail.contains("Auth required")
        || detail.contains("401")
        || detail.contains("Unauthorized")
        || detail.contains("unauthorized")
}

pub fn rfc3339(t: SystemTime) -> String {
    humantime::format_rfc3339_seconds(t).to_string()
}

// -------------------------------------------------------- fleet search merge

/// One root's contribution to a fleet search: the root, its host, and
/// either that host's `search` response or the structured error it failed
/// with — both as raw JSON, because peer responses pass through verbatim.
pub struct SearchFanout {
    pub root: String,
    pub host: String,
    pub outcome: Result<Value, Value>,
}

/// Merge per-root search responses under ONE budget with per-root
/// truncation reporting. Matches keep stable root order and each gains a
/// `root` tag; every drop is attributed to the layer that made it —
/// per-host `truncated` stays the host's own report, and what the *merge*
/// discarded lands in that root's `merge_dropped` (D-5).
pub fn merge_search(
    pattern: &str,
    fanout: Vec<SearchFanout>,
    hosts_unavailable: Vec<Value>,
    max_results: usize,
    known_roots: &[String],
) -> Value {
    let mut matches: Vec<Value> = Vec::new();
    let mut fanout_report: Vec<Value> = Vec::new();
    let mut files_searched: u64 = 0;
    let mut truncated = false;

    for part in fanout {
        match part.outcome {
            Ok(result) => {
                let mut entry = json!({ "root": part.root, "host": part.host });
                let obj = entry.as_object_mut().expect("entry is an object");
                for field in [
                    "files_searched",
                    "truncated",
                    "denied_hidden",
                    "classified_hidden",
                    "reason",
                ] {
                    if let Some(v) = result.get(field) {
                        obj.insert(field.into(), v.clone());
                    }
                }
                files_searched += result
                    .get("files_searched")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if result.get("truncated") == Some(&Value::Bool(true)) {
                    truncated = true;
                }
                let root_matches = result
                    .get("matches")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let budget_left = max_results.saturating_sub(matches.len());
                let taken = root_matches.len().min(budget_left);
                if taken < root_matches.len() {
                    obj.insert("merge_dropped".into(), json!(root_matches.len() - taken));
                    truncated = true;
                }
                for mut m in root_matches.into_iter().take(taken) {
                    if let Some(mo) = m.as_object_mut() {
                        mo.insert("root".into(), json!(part.root));
                    }
                    matches.push(m);
                }
                fanout_report.push(entry);
            }
            Err(error) => {
                fanout_report.push(json!({
                    "root": part.root,
                    "host": part.host,
                    "error": error,
                }));
            }
        }
    }

    let mut out = json!({
        "fleet": true,
        "root_pattern": pattern,
        "matches": matches,
        "truncated": truncated,
        "files_searched": files_searched,
        "fanout": fanout_report,
    });
    let obj = out.as_object_mut().expect("out is an object");
    if !hosts_unavailable.is_empty() {
        obj.insert("hosts_unavailable".into(), json!(hosts_unavailable));
    }
    if obj["fanout"].as_array().is_some_and(Vec::is_empty) {
        // The #1066 rule applied to the fleet namespace: a pattern that
        // selected no roots explains itself, it does not just come back
        // empty.
        obj.insert(
            "reason".into(),
            json!({
                "code": "root_pattern_matched_no_roots",
                "hint": format!(
                    "pattern {pattern:?} matched none of the known roots \
                     {known_roots:?}; patterns glob over full root names \
                     (`*:*`, `kai:*`, `*:src`)"
                ),
            }),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(matches: usize, files: u64, truncated: bool) -> Value {
        json!({
            "matches": (0..matches)
                .map(|i| json!({"path": format!("f{i}.rs"), "line": i}))
                .collect::<Vec<_>>(),
            "truncated": truncated,
            "files_searched": files,
        })
    }

    #[test]
    fn patterns_are_recognized_and_concrete_roots_are_not() {
        assert!(is_pattern("*:*"));
        assert!(is_pattern("kai:*"));
        assert!(is_pattern("*:src"));
        assert!(!is_pattern("kai:src"));
    }

    #[test]
    fn merge_tags_matches_and_sums_files_searched() {
        let out = merge_search(
            "*:*",
            vec![
                SearchFanout {
                    root: "kai:src".into(),
                    host: "kai".into(),
                    outcome: Ok(result(2, 10, false)),
                },
                SearchFanout {
                    root: "kubs0:src".into(),
                    host: "kubs0".into(),
                    outcome: Ok(result(1, 5, false)),
                },
            ],
            vec![],
            50,
            &[],
        );
        assert_eq!(out["matches"].as_array().unwrap().len(), 3);
        assert_eq!(out["matches"][0]["root"], "kai:src");
        assert_eq!(out["matches"][2]["root"], "kubs0:src");
        assert_eq!(out["files_searched"], 15);
        assert_eq!(out["truncated"], false);
        assert_eq!(out["fanout"].as_array().unwrap().len(), 2);
        assert!(out.get("hosts_unavailable").is_none());
        assert!(out.get("reason").is_none());
    }

    /// One budget across the fan-out; what the merge drops is attributed
    /// to the merge, per root, never silently (D-5).
    #[test]
    fn merge_enforces_one_budget_and_attributes_its_own_drops() {
        let out = merge_search(
            "*:*",
            vec![
                SearchFanout {
                    root: "a:r".into(),
                    host: "a".into(),
                    outcome: Ok(result(3, 1, false)),
                },
                SearchFanout {
                    root: "b:r".into(),
                    host: "b".into(),
                    outcome: Ok(result(3, 1, false)),
                },
            ],
            vec![],
            4,
            &[],
        );
        assert_eq!(out["matches"].as_array().unwrap().len(), 4);
        assert_eq!(out["truncated"], true);
        let fanout = out["fanout"].as_array().unwrap();
        assert!(fanout[0].get("merge_dropped").is_none());
        assert_eq!(fanout[1]["merge_dropped"], 2);
        // the hosts' own reports are preserved, not overwritten
        assert_eq!(fanout[1]["truncated"], false);
    }

    #[test]
    fn a_roots_error_becomes_a_fanout_entry_not_a_failure() {
        let out = merge_search(
            "*:*",
            vec![SearchFanout {
                root: "b:r".into(),
                host: "b".into(),
                outcome: Err(json!({"code": "denied", "message": "nope"})),
            }],
            vec![json!({"host": "c", "status": "unreachable"})],
            50,
            &[],
        );
        assert_eq!(out["fanout"][0]["error"]["code"], "denied");
        assert_eq!(out["hosts_unavailable"][0]["host"], "c");
    }

    #[test]
    fn an_empty_expansion_explains_itself() {
        let out = merge_search("*:nope", vec![], vec![], 50, &["kai:src".into()]);
        assert_eq!(out["reason"]["code"], "root_pattern_matched_no_roots");
        assert!(out["reason"]["hint"].as_str().unwrap().contains("kai:src"));
    }
}
