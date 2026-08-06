//! `deploy/install.sh --from-store` against a real static package store.
//!
//! The store is just files behind HTTP (k-homelab `docs/deploying.md`), so a
//! faithful test is a throwaway artifact tree served out of the test process
//! and the *real* installer pointed at it. Published versions on the real
//! store are immutable, so the suite must never need to publish to it.
//!
//! Everything here runs `--dry-run`, which in store mode still fetches and
//! verifies (D6) — that is the whole of the new logic. What it would install
//! is asserted from the printed plan, so nothing touches systemd.

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const INSTALL_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/install.sh");
const DEPLOY_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/deploy");

/// `x86_64-linux` — the same suffix `just publish` names files with.
fn target_suffix() -> String {
    let out = |arg: &str| {
        let o = std::process::Command::new("uname")
            .arg(arg)
            .output()
            .unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_string()
    };
    format!("{}-{}", out("-m"), out("-s").to_lowercase())
}

// --- the store ------------------------------------------------------------

async fn serve_file(State(root): State<PathBuf>, uri: Uri) -> Response {
    let rel = uri.path().trim_start_matches('/');
    if rel.split('/').any(|c| c == ".." || c.is_empty()) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match std::fs::read(root.join(rel)) {
        Ok(bytes) => (StatusCode::OK, bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

struct Store {
    base: String,
    dir: TempDir,
}

impl Store {
    /// The `artifacts/kaed/<version>/` directory, for tests that corrupt it.
    fn version_dir(&self, version: &str) -> PathBuf {
        self.dir.path().join("artifacts/kaed").join(version)
    }
}

/// Publish `version` into a fresh store, with a stub binary reporting `stamp`.
///
/// `stamp` is what `kaed --version` prints; a real one looks like
/// `kaed 0.1.0 (b08ce9f 2026-08-02)`. The stub is a shell script because the
/// installer only ever asks it for `--version`.
async fn store_with(version: &str, stamp: &str) -> Store {
    let dir = tempfile::tempdir().unwrap();
    let vdir = dir.path().join("artifacts/kaed").join(version);
    std::fs::create_dir_all(&vdir).unwrap();

    let bin = vdir.join(format!("kaed-{}", target_suffix()));
    std::fs::write(&bin, format!("#!/bin/sh\nprintf '%s\\n' '{stamp}'\n")).unwrap();

    for f in [
        "kaed.service",
        "config.example.toml",
        "new-token.sh",
        "install.sh",
    ] {
        std::fs::copy(Path::new(DEPLOY_DIR).join(f), vdir.join(f)).unwrap();
    }
    write_sums(&vdir);

    std::fs::write(
        dir.path().join("artifacts/kaed/latest"),
        format!("{version}\n"),
    )
    .unwrap();

    let base = spawn(dir.path().to_path_buf()).await;
    Store { base, dir }
}

/// (Re)write `SHA256SUMS` over every other file in `vdir`.
fn write_sums(vdir: &Path) {
    let mut names: Vec<String> = std::fs::read_dir(vdir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "SHA256SUMS")
        .collect();
    names.sort();
    let out = std::process::Command::new("sha256sum")
        .args(&names)
        .current_dir(vdir)
        .output()
        .unwrap();
    assert!(out.status.success(), "sha256sum failed in {vdir:?}");
    std::fs::write(vdir.join("SHA256SUMS"), out.stdout).unwrap();
}

async fn spawn(root: PathBuf) -> String {
    let app = Router::new().fallback(any(serve_file)).with_state(root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

// --- driving the installer ------------------------------------------------

struct Run {
    ok: bool,
    output: String,
}

impl Run {
    #[track_caller]
    fn says(&self, needle: &str) -> &Self {
        assert!(
            self.output.contains(needle),
            "expected {needle:?} in output:\n{}",
            self.output
        );
        self
    }
}

/// Run the real installer in store mode against `store`, in a throwaway HOME.
async fn install(store: Option<&str>, args: &[&str]) -> (Run, TempDir) {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = std::process::Command::new("sh");
    cmd.arg(INSTALL_SH).args(["--from-store", "--dry-run"]);
    if let Some(base) = store {
        cmd.args(["--store", base]);
    }
    cmd.args(args);
    cmd.env("HOME", home.path());
    cmd.env_remove("KAED_STORE_URL");
    // Blocking on a worker thread would stall the store's own axum task.
    let out = tokio::task::spawn_blocking(move || cmd.output())
        .await
        .unwrap()
        .expect("could not run deploy/install.sh");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        Run {
            ok: out.status.success(),
            output,
        },
        home,
    )
}

#[track_caller]
fn assert_ok(run: &Run) {
    assert!(run.ok, "installer failed:\n{}", run.output);
}

#[track_caller]
fn assert_failed(run: &Run) {
    assert!(
        !run.ok,
        "installer succeeded but should not have:\n{}",
        run.output
    );
}

// --- the happy path -------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn resolves_latest_fetches_and_verifies() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    let (run, _home) = install(Some(&store.base), &[]).await;

    assert_ok(&run);
    run.says("0.1.0-b08ce9f")
        .says("checksum OK")
        // The label check: the binary agrees with the name it was published
        // under. A mismatch is the store's fault, not the host's.
        .says("0.1.0 (b08ce9f 2026-08-02)");
}

/// Store mode must install the *fetched* unit file and config template, not
/// whatever happens to be in this checkout's `deploy/` — a clone-less host
/// has no `deploy/` at all.
#[tokio::test(flavor = "multi_thread")]
async fn installs_the_assets_it_fetched() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    let (run, home) = install(Some(&store.base), &[]).await;

    assert_ok(&run);
    let home = home.path().display().to_string();
    run.says(&format!("{home}/.config/systemd/user/kaed.service"))
        .says(&format!("{home}/.local/bin/kaed"));
    assert!(
        !run.output.contains(&format!("'{DEPLOY_DIR}/kaed.service'")),
        "store mode installed the checkout's unit file:\n{}",
        run.output
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_version_skips_the_latest_pointer() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    // Point `latest` at something that was never published: naming a version
    // must not consult it. This is the rollback path.
    std::fs::write(
        store.dir.path().join("artifacts/kaed/latest"),
        "9.9.9-nope\n",
    )
    .unwrap();

    let (run, _home) = install(Some(&store.base), &["--version", "0.1.0-b08ce9f"]).await;
    assert_ok(&run);
    run.says("checksum OK");
}

// --- the failures that matter ---------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_binary_whose_checksum_does_not_match() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    // Corrupt the binary *after* SHA256SUMS was written over the good one.
    let bin = store
        .version_dir("0.1.0-b08ce9f")
        .join(format!("kaed-{}", target_suffix()));
    std::fs::write(
        &bin,
        "#!/bin/sh\nprintf 'kaed 0.1.0 (b08ce9f 2026-08-02)\\n'\n#tamper\n",
    )
    .unwrap();

    let (run, _home) = install(Some(&store.base), &[]).await;
    assert_failed(&run);
    run.says("checksum");
}

#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_file_absent_from_sha256sums() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    let sums = store.version_dir("0.1.0-b08ce9f").join("SHA256SUMS");
    let kept: String = std::fs::read_to_string(&sums)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("kaed.service"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&sums, kept).unwrap();

    let (run, _home) = install(Some(&store.base), &[]).await;
    assert_failed(&run);
    run.says("SHA256SUMS");
}

/// The checksum proves the transfer; this proves the *labelling*. A binary
/// published under the wrong version installs cleanly and then lies to
/// `--version` — which is the signal k-homelab's build-date floor reads.
#[tokio::test(flavor = "multi_thread")]
async fn refuses_a_binary_that_disagrees_with_its_published_version() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (1234567 2026-07-01)").await;
    let (run, _home) = install(Some(&store.base), &[]).await;

    assert_failed(&run);
    run.says("0.1.0-1234567").says("0.1.0-b08ce9f");
}

#[tokio::test(flavor = "multi_thread")]
async fn reports_an_unpublished_version_clearly() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    let (run, _home) = install(Some(&store.base), &["--version", "0.9.9-nope"]).await;

    assert_failed(&run);
    run.says("0.9.9-nope");
}

/// No default store URL (D3): a guessed hostname fails later as a confusing
/// curl error instead of naming the thing you forgot to set.
#[tokio::test(flavor = "multi_thread")]
async fn refuses_to_guess_a_store_url() {
    let (run, _home) = install(None, &[]).await;
    assert_failed(&run);
    run.says("KAED_STORE_URL");
}

#[tokio::test(flavor = "multi_thread")]
async fn store_mode_and_build_mode_are_mutually_exclusive() {
    let store = store_with("0.1.0-b08ce9f", "kaed 0.1.0 (b08ce9f 2026-08-02)").await;
    let (run, _home) = install(Some(&store.base), &["--bin", "/bin/true"]).await;
    assert_failed(&run);
}
