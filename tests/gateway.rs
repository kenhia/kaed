//! Gateway integration tests (sprint 010): two real kaed instances over
//! real HTTP, one declaring the other as a routable peer. The assertions
//! are the sprint's load-bearing promises — journal attribution surviving
//! the hop (PD-4), verbatim error passthrough (D-3), unreachable-as-data
//! (D-4), fleet search under one budget (D-5), and credential handling
//! that refuses rather than impersonates (D-2).

use kaed::config::{AuthEntry, Identity, Limits, Peer, PeerStatus, Resolved, ResolvedRoot};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Instance {
    addr: std::net::SocketAddr,
    _workdir: tempfile::TempDir,
    workdir_path: std::path::PathBuf,
    ct: tokio_util::sync::CancellationToken,
}

async fn start_instance(
    host: &str,
    identities: Vec<Identity>,
    auth_spec: BTreeMap<String, AuthEntry>,
    peers: Option<Vec<Peer>>,
) -> anyhow::Result<Instance> {
    start_instance_with_roots(host, vec![], identities, auth_spec, peers).await
}

/// `start_instance` plus extra roots beyond `{host}:scratch` — each a
/// local root name and an existing directory the caller keeps alive.
/// Every fixture host having exactly ONE root is how #1231 (an `also`
/// target on the same host but a different root) stayed inexpressible in
/// this file until sprint 020.
async fn start_instance_with_roots(
    host: &str,
    extra_roots: Vec<(&str, std::path::PathBuf)>,
    identities: Vec<Identity>,
    auth_spec: BTreeMap<String, AuthEntry>,
    peers: Option<Vec<Peer>>,
) -> anyhow::Result<Instance> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    start_instance_on(listener, host, extra_roots, identities, auth_spec, peers).await
}

/// `start_instance_with_roots` on a listener the caller already bound — the
/// only way to build a pair that declares each OTHER (025): both addresses
/// have to exist before either config does.
async fn start_instance_on(
    listener: tokio::net::TcpListener,
    host: &str,
    extra_roots: Vec<(&str, std::path::PathBuf)>,
    identities: Vec<Identity>,
    auth_spec: BTreeMap<String, AuthEntry>,
    peers: Option<Vec<Peer>>,
) -> anyhow::Result<Instance> {
    let workdir = tempfile::tempdir()?;
    let workdir_path = workdir.path().canonicalize()?;
    std::fs::write(
        workdir_path.join("hello.txt"),
        "fn old_name() {\n    body\n}\n",
    )?;
    let mut roots = vec![ResolvedRoot {
        description: Some(format!("{host} scratch")),
        ..ResolvedRoot::with_default_classify(format!("{host}:scratch"), workdir_path.clone())
    }];
    for (name, path) in extra_roots {
        roots.push(ResolvedRoot::with_default_classify(
            format!("{host}:{name}"),
            path,
        ));
    }
    let resolved = Resolved {
        bind: "127.0.0.1:0".parse()?,
        allowed_hosts: vec![],
        host: host.into(),
        roots,
        peers,
        identities,
        limits: Limits::default(),
        journal_path: workdir_path.join("journal.db"),
        journal_retention_days: 7,
        deny: std::sync::Arc::new(kaed::deny::DenyList::empty()),
        classify: std::sync::Arc::new(kaed::policy::Classifier::empty()),
        auth: auth_spec,
        whois: kaed::config::WhoisConfig::default(),
        secrets: Default::default(),
    };
    let (app, _auth) = kaed::server::build_app(resolved)?;
    let addr = listener.local_addr()?;
    let ct = tokio_util::sync::CancellationToken::new();
    tokio::spawn({
        let ct = ct.clone();
        async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move { ct.cancelled_owned().await })
                .await;
        }
    });
    Ok(Instance {
        addr,
        _workdir: workdir,
        workdir_path,
        ct,
    })
}

/// An identity is a name. Since 024 there is no other shape: the bearer
/// the fixtures used to carry is gone from the struct entirely.
fn identity(author: &str) -> Identity {
    Identity {
        author: author.into(),
        nodes: std::sync::Arc::new(Vec::new()),
    }
}

/// A gateway ("alpha") declaring one routable peer ("beta"). Neither holds
/// a credential of any kind — alpha forwards the caller's declared name.
/// `ghcp` is an identity on alpha and deliberately NOT on beta, which is
/// what makes the refusal path testable.
async fn start_pair() -> anyhow::Result<(Instance, Instance, tempfile::TempDir)> {
    start_pair_with_beta_roots(vec![]).await
}

/// `start_pair` with extra roots on beta — the two-root peer #1231's
/// regression test needs.
async fn start_pair_with_beta_roots(
    beta_extra_roots: Vec<(&str, std::path::PathBuf)>,
) -> anyhow::Result<(Instance, Instance, tempfile::TempDir)> {
    let beta = start_instance_with_roots(
        "beta",
        beta_extra_roots,
        vec![identity("claude")],
        BTreeMap::new(),
        None,
    )
    .await?;

    // Kept so the signature still hands callers a tempdir to hold: some
    // tests write fixture files into it. Nothing credential-shaped lives
    // here any more.
    let secrets = tempfile::tempdir()?;
    let alpha = start_instance(
        "alpha",
        vec![identity("claude"), identity("ghcp")],
        BTreeMap::new(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    )
    .await?;
    Ok((alpha, beta, secrets))
}

/// Connect by DECLARED IDENTITY (023) rather than by bearer: the header a
/// real client sends after the cutover.
async fn connect_as(
    addr: std::net::SocketAddr,
    author: &str,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>> {
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        http::HeaderName::from_static("x-homelab-agent"),
        http::HeaderValue::from_str(author)?,
    );
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp"))
            .custom_headers(headers),
    );
    Ok(ClientInfo::default().serve(transport).await?)
}

