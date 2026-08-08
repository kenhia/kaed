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
            // default classification on, as Config::resolve would build it
            ..ResolvedRoot::with_default_classify(ROOT, workdir_path.clone())
        }],
        // A fleet with one of each declared status, so the three states
        // korg #930 turns on are exercised end to end and not just in unit
        // tests of the config parser.
        peers: Some(vec![
            // No `url` on the active peer: these tests exercise the three
            // *declared* states; routing to a live peer is tests/gateway.rs.
            Peer {
                reference: None,
                note: None,
                ..Peer::declared("peer-active", PeerStatus::Active)
            },
            Peer {
                reference: Some("korg:929".into()),
                note: Some("broad-access design not settled".into()),
                ..Peer::declared("peer-deferred", PeerStatus::Deferred)
            },
            Peer {
                since: Some("2026-08-07".into()),
                ..Peer::declared("peer-down", PeerStatus::Unreachable)
            },
        ]),
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
async fn lists_the_tool_surface() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;
    let tools = client.list_all_tools().await?;
    let mut names: Vec<_> = tools.iter().map(|t| t.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "diff",
            "edit",
            "feedback",
            "journal",
            "list",
            "read",
            "revert",
            "roots",
            "search",
            "secret",
            "secret_reveal",
            "stat"
        ]
    );
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

/// Sprint 008's loop over the wire: a classified dotenv file reads
/// redacted with the typed view; a placeholder from that read is a sealed
/// handle an env op can pass back as a value; and the vanish guard's
/// refusal arrives as a structured error the client can act on — all in
/// the JSON shapes a real MCP client sends and receives.
#[tokio::test]
async fn secrets_loop_redacted_read_env_edit_and_drop_keys() -> anyhow::Result<()> {
    let server = start_server().await?;
    const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    std::fs::write(
        server.workdir_path.join(".env"),
        format!("KLAMS_TOKEN={VALUE}\nDEBUG=true\n"),
    )?;
    let client = connect(&server).await?;

    // 1. read — redacted rendering, typed view, RAW version
    let read = client
        .call_tool(
            CallToolRequestParams::new("read")
                .with_arguments(args(json!({"root": ROOT, "path": ".env"}))),
        )
        .await?;
    let r = structured(&read);
    assert_eq!(r["redacted"], true);
    let content = r["content"].as_str().unwrap();
    assert!(!content.contains(VALUE), "redacted read leaked: {content}");
    assert!(content.contains("⟨kaed:KLAMS_TOKEN@"), "{content}");
    assert_eq!(r["dotenv"][0]["key"], "KLAMS_TOKEN");
    assert_eq!(r["dotenv"][0]["meta"]["shape"], "hex");
    assert_eq!(r["dotenv"][0]["meta"]["len"], 32);
    let placeholder = r["dotenv"][0]["placeholder"].as_str().unwrap().to_string();
    let version = r["version"].as_str().unwrap().to_string();

    // 2. env_set passing the placeholder through: the real value lands on
    // disk, the returned diff shows only placeholders
    let edit = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": ".env", "version": version}],
                "ops": [{"op": "env_set", "path": ".env",
                         "key": "KLAMS_TOKEN_COPY", "value": placeholder}],
                "intent": "copy the token by its sealed handle"
            }))),
        )
        .await?;
    assert_ne!(edit.is_error, Some(true));
    let e = structured(&edit);
    assert_eq!(e["applied"], true);
    let diff = e["diff"].as_str().unwrap();
    assert!(!diff.contains(VALUE), "diff leaked the value: {diff}");
    assert!(diff.contains("⟨kaed:KLAMS_TOKEN_COPY@"), "{diff}");
    assert!(
        std::fs::read_to_string(server.workdir_path.join(".env"))?
            .contains(&format!("KLAMS_TOKEN_COPY={VALUE}")),
        "the real value must land on disk"
    );
    let v2 = e["files"][0]["new_version"].as_str().unwrap().to_string();

    // 3. destroying a value undeclared refuses, structured; declared, lands
    let refused = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": ".env", "version": v2}],
                "ops": [{"op": "env_delete", "path": ".env", "key": "DEBUG"}]
            }))),
        )
        .await?;
    assert_eq!(refused.is_error, Some(true));
    let err = structured(&refused);
    assert_eq!(err["code"], "invalid_input");
    assert_eq!(err["data"]["reason"], "secret_would_vanish");
    assert_eq!(err["data"]["keys"], json!(["DEBUG"]));

    let confirmed = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": ".env", "version": v2}],
                "ops": [{"op": "env_delete", "path": ".env", "key": "DEBUG"}],
                "drop_keys": ["DEBUG"]
            }))),
        )
        .await?;
    assert_ne!(confirmed.is_error, Some(true));
    assert!(!std::fs::read_to_string(server.workdir_path.join(".env"))?.contains("DEBUG"));

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

