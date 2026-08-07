//! Protocol integration tests: the real server over real HTTP, driven by
//! an rmcp client — the worked example from the contract, conflict path
//! included.

use kaed::config::{AuthEntry, Identity, Limits, Peer, PeerStatus, Resolved, ResolvedRoot};
use kaed::fsops;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};

const TOKEN: &str = "test-token-claude";
/// This instance's fleet name, and therefore the prefix on every root name
/// it serves. Roots have been host-qualified since sprint 007.
const HOST: &str = "testhost";
const ROOT: &str = "testhost:scratch";

struct TestServer {
    addr: std::net::SocketAddr,
    _workdir: tempfile::TempDir,
    workdir_path: std::path::PathBuf,
    ct: tokio_util::sync::CancellationToken,
    /// The reload handle SIGHUP drives in production.
    auth: std::sync::Arc<kaed::server::AuthState>,
}

async fn start_server() -> anyhow::Result<TestServer> {
    start_server_with(
        std::collections::BTreeMap::new(),
        vec![Identity {
            author: "claude".into(),
            token: TOKEN.into(),
            prev_token: None,
        }],
    )
    .await
}

async fn start_server_with(
    auth_spec: std::collections::BTreeMap<String, AuthEntry>,
    identities: Vec<Identity>,
) -> anyhow::Result<TestServer> {
    let workdir = tempfile::tempdir()?;
    let workdir_path = workdir.path().canonicalize()?;
    std::fs::write(
        workdir_path.join("hello.txt"),
        "fn old_name() {\n    body\n}\n",
    )?;
    let resolved = Resolved {
        bind: "127.0.0.1:0".parse()?,
        allowed_hosts: vec![],
        host: HOST.into(),
        roots: vec![ResolvedRoot {
            description: Some("test root".into()),
            ..ResolvedRoot::unrestricted(ROOT, workdir_path.clone())
        }],
        // A fleet with one of each declared status, so the three states
        // korg #930 turns on are exercised end to end and not just in unit
        // tests of the config parser.
        peers: Some(vec![
            Peer {
                host: "peer-active".into(),
                status: PeerStatus::Active,
                reference: None,
                note: None,
                since: None,
                url: Some("https://peer-active.example:4870/mcp".into()),
            },
            Peer {
                host: "peer-deferred".into(),
                status: PeerStatus::Deferred,
                reference: Some("korg:929".into()),
                note: Some("broad-access design not settled".into()),
                since: None,
                url: None,
            },
            Peer {
                host: "peer-down".into(),
                status: PeerStatus::Unreachable,
                reference: None,
                note: None,
                since: Some("2026-08-07".into()),
                url: None,
            },
        ]),
        identities,
        limits: Limits::default(),
        journal_path: workdir_path.join("journal.db"),
        journal_retention_days: 7,
        deny: std::sync::Arc::new(kaed::deny::DenyList::empty()),
        auth: auth_spec,
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
    Ok(TestServer {
        addr,
        _workdir: workdir,
        workdir_path,
        ct,
        auth,
    })
}

async fn connect(
    server: &TestServer,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>> {
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(format!("http://{}/mcp", server.addr))
            .auth_header(TOKEN),
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

#[tokio::test]
async fn rejects_missing_and_bad_tokens() -> anyhow::Result<()> {
    let server = start_server().await?;
    let http = reqwest::Client::new();
    let url = format!("http://{}/mcp", server.addr);

    let no_auth = http.post(&url).body("{}").send().await?;
    assert_eq!(no_auth.status(), 401);
    // RFC 6750 §3.1: no credential presented means no error code to report
    let challenge = no_auth.headers()[reqwest::header::WWW_AUTHENTICATE]
        .to_str()?
        .to_string();
    assert_eq!(challenge, "Bearer realm=\"kaed\"");

    let bad = http
        .post(&url)
        .header("authorization", "Bearer wrong-token")
        .body("{}")
        .send()
        .await?;
    assert_eq!(bad.status(), 401);
    // a wrong token *is* self-describing, in the header and the body both —
    // clients otherwise render a bare 401 as "token expired", and kaed has
    // no expiry to go looking for
    let challenge = bad.headers()[reqwest::header::WWW_AUTHENTICATE]
        .to_str()?
        .to_string();
    assert!(challenge.contains("error=\"invalid_token\""), "{challenge}");
    assert!(challenge.contains("do not expire"), "{challenge}");
    assert!(bad.text().await?.contains("do not expire"));

    server.ct.cancel();
    Ok(())
}

/// #914: rotation should not be a hard cut. Reload is the load-bearing
/// half — the process restart is what kills live sessions, independent of
/// tokens — and the grace window is what makes the reload safe for clients
/// that have not restarted yet.
#[tokio::test]
async fn rotation_reloads_in_place_with_a_grace_window() -> anyhow::Result<()> {
    let secrets = tempfile::tempdir()?;
    let current = secrets.path().join("claude.token");
    let previous = secrets.path().join("claude.token.prev");
    std::fs::write(&current, "token-v1\n")?;

    let mut spec = std::collections::BTreeMap::new();
    spec.insert(
        "claude".to_string(),
        AuthEntry {
            token_env: None,
            token_file: Some(current.display().to_string()),
            prev_token_file: Some(previous.display().to_string()),
        },
    );
    let server = start_server_with(
        spec,
        vec![Identity {
            author: "claude".into(),
            token: "token-v1".into(),
            prev_token: None,
        }],
    )
    .await?;

    let http = reqwest::Client::new();
    let url = format!("http://{}/mcp", server.addr);
    let probe = |token: &str| {
        let (http, url, token) = (http.clone(), url.clone(), token.to_string());
        async move {
            http.post(&url)
                .header("authorization", format!("Bearer {token}"))
                .body("{}")
                .send()
                .await
                .map(|r| r.status())
        }
    };
    // v1 authenticates (400-something from the MCP layer, not 401)
    assert_ne!(probe("token-v1").await?, 401);
    assert_eq!(probe("token-v2").await?, 401);

    // rotate: v2 becomes current, v1 moves to the grace slot. No restart.
    std::fs::write(&current, "token-v2\n")?;
    std::fs::write(&previous, "token-v1\n")?;
    server.auth.reload();

    assert_ne!(probe("token-v2").await?, 401, "the new token must work");
    assert_ne!(
        probe("token-v1").await?,
        401,
        "the grace window must still honour the old token"
    );

    // grace window closes: the old token dies, still with no restart
    std::fs::remove_file(&previous)?;
    server.auth.reload();
    assert_ne!(probe("token-v2").await?, 401);
    assert_eq!(probe("token-v1").await?, 401);

    server.ct.cancel();
    Ok(())
}

#[tokio::test]
async fn lists_the_walking_skeleton_tools() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;
    let tools = client.list_all_tools().await?;
    let mut names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(names, ["edit", "list", "read", "roots", "search", "stat"]);
    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

/// The contract's worked example: search → windowed read → edit, three
/// calls, no verification read — then the conflict path.
#[tokio::test]
async fn core_loop_and_conflict_path() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;

    // roots — host-qualified since sprint 007
    let roots = client
        .call_tool(CallToolRequestParams::new("roots"))
        .await?;
    assert_eq!(structured(&roots)["roots"][0]["name"], ROOT);

    // 1. search — the hit carries the version
    let search = client
        .call_tool(
            CallToolRequestParams::new("search")
                .with_arguments(args(json!({"root": ROOT, "pattern": "old_name"}))),
        )
        .await?;
    let s = structured(&search);
    assert_eq!(s["matches"][0]["path"], "hello.txt");
    assert_eq!(s["matches"][0]["line"], 1);
    let version = s["matches"][0]["version"].as_str().unwrap().to_string();

    // 2. windowed read around the anchor (optional in the loop, cheap)
    let read = client
        .call_tool(
            CallToolRequestParams::new("read").with_arguments(args(json!({
                "root": ROOT, "path": "hello.txt",
                "window": {"anchor": "old_name", "context": 1}
            }))),
        )
        .await?;
    let r = structured(&read);
    assert_eq!(r["version"], version.as_str());
    assert_eq!(r["range"]["start"], 1);

    // 3. edit against that version — diff comes back as proof
    let edit = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": "hello.txt", "version": version}],
                "ops": [{"op": "anchor_replace", "path": "hello.txt",
                         "old_text": "old_name", "new_text": "new_name"}],
                "intent": "integration test rename"
            }))),
        )
        .await?;
    assert_ne!(edit.is_error, Some(true));
    let e = structured(&edit);
    assert_eq!(e["applied"], true);
    assert!(e["txn_id"].as_i64().is_some());
    let new_version = e["files"][0]["new_version"].as_str().unwrap().to_string();
    assert!(e["diff"].as_str().unwrap().contains("+fn new_name() {"));
    // the write really landed
    assert_eq!(
        std::fs::read_to_string(server.workdir_path.join("hello.txt"))?,
        "fn new_name() {\n    body\n}\n"
    );
    assert_eq!(
        new_version,
        fsops::version_of(b"fn new_name() {\n    body\n}\n")
    );

    // 4. conflict path: same base version again — structured error with
    // the delta of what changed since the agent looked
    let stale = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": "hello.txt", "version": version}],
                "ops": [{"op": "anchor_replace", "path": "hello.txt",
                         "old_text": "new_name", "new_text": "third_name"}]
            }))),
        )
        .await?;
    assert_eq!(stale.is_error, Some(true));
    let err = structured(&stale);
    assert_eq!(err["code"], "version_conflict");
    assert_eq!(err["data"]["expected_version"], version.as_str());
    assert_eq!(err["data"]["actual_version"], new_version.as_str());
    let delta = err["data"]["delta"].as_str().unwrap();
    assert!(delta.contains("-fn old_name() {"), "delta: {delta}");
    assert!(delta.contains("+fn new_name() {"), "delta: {delta}");

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