fn structured(result: &rmcp::model::CallToolResult) -> Value {
    result
        .structured_content
        .clone()
        .expect("tool result carries structured content")
}

fn args(v: Value) -> serde_json::Map<String, Value> {
    v.as_object().expect("args are an object").clone()
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>,
    tool: &str,
    arguments: Value,
) -> rmcp::model::CallToolResult {
    client
        .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(args(arguments)))
        .await
        .expect("tool call completes")
}

/// One raw `tools/call` on a conformant `2026-07-28` session, returning the
/// JSON-RPC `result` object as it goes on the wire.
///
/// Raw because rmcp's client cannot make this request — it omits the
/// per-request `_meta` its own server requires (016 D-2), which is precisely
/// why the bug this guards against was invisible from the Rust test suite
/// and only showed up from cleo.
async fn raw_call_at_2026_07_28(
    addr: std::net::SocketAddr,
    author: &str,
    tool: &str,
    arguments: Value,
) -> anyhow::Result<Value> {
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "probe", "version": "0"}
    });
    let http = reqwest::Client::new();
    let url = format!("http://{addr}/mcp");
    let common = |req: reqwest::RequestBuilder| {
        req.header("x-homelab-agent", author)
            .header("accept", "application/json, text/event-stream")
            .header("content-type", "application/json")
    };
    // 2026-07-28 is the inline lifecycle: initialize issues no session id,
    // and each later request carries its own version and capabilities.
    common(http.post(&url))
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": {"name": "probe", "version": "0"}
            }
        }))
        .send()
        .await?;
    let resp = common(http.post(&url))
        .header("mcp-protocol-version", "2026-07-28")
        // SEP-2243 standard headers, which the revision also requires.
        .header("mcp-method", "tools/call")
        .header("mcp-name", tool)
        .json(&json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments, "_meta": meta}
        }))
        .send()
        .await?;
    let text = resp.text().await?;
    let frame = text
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .find(|frame| !frame.trim().is_empty())
        .unwrap_or(&text);
    let answer: Value =
        serde_json::from_str(frame).map_err(|e| anyhow::anyhow!("{e}: not JSON-RPC: {text:?}"))?;
    Ok(answer["result"].clone())
}

/// PD-4, the invariant the whole design turned on: an edit proxied through
/// the gateway is journaled on the BACKEND, under the CALLER's identity —
/// and nowhere on the gateway (D-7).
#[tokio::test]
async fn a_proxied_edit_journals_on_the_backend_under_the_real_author() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let gw = connect_as(alpha.addr, "claude").await?;

    // read through the gateway: identical addressing, remote root
    let read = structured(
        &call(
            &gw,
            "read",
            json!({"root": "beta:scratch", "path": "hello.txt"}),
        )
        .await,
    );
    let version = read["version"].as_str().expect("version").to_string();
    assert!(read["content"].as_str().unwrap().contains("old_name"));

    let edit = call(
        &gw,
        "edit",
        json!({
            "root": "beta:scratch",
            "base": [{"path": "hello.txt", "version": version}],
            "ops": [{"op": "anchor_replace", "path": "hello.txt",
                     "old_text": "old_name", "new_text": "new_name"}],
            "intent": "gateway attribution test",
        }),
    )
    .await;
    assert_ne!(edit.is_error, Some(true), "{:?}", edit.structured_content);
    let edit = structured(&edit);
    assert!(edit["diff"].as_str().unwrap().contains("new_name"));

    // the file actually changed on beta's disk
    let on_disk = std::fs::read_to_string(beta.workdir_path.join("hello.txt"))?;
    assert!(on_disk.contains("new_name"));

    // beta's own journal, read directly: authored by claude, not "gateway"
    let direct = connect_as(beta.addr, "claude").await?;
    let journal = structured(&call(&direct, "journal", json!({})).await);
    let txn = &journal["entries"][0];
    assert_eq!(txn["kind"], "txn");
    assert_eq!(txn["author"], "claude");
    assert_eq!(txn["root"], "beta:scratch");
    assert_eq!(txn["intent"], "gateway attribution test");

    // the same rows via the gateway: a peer root filter proxies (D-6)
    let via_gw = structured(&call(&gw, "journal", json!({"root": "beta:scratch"})).await);
    assert_eq!(via_gw["entries"][0]["author"], "claude");
    assert_eq!(via_gw["entries"][0]["root"], "beta:scratch");

    // and the gateway's own journal never saw it (D-7)
    let local = structured(&call(&gw, "journal", json!({})).await);
    assert_eq!(local["entries"].as_array().unwrap().len(), 0);

    let _ = gw.cancel().await;
    let _ = direct.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// D-3: the re-anchor-without-re-reading loop must survive the hop. The
/// conflict delta a backend produces arrives verbatim, plus a root tag.
#[tokio::test]
async fn a_version_conflict_crosses_the_gateway_verbatim() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let gw = connect_as(alpha.addr, "claude").await?;

    // Establish a journaled state first: the conflict delta renders from
    // the journal's blob of the expected version, so that version has to
    // have been produced by a transaction beta retains.
    let read = structured(
        &call(
            &gw,
            "read",
            json!({"root": "beta:scratch", "path": "hello.txt"}),
        )
        .await,
    );
    let v0 = read["version"].as_str().unwrap().to_string();
    let first = structured(
        &call(
            &gw,
            "edit",
            json!({
                "root": "beta:scratch",
                "base": [{"path": "hello.txt", "version": v0}],
                "ops": [{"op": "anchor_replace", "path": "hello.txt",
                         "old_text": "old_name", "new_text": "step_one"}],
            }),
        )
        .await,
    );
    let stale = first["files"][0]["new_version"]
        .as_str()
        .unwrap()
        .to_string();
    // the file moves underneath (out-of-band edit on beta)
    std::fs::write(
        beta.workdir_path.join("hello.txt"),
        "fn renamed() {\n    body\n}\n",
    )?;

    let conflict = call(
        &gw,
        "edit",
        json!({
            "root": "beta:scratch",
            "base": [{"path": "hello.txt", "version": stale}],
            "ops": [{"op": "anchor_replace", "path": "hello.txt",
                     "old_text": "old_name", "new_text": "other"}],
        }),
    )
    .await;
    assert_eq!(conflict.is_error, Some(true));
    let e = structured(&conflict);
    assert_eq!(e["code"], "version_conflict");
    assert_eq!(e["data"]["expected_version"], stale);
    assert!(
        e["data"]["delta"].as_str().unwrap().contains("renamed"),
        "the delta names what changed: {}",
        e["data"]["delta"]
    );
    // the one thing passthrough adds
    assert_eq!(e["root"], "beta:scratch");

    let _ = gw.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// D-4: while the peer answers, its roots are part of the gateway's