/// Sprint 012 end to end: a redacted `read` of `.env` teaches the host the
/// secret's digest, and the write that would paste that value into a
/// README refuses with the named override — the incident that actually
/// happens, caught at the choke point. Provider prefixes refuse with no
/// index at all, and the audit stream counts every event.
#[tokio::test]
async fn write_side_leak_detection_catches_the_readme_incident() -> anyhow::Result<()> {
    let server = start_server().await?;
    const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    std::fs::write(
        server.workdir_path.join(".env"),
        format!("KLAMS_TOKEN={VALUE}\n"),
    )?;
    let client = connect(&server).await?;

    // 1. a redacted read — this is where kaed first *sees* the value
    let read = client
        .call_tool(
            CallToolRequestParams::new("read")
                .with_arguments(args(json!({"root": ROOT, "path": ".env"}))),
        )
        .await?;
    assert_eq!(structured(&read)["redacted"], true);

    // 2. pasting the value into a README refuses, names the variable and
    // the exact override — and never echoes the value
    let refused = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "ops": [{"op": "create", "path": "README.md",
                         "content": format!("# setup\nexport KLAMS_TOKEN={VALUE}\n")}],
                "intent": "document the setup"
            }))),
        )
        .await?;
    assert_eq!(refused.is_error, Some(true));
    let err = structured(&refused);
    assert_eq!(err["code"], "invalid_input");
    assert_eq!(err["data"]["reason"], "secret_leak");
    assert_eq!(err["data"]["matches"][0]["kind"], "known_digest");
    assert_eq!(err["data"]["matches"][0]["source"]["key"], "KLAMS_TOKEN");
    assert_eq!(err["data"]["matches"][0]["source"]["path"], ".env");
    let msg = err["message"].as_str().unwrap();
    assert!(!msg.contains(VALUE), "the refusal echoed the secret: {msg}");
    let overrides = err["data"]["allow_secrets"].clone();
    assert!(!server.workdir_path.join("README.md").exists());

    // 3. the named override is the retry path; the write lands with a
    // warning instead of silently
    let allowed = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "ops": [{"op": "create", "path": "README.md",
                         "content": format!("# setup\nexport KLAMS_TOKEN={VALUE}\n")}],
                "allow_secrets": overrides,
                "intent": "deliberate: scratch root, test data"
            }))),
        )
        .await?;
    assert_ne!(allowed.is_error, Some(true));
    let a = structured(&allowed);
    assert_eq!(a["applied"], true);
    assert!(
        a["warnings"][0].as_str().unwrap().contains("allow_secrets"),
        "{a}"
    );

    // 4. a provider token needs no index: refused on shape alone
    let provider = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "ops": [{"op": "create", "path": "notes.md",
                         "content": "key: sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1d\n"}]
            }))),
        )
        .await?;
    assert_eq!(provider.is_error, Some(true));
    let p = structured(&provider);
    assert_eq!(p["data"]["reason"], "secret_leak");
    assert_eq!(p["data"]["allow_secrets"], json!(["sk-ant-"]));

    // 5. every event is countable in the audit stream
    let journal = client
        .call_tool(
            CallToolRequestParams::new("journal")
                .with_arguments(args(json!({"root": ROOT, "kind": ["secret"], "max": 20}))),
        )
        .await?;
    let entries = structured(&journal)["entries"].clone();
    let actions: Vec<&str> = entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert!(
        actions.contains(&"leak_refused") && actions.contains(&"leak_allowed"),
        "{actions:?}"
    );

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

