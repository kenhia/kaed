//! Gateway integration tests (sprint 010): two real kaed instances over
//! real HTTP, one declaring the other as a routable peer. The assertions
//! are the sprint's load-bearing promises — journal attribution surviving
//! the hop (PD-4), verbatim error passthrough (D-3), unreachable-as-data
//! (D-4), fleet search under one budget (D-5), and credential handling
//! that refuses rather than impersonates (D-2).

use kaed::config::{
    AuthEntry, Identity, Limits, Peer, PeerStatus, PeerTokenEntry, Resolved, ResolvedRoot,
};
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
    auth: std::sync::Arc<kaed::server::AuthState>,
}

async fn start_instance(
    host: &str,
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
    let resolved = Resolved {
        bind: "127.0.0.1:0".parse()?,
        allowed_hosts: vec![],
        host: host.into(),
        roots: vec![ResolvedRoot {
            description: Some(format!("{host} scratch")),
            ..ResolvedRoot::with_default_classify(format!("{host}:scratch"), workdir_path.clone())
        }],
        peers,
        identities,
        limits: Limits::default(),
        journal_path: workdir_path.join("journal.db"),
        journal_retention_days: 7,
        deny: std::sync::Arc::new(kaed::deny::DenyList::empty()),
        classify: std::sync::Arc::new(kaed::policy::Classifier::empty()),
        auth: auth_spec,
        secrets: Default::default(),
    };
    let (app, auth) = kaed::server::build_app(resolved)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
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
        auth,
    })
}

fn identity(author: &str, token: &str) -> Identity {
    Identity {
        author: author.into(),
        token: token.into(),
        prev_token: None,
    }
}