/// korg #930: "which hosts should run kaed, and do they" must be answerable
/// from the tool surface, without reading korg history — and a host that is
/// deliberately without an instance must not look like a failed rollout.
#[tokio::test]
async fn roots_answers_which_hosts_should_run_kaed() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;

    let r = structured(
        &client
            .call_tool(CallToolRequestParams::new("roots"))
            .await?,
    );
    assert_eq!(r["host"], HOST);

    let root = &r["roots"][0];
    assert_eq!(root["name"], ROOT);
    assert_eq!(root["host"], HOST);
    assert_eq!(root["status"], "active");
    // the union, per-root — never the intersection
    assert!(
        root["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("edit"))
    );

    let fleet = &r["fleet"];
    assert_eq!(fleet["declared"], true);
    let hosts = fleet["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 4, "self plus three declared peers");

    // Only the instance answering the call reports an observed status.
    assert_eq!(hosts[0]["host"], HOST);
    assert_eq!(hosts[0]["self"], true);
    assert_eq!(hosts[0]["verified"], true);
    assert_eq!(hosts[0]["roots"][0], ROOT);
    assert!(hosts[0]["version"].as_str().is_some());

    let by_host = |name: &str| {
        hosts
            .iter()
            .find(|h| h["host"] == name)
            .expect("declared host present")
            .clone()
    };
    // The three states stay apart, and each carries its own evidence.
    let deferred = by_host("peer-deferred");
    assert_eq!(deferred["status"], "deferred");
    assert_eq!(deferred["ref"], "korg:929");
    assert_eq!(deferred["verified"], false);

    let down = by_host("peer-down");
    assert_eq!(down["status"], "unreachable");
    assert_eq!(down["since"], "2026-08-07");

    let active = by_host("peer-active");
    assert_eq!(active["status"], "active");
    // declared, not observed: this sprint proxies nothing and probes nothing
    assert_eq!(active["verified"], false);

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

/// The other half of #930: the agent that assumed rather than asked never
/// reads `roots`, it reads the error. Each wrong `root` gets the remedy that
/// belongs to it (D-2).
#[tokio::test]
async fn a_wrong_root_name_gets_the_remedy_that_belongs_to_it() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;

    let stat = |root: &str| {
        let client = &client;
        let root = root.to_string();
        async move {
            let out = client
                .call_tool(
                    CallToolRequestParams::new("stat")
                        .with_arguments(args(json!({"root": root, "path": ""}))),
                )
                .await
                .expect("tool call completes");
            assert_eq!(out.is_error, Some(true), "root {root:?} should fail");
            let v = structured(&out);
            assert_eq!(v["code"], "not_found");
            v
        }
    };

    // the pre-007 spelling: name the replacement rather than shrugging
    let unqualified = stat("scratch").await;
    assert_eq!(unqualified["data"]["reason"], "unqualified_root");
    assert_eq!(unqualified["data"]["did_you_mean"], ROOT);

    // right host, wrong root
    assert_eq!(
        stat("testhost:nope").await["data"]["reason"],
        "unknown_root"
    );

    // A deferred host is the case that got #930 filed: an agent read the
    // absence as a broken deploy and was one step from "fixing" it.
    let deferred = stat("peer-deferred:src").await;
    assert_eq!(deferred["data"]["reason"], "host_deferred");
    assert_eq!(deferred["data"]["ref"], "korg:929");
    // two machines in one payload: which is which must not be inferred
    assert_eq!(deferred["data"]["target_host"], "peer-deferred");
    assert_eq!(deferred["data"]["this_host"], HOST);
    assert!(
        deferred["message"]
            .as_str()
            .unwrap()
            .contains("not a broken deploy"),
        "{}",
        deferred["message"]
    );

    let down = stat("peer-down:src").await;
    assert_eq!(down["data"]["reason"], "host_unreachable");
    assert_eq!(down["data"]["since"], "2026-08-07");

    // declared and up, but this instance does not proxy yet
    assert_eq!(
        stat("peer-active:src").await["data"]["reason"],
        "peer_routing_unavailable"
    );

    // never declared — distinct from deferred, which is the whole point
    assert_eq!(
        stat("wat:src").await["data"]["reason"],
        "host_never_declared"
    );

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

#[tokio::test]
async fn structured_errors_reach_the_agent() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;

    // unknown root
    let bad_root = client
        .call_tool(
            CallToolRequestParams::new("stat")
                .with_arguments(args(json!({"root": "nope", "path": ""}))),
        )
        .await?;
    assert_eq!(bad_root.is_error, Some(true));
    assert_eq!(structured(&bad_root)["code"], "not_found");

    // jail escape
    let escape = client
        .call_tool(
            CallToolRequestParams::new("read")
                .with_arguments(args(json!({"root": ROOT, "path": "../etc/passwd"}))),
        )
        .await?;
    assert_eq!(escape.is_error, Some(true));
    assert_eq!(structured(&escape)["code"], "outside_root");

    // ambiguous anchor carries candidates
    std::fs::write(server.workdir_path.join("dup.txt"), "x\nx\n")?;
    let v = fsops::version_of(b"x\nx\n");
    let ambiguous = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": "dup.txt", "version": v}],
                "ops": [{"op": "anchor_replace", "path": "dup.txt",
                         "old_text": "x", "new_text": "y"}]
            }))),
        )
        .await?;
    assert_eq!(ambiguous.is_error, Some(true));
    let a = structured(&ambiguous);
    assert_eq!(a["code"], "ambiguous_anchor");
    assert_eq!(a["data"]["occurrences"], json!([1, 2]));

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

#[tokio::test]
async fn dry_run_multi_file_create() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;

    let dry = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "ops": [
                    {"op": "create", "path": "a/new.txt", "content": "one\n"},
                    {"op": "create", "path": "b/run.sh", "content": "#!/bin/sh\n",
                     "executable": true}
                ],
                "dry_run": true
            }))),
        )
        .await?;
    let d = structured(&dry);
    assert_eq!(d["applied"], false);
    assert!(!server.workdir_path.join("a/new.txt").exists());

    let real = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "ops": [
                    {"op": "create", "path": "a/new.txt", "content": "one\n"},
                    {"op": "create", "path": "b/run.sh", "content": "#!/bin/sh\n",
                     "executable": true}
                ]
            }))),
        )
        .await?;
    let r = structured(&real);
    assert_eq!(r["applied"], true);
    assert_eq!(r["files"].as_array().unwrap().len(), 2);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(server.workdir_path.join("b/run.sh"))?
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}