/// Sprint 011's lifecycle over the wire: `secret` mints and rotates
/// without ever disclosing a value, `describe` hands out the durable
/// handle, `value_from` consumes one, `secret_reveal` is its own gated
/// tool, and the whole trail reads back as journal kind "secret".
#[tokio::test]
async fn secret_lifecycle_generate_rotate_handle_and_audit() -> anyhow::Result<()> {
    let server = start_server().await?;
    const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    std::fs::write(
        server.workdir_path.join(".env"),
        format!("KLAMS_TOKEN={VALUE}\n"),
    )?;
    let client = connect(&server).await?;
    let version = |path: &str| -> anyhow::Result<String> {
        Ok(kaed::fsops::version_of(
            std::fs::read_to_string(server.workdir_path.join(path))?.as_bytes(),
        ))
    };

    // 1. describe = load_secret: the handle, never the value
    let described = structured(
        &client
            .call_tool(
                CallToolRequestParams::new("secret").with_arguments(args(json!({
                    "action": "describe", "root": ROOT, "path": ".env", "key": "KLAMS_TOKEN"
                }))),
            )
            .await?,
    );
    assert_eq!(described["shape"], "hex");
    assert_eq!(described["handle"]["root"], ROOT);
    let digest = described["digest"].as_str().unwrap().to_string();
    assert_eq!(
        described["handle_line"].as_str().unwrap(),
        format!("{ROOT}/.env#KLAMS_TOKEN@{digest}")
    );
    assert!(
        !serde_json::to_string(&described)?.contains(VALUE),
        "describe leaked the value"
    );

    // 2. generate: minted server-side; the response carries a placeholder
    let generated = client
        .call_tool(
            CallToolRequestParams::new("secret").with_arguments(args(json!({
                "action": "generate", "root": ROOT, "path": ".env", "key": "MINTED",
                "version": version(".env")?, "shape": "hex(64)",
                "intent": "wire test mint"
            }))),
        )
        .await?;
    assert_ne!(generated.is_error, Some(true), "{generated:?}");
    let g = structured(&generated);
    let disk = std::fs::read_to_string(server.workdir_path.join(".env"))?;
    let minted = disk
        .lines()
        .find_map(|l| l.strip_prefix("MINTED="))
        .expect("minted key on disk")
        .to_string();
    assert_eq!(minted.len(), 64);
    assert!(
        !serde_json::to_string(&g)?.contains(&minted),
        "generate leaked the value"
    );

    // 3. an edit consumes the handle: create a service env in the same txn
    // and copy the token into it by value_from — never holding plaintext
    let copied = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [],
                "ops": [
                    {"op": "create", "path": "svc/.env", "content": ""},
                    {"op": "env_set", "path": "svc/.env", "key": "CLIENT_TOKEN",
                     "value_from": {"path": ".env", "key": "KLAMS_TOKEN", "digest": digest}}
                ],
                "intent": "hand the token to the service by reference"
            }))),
        )
        .await?;
    assert_ne!(copied.is_error, Some(true), "{copied:?}");
    assert!(
        std::fs::read_to_string(server.workdir_path.join("svc/.env"))?
            .contains(&format!("CLIENT_TOKEN={VALUE}")),
        "value_from must write the real value"
    );

    // 4. rotate: same shape, new entropy, occurrences fed the also-target
    let occurrences = structured(
        &client
            .call_tool(
                CallToolRequestParams::new("secret").with_arguments(args(json!({
                    "action": "occurrences", "root": ROOT, "path": ".env", "key": "KLAMS_TOKEN"
                }))),
            )
            .await?,
    );
    assert_eq!(occurrences["occurrences"].as_array().unwrap().len(), 2);
    let rotated = client
        .call_tool(
            CallToolRequestParams::new("secret").with_arguments(args(json!({
                "action": "rotate", "root": ROOT, "path": ".env", "key": "KLAMS_TOKEN",
                "version": version(".env")?,
                "also": [{"path": "svc/.env", "key": "CLIENT_TOKEN",
                          "version": version("svc/.env")?}]
            }))),
        )
        .await?;
    assert_ne!(rotated.is_error, Some(true), "{rotated:?}");
    let r = structured(&rotated);
    assert_eq!(r["old_digest"].as_str().unwrap(), digest);
    let live = std::fs::read_to_string(server.workdir_path.join(".env"))?;
    let new_value = live
        .lines()
        .find_map(|l| l.strip_prefix("KLAMS_TOKEN="))
        .unwrap();
    assert_ne!(new_value, VALUE);
    assert!(
        std::fs::read_to_string(server.workdir_path.join("svc/.env"))?
            .contains(&format!("CLIENT_TOKEN={new_value}")),
        "the also-target got the same new value"
    );

    // 5. reveal: intent required, then disclosed and journaled
    let refused = client
        .call_tool(
            CallToolRequestParams::new("secret_reveal").with_arguments(args(json!({
                "root": ROOT, "path": ".env", "key": "KLAMS_TOKEN", "intent": ""
            }))),
        )
        .await?;
    assert_eq!(refused.is_error, Some(true));
    let revealed = structured(
        &client
            .call_tool(
                CallToolRequestParams::new("secret_reveal").with_arguments(args(json!({
                    "root": ROOT, "path": ".env", "key": "KLAMS_TOKEN",
                    "intent": "carry to a host kaed does not serve"
                }))),
            )
            .await?,
    );
    assert_eq!(revealed["value"], new_value);
    assert_eq!(revealed["disclosed"], true);

    // 6. the audit stream: journal kind "secret", newest first
    let audit = structured(
        &client
            .call_tool(
                CallToolRequestParams::new("journal")
                    .with_arguments(args(json!({"kind": ["secret"]}))),
            )
            .await?,
    );
    let actions: Vec<&str> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    assert_eq!(actions, ["reveal", "rotate", "rotate", "generate"]);
    assert_eq!(audit["entries"][0]["disclosed"], true);
    assert!(audit["coverage"]["secrets_from"].is_string());

    // 7. the example sync rides the edit envelope
    let synced = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": ".env", "version": version(".env")?}],
                "ops": [{"op": "env_sync_example", "path": ".env"}]
            }))),
        )
        .await?;
    assert_ne!(synced.is_error, Some(true), "{synced:?}");
    let example = std::fs::read_to_string(server.workdir_path.join(".env.example"))?;
    assert!(example.contains("KLAMS_TOKEN=\n"), "{example}");
    assert!(!example.contains(new_value), "{example}");

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
    // declared with no url: nothing to probe, so never reported as observed
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

    // declared active but with no `url` here: routing was never configured
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