/// A gateway ("alpha") declaring one routable peer ("beta"), with a claude
/// credential for it in a rotatable file — and a ghcp identity that has
/// deliberately NO beta credential.
async fn start_pair() -> anyhow::Result<(Instance, Instance, tempfile::TempDir)> {
    let beta = start_instance(
        "beta",
        vec![identity("claude", "tok-beta-claude")],
        BTreeMap::new(),
        None,
    )
    .await?;

    let secrets = tempfile::tempdir()?;
    let beta_token_file = secrets.path().join("beta-claude.token");
    std::fs::write(&beta_token_file, "tok-beta-claude\n")?;

    let mut tokens = BTreeMap::new();
    tokens.insert(
        "claude".to_string(),
        PeerTokenEntry {
            token_env: None,
            token_file: Some(beta_token_file.display().to_string()),
        },
    );
    let alpha = start_instance(
        "alpha",
        vec![
            identity("claude", "tok-alpha-claude"),
            identity("ghcp", "tok-alpha-ghcp"),
        ],
        BTreeMap::new(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            tokens,
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    )
    .await?;
    Ok((alpha, beta, secrets))
}

async fn connect(
    addr: std::net::SocketAddr,
    token: &str,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp"))
            .auth_header(token),
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

/// PD-4, the invariant the whole design turned on: an edit proxied through
/// the gateway is journaled on the BACKEND, under the CALLER's identity —
/// and nowhere on the gateway (D-7).
#[tokio::test]
async fn a_proxied_edit_journals_on_the_backend_under_the_real_author() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
    let direct = connect(beta.addr, "tok-beta-claude").await?;
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
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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

/// D-2: no credential for the caller means REFUSE, never impersonate — the
/// misattributed journal row would be worse than the refused call.
#[tokio::test]
async fn an_author_with_no_peer_credential_is_refused_not_impersonated() -> anyhow::Result<()> {
    let (alpha, beta, _secrets) = start_pair().await?;
    let gw = connect(alpha.addr, "tok-alpha-ghcp").await?;

    let stat = call(&gw, "stat", json!({"root": "beta:scratch", "path": ""})).await;
    assert_eq!(stat.is_error, Some(true));
    let e = structured(&stat);
    assert_eq!(e["code"], "denied");
    assert_eq!(e["data"]["reason"], "no_peer_credential");
    assert_eq!(e["data"]["author"], "ghcp");
    assert_eq!(e["data"]["target_host"], "beta");
    assert!(e["data"]["hint"].as_str().unwrap().contains("tokens"));

    // roots for ghcp: the peer is declared, unprobeable, and says why
    let roots = structured(&call(&gw, "roots", json!({})).await);
    let host = roots["fleet"]["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "beta")
        .unwrap()
        .clone();
    assert_eq!(host["verified"], false);
    assert_eq!(host["probe"]["status"], "skipped");
    assert!(host["probe"]["detail"].as_str().unwrap().contains("ghcp"));

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
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
/// **declares** the fleet (PD-5) but holds no peer tokens for it, because it
/// is a leaf and not a gateway.
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
        vec![identity("claude", "tok-beta-claude")],
        BTreeMap::new(),
        // declared, routable, and NO tokens — kubsdb's shape exactly
        Some(vec![Peer {
            url: Some(format!("http://127.0.0.1:{beta_port}/mcp")),
            ..Peer::declared("alpha", PeerStatus::Active)
        }]),
    )
    .await?;

    let secrets = tempfile::tempdir()?;
    let beta_token_file = secrets.path().join("beta-claude.token");
    std::fs::write(&beta_token_file, "tok-beta-claude\n")?;
    let mut tokens = BTreeMap::new();
    tokens.insert(
        "claude".to_string(),
        PeerTokenEntry {
            token_env: None,
            token_file: Some(beta_token_file.display().to_string()),
        },
    );
    let alpha = start_instance(
        "alpha",
        vec![
            identity("claude", "tok-alpha-claude"),
            // deliberately no beta credential, so this author has a real
            // fleet gap to report — the control for the assertions below
            identity("ghcp", "tok-alpha-ghcp"),
        ],
        BTreeMap::new(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            tokens,
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    )
    .await?;

    std::fs::write(alpha.workdir_path.join("a.txt"), "needle here\n")?;
    std::fs::write(beta.workdir_path.join("b.txt"), "needle there\n")?;
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
    let stranger = connect(alpha.addr, "tok-alpha-ghcp").await?;
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
    assert_eq!(fleetwide["hosts_unavailable"][0]["status"], "no_credential");

    let _ = stranger.cancel().await;
    let _ = gw.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}

/// D-2's rotation story: a backend rotates, the gateway's stale credential
/// fails loudly with the remedy named, and a SIGHUP-equivalent reload picks
/// up the new token with no restart — the #914 promise, extended to the
/// outbound direction.
#[tokio::test]
async fn peer_token_rotation_reloads_without_restart() -> anyhow::Result<()> {
    // beta reads its accepted token from a file, so it can rotate live
    let beta_secrets = tempfile::tempdir()?;
    let accepted = beta_secrets.path().join("claude.token");
    std::fs::write(&accepted, "tok-v1\n")?;
    let mut beta_auth = BTreeMap::new();
    beta_auth.insert(
        "claude".to_string(),
        AuthEntry {
            token_env: None,
            token_file: Some(accepted.display().to_string()),
            prev_token_file: None,
        },
    );
    let beta = start_instance("beta", vec![identity("claude", "tok-v1")], beta_auth, None).await?;

    let gw_secrets = tempfile::tempdir()?;
    let held = gw_secrets.path().join("beta-claude.token");
    std::fs::write(&held, "tok-v1\n")?;
    let mut tokens = BTreeMap::new();
    tokens.insert(
        "claude".to_string(),
        PeerTokenEntry {
            token_env: None,
            token_file: Some(held.display().to_string()),
        },
    );
    let alpha = start_instance(
        "alpha",
        vec![identity("claude", "tok-alpha-claude")],
        BTreeMap::new(),
        Some(vec![Peer {
            url: Some(format!("http://{}/mcp", beta.addr)),
            tokens,
            ..Peer::declared("beta", PeerStatus::Active)
        }]),
    )
    .await?;
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

    let ok = call(&gw, "stat", json!({"root": "beta:scratch", "path": ""})).await;
    assert_ne!(ok.is_error, Some(true), "v1 works before the rotation");

    // beta rotates, hard cut — the gateway now holds a dead credential
    std::fs::write(&accepted, "tok-v2\n")?;
    beta.auth.reload();
    let rejected = call(&gw, "stat", json!({"root": "beta:scratch", "path": ""})).await;
    assert_eq!(rejected.is_error, Some(true));
    let e = structured(&rejected);
    assert_eq!(e["code"], "denied", "{e}");
    assert_eq!(e["data"]["reason"], "peer_credential_rejected");
    assert!(e["data"]["hint"].as_str().unwrap().contains("SIGHUP"));

    // the gateway's token file catches up; reload — no restart anywhere
    std::fs::write(&held, "tok-v2\n")?;
    alpha.auth.reload();
    let again = call(&gw, "stat", json!({"root": "beta:scratch", "path": ""})).await;
    assert_ne!(
        again.is_error,
        Some(true),
        "rotated peer credential works after reload: {:?}",
        again.structured_content
    );

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
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
    let direct = connect(beta.addr, "tok-beta-claude").await?;
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
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
    let direct = connect(beta.addr, "tok-beta-claude").await?;
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
    let gw = connect(alpha.addr, "tok-alpha-claude").await?;

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
    let direct = connect(beta.addr, "tok-beta-claude").await?;
    let beta_journal = structured(&call(&direct, "journal", json!({})).await);
    assert_eq!(beta_journal["entries"][0]["author"], "claude");

    let _ = gw.cancel().await;
    let _ = direct.cancel().await;
    alpha.ct.cancel();
    beta.ct.cancel();
    Ok(())
}