/// namespace, observed and versioned; when it stops answering it becomes
/// `unreachable` WITH `since` — in `roots` and on the call path — and its
/// last-known roots stay visible, labelled, instead of vanishing.
#[tokio::test]
async fn an_unreachable_peer_is_data_not_a_connection_failure() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let up = structured(&call(&gw, "roots", json!({})).await);
    let beta_root = up["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "beta:scratch")
        .expect("peer root aggregated");
    assert_eq!(beta_root["status"], "active");
    assert!(
        beta_root["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("edit")),
        "peer capabilities pass through per-root"
    );
    let beta_host = up["fleet"]["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "beta")
        .unwrap()
        .clone();
    assert_eq!(beta_host["verified"], true);
    assert_eq!(beta_host["probe"]["status"], "ok");
    assert!(beta_host["version"].as_str().is_some(), "observed version");
    assert_eq!(beta_host["roots"][0], "beta:scratch");

    // The peer goes away. Teardown is graceful, so the gateway's pooled
    // connection can win one last race — poll (bounded) until the outage
    // is observed; WHAT is reported once it is, is the assertion.
    beta.ct.cancel();
    let mut down = json!(null);
    for _ in 0..100 {
        down = structured(&call(&gw, "roots", json!({})).await);
        let status = &down["fleet"]["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["host"] == "beta")
            .unwrap()["status"];
        if status == "unreachable" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let host = down["fleet"]["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "beta")
        .unwrap()
        .clone();
    assert_eq!(host["status"], "unreachable", "outage never observed");
    assert_eq!(
        host["verified"], true,
        "we checked; that IS the observation"
    );
    assert!(
        host["since"].as_str().is_some(),
        "an outage carries its date"
    );
    // last-known namespace survives the outage, labelled as such
    let cached = down["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "beta:scratch")
        .expect("cached peer root still visible");
    assert_eq!(cached["status"], "unreachable");

    // and the call path says the same thing, structured
    let stat = call(&gw, "stat", json!({"root": "beta:scratch", "path": ""})).await;
    assert_eq!(stat.is_error, Some(true));
    let e = structured(&stat);
    assert_eq!(e["code"], "not_found");
    assert_eq!(e["data"]["reason"], "host_unreachable");
    assert_eq!(e["data"]["this_host"], "alpha");
    assert_eq!(e["data"]["target_host"], "beta");
    assert!(e["data"]["since"].as_str().is_some());

    let _ = gw.cancel().await;
    alpha.ct.cancel();
    Ok(())
}

/// 023 D-3, and PD-4's guarantee in the form that outlived the token
/// matrix: the gateway forwards the caller's OWN declared name and never
/// substitutes one. A peer that does not allow-list that name refuses the
/// call — which is the right outcome, because a misattributed journal row
/// on the peer would be worse than a refused call.
///
/// `ghcp` is an identity on alpha and not on beta, so this is the whole
/// proof in one hop: alpha could trivially have reached beta as `claude`
/// (it allow-lists that name too), and does not.
#[tokio::test]
async fn a_forwarded_identity_the_peer_rejects_is_refused_not_substituted() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let gw = connect_as(alpha.addr, "ghcp").await?;

    let stat = call(&gw, "stat", json!({"root": "beta:scratch", "path": ""})).await;
    assert_eq!(stat.is_error, Some(true));
    let e = structured(&stat);
    assert_eq!(e["code"], "denied");
    assert_eq!(e["data"]["reason"], "peer_credential_rejected");
    assert_eq!(
        e["data"]["author"], "ghcp",
        "the caller's own name, not alpha's"
    );
    assert_eq!(e["data"]["target_host"], "beta");
    // The remedy is the allow-list and a restart, not a credential.
    let hint = e["data"]["hint"].as_str().unwrap();
    assert!(hint.contains("[auth]"), "{hint}");
    assert!(hint.contains("RESTART"), "{hint}");

    let _ = gw.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// 023's point, as one assertion, and since 024 the only shape a gateway
/// has: alpha declares beta as a URL and nothing else, and the proxied call
/// lands journaled on beta under the caller's name. Nine credentials became
/// zero and identity fidelity did not move.
#[tokio::test]
async fn a_gateway_with_no_peer_credentials_still_proxies_as_the_caller() -> anyhow::Result<()> {
    let beta = start_instance("beta", vec![identity("claude")], BTreeMap::new(), None).await?;
    let alpha = start_instance(
        "alpha",
        vec![identity("claude")],
        BTreeMap::new(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    )
    .await?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let edit = call(
        &gw,
        "edit",
        json!({
            "root": "beta:scratch",
            "ops": [{"op": "create", "path": "forwarded.txt", "content": "landed\n"}],
            "intent": "023 forwarded identity",
        }),
    )
    .await;
    assert_ne!(
        edit.is_error,
        Some(true),
        "proxied edit with no peer credential: {:?}",
        edit.structured_content
    );
    assert_eq!(
        std::fs::read_to_string(beta.workdir_path.join("forwarded.txt"))?,
        "landed\n"
    );

    // Beta's own journal is the record, and it says `claude` — not
    // `alpha`, and not a shared gateway identity.
    let rows = structured(&call(&gw, "journal", json!({"root": "beta:scratch"})).await);
    let txn = rows["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "txn")
        .expect("the proxied edit is journaled on beta");
    assert_eq!(txn["author"], "claude");

    let _ = gw.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// D-5: one call, every root on every reachable host, one budget, and
/// every layer's drops attributed to it.
#[tokio::test]
async fn fleet_search_merges_across_hosts_with_per_root_reporting() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    std::fs::write(alpha.workdir_path.join("a.txt"), "needle here\n")?;
    std::fs::write(
        beta.workdir_path.join("b.txt"),
        "needle there\nneedle again\n",
    )?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let out = structured(&call(&gw, "search", json!({"root": "*:*", "pattern": "needle"})).await);
    assert_eq!(out["fleet"], true);
    let matches = out["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 3);
    // stable order: the gateway's own roots first, every match root-tagged
    assert_eq!(matches[0]["root"], "alpha:scratch");
    assert_eq!(matches[1]["root"], "beta:scratch");
    let fanout = out["fanout"].as_array().unwrap();
    assert_eq!(fanout.len(), 2);
    assert!(
        fanout
            .iter()
            .all(|f| f["files_searched"].as_u64().is_some())
    );
    assert!(out.get("hosts_unavailable").is_none());

    // one budget across the fleet, the merge's own drops attributed
    let capped = structured(
        &call(
            &gw,
            "search",
            json!({"root": "*:*", "pattern": "needle", "max_results": 1}),
        )
        .await,
    );
    assert_eq!(capped["matches"].as_array().unwrap().len(), 1);
    assert_eq!(capped["truncated"], true);

    // a pattern that selects nothing explains itself (#1066, fleet edition)
    let none = structured(
        &call(
            &gw,
            "search",
            json!({"root": "*:nope", "pattern": "needle"}),
        )
        .await,
    );
    assert_eq!(none["reason"]["code"], "root_pattern_matched_no_roots");
    assert!(
        none["reason"]["hint"]
            .as_str()
            .unwrap()
            .contains("alpha:scratch")
    );

    // patterns stay search-only: no honest atomicity story anywhere else
    let bad = call(&gw, "stat", json!({"root": "*:*", "path": ""})).await;
    assert_eq!(bad.is_error, Some(true));
    let e = structured(&bad);
    assert_eq!(e["code"], "invalid_input");
    assert!(e["message"].as_str().unwrap().contains("search"));

    let _ = gw.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// korg #1089, reproduced with kubsdb's exact topology: a peer that
/// **declares** the fleet (PD-5) while being a leaf and not a gateway.
///
/// `beta:*` addressed to alpha used to route on the host prefix (010 D-1)
/// before the pattern was recognised (010 D-5), so the whole call forwarded
/// and beta ran the fan-out. Beta's honest answer — "I have no credential
/// for alpha" — arrived at a caller who is *talking to* alpha, describing a
/// degraded fleet that is not degraded. The matches were always right; the
/// one field whose job is preventing a wrong world-model was the casualty.
#[tokio::test]
async fn a_single_peer_pattern_is_expanded_by_the_host_that_was_asked() -> anyhow::Result<()> {
    let beta_port = {
        // beta must declare alpha, so it needs alpha's address first — bind
        // a throwaway listener to reserve a port number for alpha.
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        l.local_addr()?.port()
    };
    let beta = start_instance(
        "beta",
        vec![identity("claude")],
        BTreeMap::new(),
        // declared and routable — kubsdb's shape exactly
        Some(vec![Peer {
            url: Some(format!("http://127.0.0.1:{beta_port}/mcp")),
            ..Peer::declared("alpha", PeerStatus::Active)
        }]),
    )
    .await?;

    let alpha = start_instance(
        "alpha",
        vec![
            identity("claude"),
            // deliberately NOT allow-listed on beta, so this author has a
            // real fleet gap to report — the control for the assertions
            // below
            identity("ghcp"),
        ],
        BTreeMap::new(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    )
    .await?;

    std::fs::write(alpha.workdir_path.join("a.txt"), "needle here\n")?;
    std::fs::write(beta.workdir_path.join("b.txt"), "needle there\n")?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let single = structured(
        &call(
            &gw,
            "search",
            json!({"root": "beta:*", "pattern": "needle"}),
        )
        .await,
    );
    // The results were never the problem, and must stay right.
    let matches = single["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0]["root"], "beta:scratch");
    assert_eq!(single["fleet"], true);
    // The finding itself: no borrowed world-model.
    assert!(
        single.get("hosts_unavailable").is_none(),
        "the peer's reachability answered the caller's question: {single:#}"
    );

    // The gate from the work item: `beta:*` and `*:*` agree about the fleet.
    let all = structured(&call(&gw, "search", json!({"root": "*:*", "pattern": "needle"})).await);
    assert!(all.get("hosts_unavailable").is_none());
    let hosts: Vec<&str> = single["fanout"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["host"].as_str().unwrap())
        .collect();
    assert_eq!(hosts, ["beta"], "only the named host is in the fan-out");

    // The routing half, tested on what actually distinguishes it: the
    // instance that ANSWERS a pattern is the one whose world-model the
    // caller gets. Alpha knows both hosts' roots; beta, holding no
    // credential for alpha, knows only its own — so if this call were still
    // forwarded, the hint would be missing half the fleet.
    let none = structured(
        &call(
            &gw,
            "search",
            json!({"root": "beta:nop*", "pattern": "needle"}),
        )
        .await,
    );
    assert_eq!(none["reason"]["code"], "root_pattern_matched_no_roots");
    let hint = none["reason"]["hint"].as_str().unwrap();
    assert!(
        hint.contains("alpha:scratch") && hint.contains("beta:scratch"),
        "the answering host expands the pattern against the whole fleet it can see: {hint}"
    );

    // …and a host the pattern excludes is not probed, so it can never be
    // reported unavailable either. `alpha:*` must not mention beta at all.
    let local = structured(
        &call(
            &gw,
            "search",
            json!({"root": "alpha:*", "pattern": "needle"}),
        )
        .await,
    );
    let hosts: Vec<&str> = local["fanout"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["host"].as_str().unwrap())
        .collect();
    assert_eq!(hosts, ["alpha"]);
    assert!(local.get("hosts_unavailable").is_none());

    // The same lie the gateway could tell, and the control that keeps the
    // fix from being a mute button. ghcp genuinely has no beta credential:
    //   - under `alpha:*`, beta is not part of the search, so reporting it
    //     unavailable would be #1089 with the hosts swapped;
    //   - under `*:*`, beta IS part of the search and the gap is real, so
    //     it must still be reported (D-5's no-silent-gap rule).
    let stranger = connect_as(alpha.addr, "ghcp").await?;
    let scoped = structured(
        &call(
            &stranger,
            "search",
            json!({"root": "alpha:*", "pattern": "needle"}),
        )
        .await,
    );
    assert!(
        scoped.get("hosts_unavailable").is_none(),
        "a host the pattern excludes has no reachability to report: {scoped:#}"
    );
    let fleetwide = structured(
        &call(
            &stranger,
            "search",
            json!({"root": "*:*", "pattern": "needle"}),
        )
        .await,
    );
    assert_eq!(fleetwide["hosts_unavailable"][0]["host"], "beta");
    // Refused, not unreachable: beta answered and declined the forwarded
    // name. Reporting an outage here would be a false claim about the
    // world in the one field whose job is preventing exactly that.
    assert_eq!(
        fleetwide["hosts_unavailable"][0]["status"],
        "identity_refused"
    );
    assert_eq!(fleetwide["hosts_unavailable"][0]["author"], "ghcp");

    let _ = stranger.cancel().await;
    let _ = gw.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// Sprint 011, PD-3's payoff: a secret written on one host reaches another
/// by REFERENCE. The agent passes a handle (`value_from`), the gateway
/// moves the bytes in its own memory, the source host journals the exit,
/// the target journals a redacted write — and no tool response anywhere in
/// the exchange ever contains the value.
#[tokio::test]
async fn a_cross_host_value_from_carries_the_secret_without_the_agent() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    std::fs::write(
        alpha.workdir_path.join(".env"),
        format!("SERVICE_TOKEN={VALUE}\n"),
    )?;
    let digest = kaed::secrets::digest_of(VALUE);
    let gw = connect_as(alpha.addr, "claude").await?;

    // gateway-local source → remote target, one proxied edit
    let edit = call(
        &gw,
        "edit",
        json!({
            "root": "beta:scratch",
            "base": [],
            "ops": [
                {"op": "create", "path": "svc.env", "content": ""},
                {"op": "env_set", "path": "svc.env", "key": "CLIENT_TOKEN",
                 "value_from": {"root": "alpha:scratch", "path": ".env",
                                "key": "SERVICE_TOKEN", "digest": digest}}
            ],
            "intent": "hand the service its token by reference"
        }),
    )
    .await;
    assert_ne!(edit.is_error, Some(true), "{:?}", edit.structured_content);
    assert!(
        !serde_json::to_string(&edit.structured_content)?.contains(VALUE),
        "the edit response leaked the value"
    );
    let on_beta = std::fs::read_to_string(beta.workdir_path.join("svc.env"))?;
    assert!(
        on_beta.contains(&format!("CLIENT_TOKEN={VALUE}")),
        "the value landed on beta: {on_beta}"
    );

    // the SOURCE host journaled the exit: a transport event, disclosed,
    // with the claimed destination (D-5/D-6)
    let exit = structured(&call(&gw, "journal", json!({"kind": ["secret"]})).await);
    let event = &exit["entries"][0];
    assert_eq!(event["action"], "transport");
    assert_eq!(event["disclosed"], true);
    assert_eq!(event["root"], "alpha:scratch");
    assert_eq!(event["key"], "SERVICE_TOKEN");
    assert_eq!(event["destination"], "beta:scratch/svc.env");
    assert_eq!(event["author"], "claude");

    // the TARGET journaled a redacted transaction under the real author
    let direct = connect_as(beta.addr, "claude").await?;
    let beta_journal = structured(&call(&direct, "journal", json!({})).await);
    let txn = &beta_journal["entries"][0];
    assert_eq!(txn["kind"], "txn");
    assert_eq!(txn["author"], "claude");

    // a stale digest refuses loudly BEFORE anything crosses (PD-3)
    let stale = call(
        &gw,
        "edit",
        json!({
            "root": "beta:scratch",
            "base": [],
            "ops": [{"op": "env_set", "path": "svc.env", "key": "AGAIN",
                     "value_from": {"root": "alpha:scratch", "path": ".env",
                                    "key": "SERVICE_TOKEN",
                                    "digest": "0000000000000000"}}],
        }),
    )
    .await;
    assert_eq!(stale.is_error, Some(true));
    assert_eq!(structured(&stale)["data"]["reason"], "digest_mismatch");

    let _ = gw.cancel().await;
    let _ = direct.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// The reverse direction: the source is on the PEER, the target is local.
/// The gateway fetches through the peer's `secret_reveal` under the
/// caller's identity, so the disclosure is journaled on the host the value
/// left — as a transport with the claimed destination, not an agent reveal.
#[tokio::test]
async fn a_peer_sourced_value_from_journals_the_exit_on_the_peer() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    const VALUE: &str = "2f6b8a1e9c4d4e7ab3f50d8c6a2e9b41";
    std::fs::write(
        beta.workdir_path.join(".env"),
        format!("ISSUED_TOKEN={VALUE}\n"),
    )?;
    std::fs::write(alpha.workdir_path.join("app.env"), "")?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let version = kaed::fsops::version_of(b"");
    let edit = call(
        &gw,
        "edit",
        json!({
            "root": "alpha:scratch",
            "base": [{"path": "app.env", "version": version}],
            "ops": [{"op": "env_set", "path": "app.env", "key": "TOKEN",
                     "value_from": {"root": "beta:scratch", "path": ".env",
                                    "key": "ISSUED_TOKEN"}}],
        }),
    )
    .await;
    assert_ne!(edit.is_error, Some(true), "{:?}", edit.structured_content);
    assert!(
        std::fs::read_to_string(alpha.workdir_path.join("app.env"))?
            .contains(&format!("TOKEN={VALUE}"))
    );
    assert!(
        !serde_json::to_string(&edit.structured_content)?.contains(VALUE),
        "the edit response leaked the value"
    );

    // beta journaled the disclosure; alpha's secret stream stays empty (the
    // value ARRIVED here, it did not leave here)
    let direct = connect_as(beta.addr, "claude").await?;
    let exit = structured(&call(&direct, "journal", json!({"kind": ["secret"]})).await);
    let event = &exit["entries"][0];
    assert_eq!(event["action"], "transport");
    assert_eq!(event["author"], "claude");
    assert_eq!(event["destination"], "alpha:scratch/app.env");
    let local = structured(&call(&gw, "journal", json!({"kind": ["secret"]})).await);
    assert_eq!(local["entries"].as_array().unwrap().len(), 0);

    let _ = gw.cancel().await;
    let _ = direct.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// PD-3's preferred path over the transport fallback: rotate on the
/// gateway with a remote `also` target writes both hosts in one call —
/// the same fresh value, never seen by the agent, each host journaling its
/// own side.
#[tokio::test]
async fn rotate_writes_both_hosts_via_a_remote_also_target() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    std::fs::write(
        alpha.workdir_path.join(".env"),
        format!("SHARED_TOKEN={VALUE}\n"),
    )?;
    std::fs::write(
        beta.workdir_path.join(".env"),
        format!("SHARED_TOKEN={VALUE}\n"),
    )?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let rotated = call(
        &gw,
        "secret",
        json!({
            "action": "rotate", "root": "alpha:scratch", "path": ".env",
            "key": "SHARED_TOKEN",
            "version": kaed::fsops::version_of(format!("SHARED_TOKEN={VALUE}\n").as_bytes()),
            "also": [{"root": "beta:scratch", "path": ".env",
                      "version": kaed::fsops::version_of(format!("SHARED_TOKEN={VALUE}\n").as_bytes())}],
        }),
    )
    .await;
    assert_ne!(
        rotated.is_error,
        Some(true),
        "{:?}",
        rotated.structured_content
    );
    let r = structured(&rotated);
    assert_eq!(r["targets"].as_array().unwrap().len(), 2);
    assert!(r["targets"][1]["applied"].as_bool().unwrap(), "{r}");

    let on_alpha = std::fs::read_to_string(alpha.workdir_path.join(".env"))?;
    let on_beta = std::fs::read_to_string(beta.workdir_path.join(".env"))?;
    let new_value = on_alpha
        .lines()
        .find_map(|l| l.strip_prefix("SHARED_TOKEN="))
        .expect("rotated key");
    assert_ne!(new_value, VALUE);
    assert_eq!(on_alpha, on_beta, "both hosts hold the same new value");
    assert!(
        !serde_json::to_string(&r)?.contains(new_value),
        "the rotate response leaked the value"
    );

    // each side journaled its own half: rotate + transport on the gateway,
    // an ordinary redacted txn on the peer under the real author
    let stream = structured(&call(&gw, "journal", json!({"kind": ["secret"]})).await);
    let actions: Vec<&str> = stream["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["transport", "rotate"]);
    let direct = connect_as(beta.addr, "claude").await?;
    let beta_journal = structured(&call(&direct, "journal", json!({})).await);
    assert_eq!(beta_journal["entries"][0]["author"], "claude");

    let _ = gw.cancel().await;
    let _ = direct.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// #1231 (sprint 020): an `also` target on the SAME host as the primary
/// but a DIFFERENT root used to be classed remote by the whole-root-string
/// partition — `write_value_to_peer` then asked the executing host for
/// itself as a peer, got `None`, and refused with an `unknown_root` that
/// listed the root it claimed not to serve. The fix routes it as a local
/// write in its own transaction: applied independently, reported
/// per-target with its OWN `txn_id` (020 D-1/D-3), journaled as a second
/// `rotate` row and never a `transport` row, because the value never
/// left the host (020 D-2).
#[tokio::test]
async fn rotate_writes_a_same_host_cross_root_also_target_locally() -> anyhow::Result<()> {
    const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    let etc = tempfile::tempdir()?;
    let etc_path = etc.path().canonicalize()?;
    std::fs::write(etc_path.join("svc.env"), format!("SHARED_TOKEN={VALUE}\n"))?;
    let (alpha, beta, _secrets) =
        start_pair_with_beta_roots(vec![("etc", etc_path.clone())]).await?;
    std::fs::write(
        beta.workdir_path.join(".env"),
        format!("SHARED_TOKEN={VALUE}\n"),
    )?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let version = kaed::fsops::version_of(format!("SHARED_TOKEN={VALUE}\n").as_bytes());
    let rotated = call(
        &gw,
        "secret",
        json!({
            "action": "rotate", "root": "beta:scratch", "path": ".env",
            "key": "SHARED_TOKEN", "version": version,
            "also": [{"root": "beta:etc", "path": "svc.env", "version": version}],
        }),
    )
    .await;
    assert_ne!(
        rotated.is_error,
        Some(true),
        "{:?}",
        rotated.structured_content
    );
    let r = structured(&rotated);

    // Per-target report: the primary shares the top-level txn_id; the
    // cross-root target applied in its own transaction and says which.
    let targets = r["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 2, "{r}");
    assert_eq!(targets[0]["txn_id"], r["txn_id"], "{r}");
    assert!(targets[1]["applied"].as_bool().unwrap(), "{r}");
    assert!(targets[1]["txn_id"].is_i64(), "{r}");
    assert_ne!(targets[1]["txn_id"], r["txn_id"], "{r}");

    // Both files hold the same new value, and the response never held it.
    let on_scratch = std::fs::read_to_string(beta.workdir_path.join(".env"))?;
    let on_etc = std::fs::read_to_string(etc_path.join("svc.env"))?;
    let new_value = on_scratch
        .lines()
        .find_map(|l| l.strip_prefix("SHARED_TOKEN="))
        .expect("rotated key");
    assert_ne!(new_value, VALUE);
    assert_eq!(
        on_etc.lines().find_map(|l| l.strip_prefix("SHARED_TOKEN=")),
        Some(new_value)
    );
    assert!(
        !serde_json::to_string(&r)?.contains(new_value),
        "the rotate response leaked the value"
    );

    // beta journaled one rotate row per location and NO transport; the
    // gateway's secret stream stays empty — the value never touched alpha.
    let direct = connect_as(beta.addr, "claude").await?;
    let stream = structured(&call(&direct, "journal", json!({"kind": ["secret"]})).await);
    let mut rows: Vec<(&str, &str)> = stream["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["action"].as_str().unwrap(), e["root"].as_str().unwrap()))
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [("rotate", "beta:etc"), ("rotate", "beta:scratch")],
        "{stream}"
    );
    let on_gateway = structured(&call(&gw, "journal", json!({"kind": ["secret"]})).await);
    assert_eq!(on_gateway["entries"].as_array().unwrap().len(), 0);

    let _ = gw.cancel().await;
    let _ = direct.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// 016, found by the live test from cleo and not by any of the above: a
/// gateway that serves a NEWER revision than it asks its peers for has to
/// translate in **both** directions.
///
/// kai serves `2026-07-28`, where `resultType` is mandatory and the
/// absent-means-complete bridge is explicitly unavailable. It asks peers at
/// `PEER_PROTOCOL_VERSION` (016 D-2), where a peer correctly omits the
/// field. Returning that envelope verbatim put a legitimately-absent field
/// onto a session where it is not legitimate, and the client rejected the
/// whole result — so the call's real outcome never arrived.
///
/// rmcp's `strip_result_type_for_legacy_peer` covers modern→legacy for
/// results rmcp builds itself. Nothing stamped one that arrived from a peer
/// already deserialized.
#[tokio::test]
async fn a_proxied_result_is_stamped_for_the_revision_it_is_returned_on() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;

    // the local half, as the control: kai's own result is rmcp's to build
    let local = raw_call_at_2026_07_28(
        alpha.addr,
        "claude",
        "stat",
        json!({"root": "alpha:scratch", "path": "hello.txt"}),
    )
    .await?;
    assert_eq!(local["isError"], false, "local stat failed: {local}");
    assert_eq!(local["resultType"], "complete");

    // and the proxied half, which is the one that regressed
    let proxied = raw_call_at_2026_07_28(
        alpha.addr,
        "claude",
        "stat",
        json!({"root": "beta:scratch", "path": "hello.txt"}),
    )
    .await?;
    assert_eq!(proxied["isError"], false, "proxied stat failed: {proxied}");
    assert_eq!(
        proxied["resultType"], "complete",
        "a peer result returned on a 2026-07-28 session must carry resultType: {proxied}"
    );

    // a proxied ERROR result travels the same path and needs the same stamp
    let refused = raw_call_at_2026_07_28(
        alpha.addr,
        "claude",
        "stat",
        json!({"root": "beta:scratch", "path": "nope.txt"}),
    )
    .await?;
    assert_eq!(refused["isError"], true, "expected a refusal: {refused}");
    assert_eq!(refused["resultType"], "complete");

    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

// ------------------------------------------------------- 025: the hop guard

/// `connect_as`, plus the marker a gateway puts on every session it opens to
/// a peer (025 D-1). A client can set it by hand — nothing checks it, and
/// this is how the guard is tested without standing up a third instance.
async fn connect_as_forwarded(
    addr: std::net::SocketAddr,
    author: &str,
    from: &str,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>> {
    let mut headers = std::collections::HashMap::new();
    headers.insert(
        http::HeaderName::from_static("x-homelab-agent"),
        http::HeaderValue::from_str(author)?,
    );
    headers.insert(
        http::HeaderName::from_static("x-kaed-hop"),
        http::HeaderValue::from_str(from)?,
    );
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp"))
            .custom_headers(headers),
    );
    Ok(ClientInfo::default().serve(transport).await?)
}

/// The fleet's peer declarations are SYMMETRIC — kai declares kubs0 and
/// kubs0 declares kai — which is what turned one `roots` call into a storm
/// that filled every host's descriptor table in under half a minute
/// (WI 2587). This is that topology in two instances.
async fn start_symmetric_pair() -> anyhow::Result<(Instance, Instance)> {
    // Both listeners first: each config has to name the other's address.
    let alpha_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let beta_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let (alpha_addr, beta_addr) = (alpha_listener.local_addr()?, beta_listener.local_addr()?);
    let declaring = |host: &'static str, addr: std::net::SocketAddr| {
        Some(vec![Peer {
            url: Some(format!("http://{addr}/mcp")),
            ..Peer::declared(host, PeerStatus::Active)
        }])
    };
    let alpha = start_instance_on(
        alpha_listener,
        "alpha",
        vec![],
        vec![identity("claude")],
        BTreeMap::new(),
        declaring("beta", beta_addr),
    )
    .await?;
    let beta = start_instance_on(
        beta_listener,
        "beta",
        vec![],
        vec![identity("claude")],
        BTreeMap::new(),
        declaring("alpha", alpha_addr),
    )
    .await?;
    Ok((alpha, beta))
}

/// WI 2587's mechanism, in the smallest topology that has it: two hosts
/// declaring each other. Before 025 this call recursed — alpha probing beta,
/// beta probing alpha, each level under a 30 s timeout while the level below
/// kept going — so it answered in ~30 s with beta reported unreachable, and
/// left an unbounded number of sessions behind.
///
/// The 10 s budget is the assertion: a fleet this size answers in
/// milliseconds, and anything approaching `CALL_TIMEOUT` means the recursion
/// is back.
#[tokio::test]
async fn a_symmetric_peer_mesh_answers_roots_without_recursing() -> anyhow::Result<()> {
    let (alpha, beta) = start_symmetric_pair().await?;
    let gw = connect_as(alpha.addr, "claude").await?;

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        call(&gw, "roots", json!({})),
    )
    .await
    .map_err(|_| anyhow::anyhow!("`roots` did not answer in 10s — the fan-out is recursing"))?;

    let v = structured(&result);
    let beta_host = v["fleet"]["hosts"]
        .as_array()
        .expect("hosts is an array")
        .iter()
        .find(|h| h["host"] == "beta")
        .expect("beta is in the fleet block")
        .clone();
    assert_eq!(
        beta_host["status"], "active",
        "beta answered, so it is active: {beta_host}"
    );
    assert_eq!(beta_host["verified"], true, "{beta_host}");

    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// The guard itself: a call that arrived through a gateway answers from local
/// knowledge and probes nobody. The peer is up and would answer — so a
/// `skipped` probe here is the rule being applied, not an outage.
#[tokio::test]
async fn a_forwarded_roots_probes_no_peers_and_says_why() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;

    let direct = connect_as(alpha.addr, "claude").await?;
    let direct = structured(&call(&direct, "roots", json!({})).await);
    let probed = direct["fleet"]["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "beta")
        .unwrap()
        .clone();
    assert_eq!(probed["verified"], true, "control: a direct call probes");

    let fwd = connect_as_forwarded(alpha.addr, "claude", "kubs0").await?;
    let forwarded = structured(&call(&fwd, "roots", json!({})).await);
    let hosts = forwarded["fleet"]["hosts"].as_array().unwrap();
    let beta_host = hosts.iter().find(|h| h["host"] == "beta").unwrap();
    assert_eq!(beta_host["verified"], false, "{beta_host}");
    assert_eq!(beta_host["probe"]["status"], "skipped", "{beta_host}");
    let detail = beta_host["probe"]["detail"].as_str().unwrap();
    assert!(
        detail.contains("kubs0") && detail.contains("forwarded"),
        "the skip should name the hop it came from: {detail}"
    );
    // The host's own roots are still the whole point of the answer.
    let names: Vec<&str> = forwarded["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha:scratch"], "{forwarded}");

    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// The same rule on the other fan-out site: a root PATTERN on a forwarded
/// call expands over this host's own roots only. 014 D-5 already says a
/// pattern is never proxied, so this arm is unreachable from a well-behaved
/// gateway — and the peer it did not probe is still reported, because "the
/// fleet was searched" must never be quietly narrower than the fleet.
#[tokio::test]
async fn a_forwarded_pattern_search_expands_locally_and_reports_the_peer() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let fwd = connect_as_forwarded(alpha.addr, "claude", "kubs0").await?;

    let v = structured(
        &call(
            &fwd,
            "search",
            json!({"root": "*:*", "pattern": "old_name"}),
        )
        .await,
    );
    let roots_hit: Vec<&str> = v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["root"].as_str().unwrap_or("alpha:scratch"))
        .collect();
    assert!(
        roots_hit.iter().all(|r| r.starts_with("alpha:")),
        "a forwarded pattern must not reach beta: {v}"
    );
    let unavailable = v["hosts_unavailable"].as_array().unwrap();
    let beta_entry = unavailable
        .iter()
        .find(|h| h["host"] == "beta")
        .unwrap_or_else(|| panic!("beta must be reported, not silently dropped: {v}"));
    assert_eq!(beta_entry["status"], "not_probed", "{beta_entry}");

    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// The leak, as a bound rather than a symptom (025 D-2): a burst of
/// concurrent proxied calls to one peer leaves **one** session behind.
///
/// Before 025 `checkout` released the map lock between the miss and the
/// insert, so every call in a burst built its own session — its own reqwest
/// client, its own pool, its own standalone SSE stream — and the insert
/// replaced the previous one without cancelling it. Eight calls, seven
/// leaked sessions, and that is what filled a 1024-descriptor table in
/// under four hours of fleet traffic.
#[tokio::test]
async fn concurrent_proxied_calls_to_one_peer_build_one_session() -> anyhow::Result<()> {
    let beta = start_instance("beta", vec![identity("claude")], BTreeMap::new(), None).await?;
    let peers = std::sync::Arc::new(kaed::fleet::Peers::new(
        "alpha".into(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    ));
    let peer = peers.routable("beta").expect("beta is routable").clone();

    let mut burst = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let (peers, peer) = (peers.clone(), peer.clone());
        burst.spawn(async move {
            peers
                .call(
                    &peer,
                    "claude",
                    "stat",
                    Some(args(json!({"root": "beta:scratch", "path": "hello.txt"}))),
                    None,
                )
                .await
                .map(|r| r.is_error == Some(true))
        });
    }
    while let Some(joined) = burst.join_next().await {
        assert!(
            !joined?.map_err(|e| anyhow::anyhow!("{e}"))?,
            "every call in the burst should succeed"
        );
    }

    assert_eq!(
        peers.sessions_built(),
        1,
        "eight concurrent misses for one (host, author) must BUILD one session, not eight"
    );
    assert_eq!(peers.live_sessions().await, 1, "and hold exactly that one");

    beta.ct.cancel();
    Ok(())
}