/// The loop sprint 009 exists for: edit, then read your own history back
/// through the contract instead of over ssh, then undo it.
#[tokio::test]
async fn history_loop_journal_diff_revert() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;
    let original = std::fs::read_to_string(server.workdir_path.join("hello.txt"))?;

    let edit = client
        .call_tool(
            CallToolRequestParams::new("edit").with_arguments(args(json!({
                "root": ROOT,
                "base": [{"path": "hello.txt", "version": fsops::version_of(original.as_bytes())}],
                "ops": [{"op": "anchor_replace", "path": "hello.txt",
                         "old_text": "old_name", "new_text": "new_name"}],
                "intent": "rename for clarity"
            }))),
        )
        .await?;
    let e = structured(&edit);
    let txn_id = e["txn_id"].as_i64().expect("a txn id");

    // journal: the transaction is there, attributed, with its intent
    let hist = client
        .call_tool(CallToolRequestParams::new("journal").with_arguments(args(json!({}))))
        .await?;
    let h = structured(&hist);
    let entry = &h["entries"][0];
    assert_eq!(entry["kind"], "txn");
    assert_eq!(entry["txn_id"], txn_id);
    assert_eq!(entry["author"], "claude");
    assert_eq!(entry["status"], "applied");
    assert_eq!(entry["intent"], "rename for clarity");
    assert_eq!(entry["root"], ROOT);
    assert!(
        entry["historical"].is_null(),
        "a live root is not historical"
    );
    // the gap is stated in the response, not left to be discovered (D-2)
    assert!(
        h["coverage"]["notes"][0]
            .as_str()
            .unwrap()
            .contains("reads are not journaled"),
        "{:?}",
        h["coverage"]
    );

    // diff: what that transaction left, against what is there now
    let diff = client
        .call_tool(
            CallToolRequestParams::new("diff").with_arguments(args(json!({
                "root": ROOT, "path": "hello.txt",
                "from": e["files"][0]["old_version"], "to": "current"
            }))),
        )
        .await?;
    let d = structured(&diff);
    assert!(d["diff"].as_str().unwrap().contains("-fn old_name()"));
    assert!(d["diff"].as_str().unwrap().contains("+fn new_name()"));
    assert_eq!(d["from_source"], "journal_blob");
    assert_eq!(d["to_source"], "working_tree");

    // revert: a new transaction, not a rewrite
    let rev = client
        .call_tool(
            CallToolRequestParams::new("revert")
                .with_arguments(args(json!({"root": ROOT, "txn_id": txn_id}))),
        )
        .await?;
    let r = structured(&rev);
    assert_eq!(r["applied"], true);
    assert_ne!(r["txn_id"], json!(txn_id));
    assert_eq!(
        std::fs::read_to_string(server.workdir_path.join("hello.txt"))?,
        original
    );

    // …and the revert is itself in the history, naming what it undid
    let hist = client
        .call_tool(CallToolRequestParams::new("journal").with_arguments(args(json!({"max": 1}))))
        .await?;
    assert!(
        structured(&hist)["entries"][0]["intent"]
            .as_str()
            .unwrap()
            .contains(&format!("revert of txn {txn_id}")),
    );

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}

/// #1046's re-shaping: the invitation rides the failure, and the report
/// lands in the same store as the events it is about.
#[tokio::test]
async fn a_refusal_invites_feedback_and_the_report_lands_in_the_journal() -> anyhow::Result<()> {
    let server = start_server().await?;
    let client = connect(&server).await?;
    std::fs::write(
        server.workdir_path.join("secret.pem"),
        "-----BEGIN PRIVATE KEY-----\nxyz\n-----END PRIVATE KEY-----\n",
    )?;

    // a refusal an agent might route around kaed over
    let refused = client
        .call_tool(
            CallToolRequestParams::new("read")
                .with_arguments(args(json!({"root": ROOT, "path": "secret.pem"}))),
        )
        .await?;
    assert_eq!(refused.is_error, Some(true));
    let r = structured(&refused);
    assert_eq!(r["code"], "denied");
    assert_eq!(r["data"]["reason"], "classified_opaque");
    assert!(r["data"]["hint"].is_string(), "the hint still survives");
    let invite = &r["data"]["feedback_invite"];
    assert_eq!(invite["tool"], "feedback");
    assert_eq!(invite["required"], json!(["summary"]));

    // taking it up costs one call and one field
    let filed = client
        .call_tool(
            CallToolRequestParams::new("feedback").with_arguments(args(json!({
                "summary": "no way to read a pem, so I used ssh",
                "context": "read secret.pem -> denied/classified_opaque"
            }))),
        )
        .await?;
    assert_eq!(structured(&filed)["recorded"], true);

    // and it reads back beside the transactions it is about
    let hist = client
        .call_tool(
            CallToolRequestParams::new("journal")
                .with_arguments(args(json!({"kind": ["feedback"]}))),
        )
        .await?;
    let h = structured(&hist);
    assert_eq!(h["entries"][0]["kind"], "feedback");
    assert_eq!(h["entries"][0]["category"], "friction");
    assert_eq!(h["entries"][0]["author"], "claude");
    assert!(
        h["entries"][0]["summary"]
            .as_str()
            .unwrap()
            .contains("used ssh")
    );

    let _ = client.cancel().await;
    server.ct.cancel();
    Ok(())
}
