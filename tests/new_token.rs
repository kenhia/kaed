//! `deploy/new-token.sh` — minting, rotation and the grace-window gate.
//!
//! The script is the only safe route to a kaed credential (refuse-if-exists,
//! never-print), so it is tested the way `install_from_store.rs` tests the
//! installer: run the *real* script against a throwaway `KAED_CONFIG_DIR`.
//!
//! `XDG_RUNTIME_DIR` is pointed at nothing so `systemctl --user is-active`
//! fails and the script takes its "kaed is not running" branch. Without that
//! a test run on a host where kaed is live would SIGHUP the real daemon.

use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

const NEW_TOKEN_SH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/deploy/new-token.sh");

fn run(dir: &Path, args: &[&str]) -> Output {
    std::process::Command::new("sh")
        .arg(NEW_TOKEN_SH)
        .args(args)
        .env("KAED_CONFIG_DIR", dir)
        .env("XDG_RUNTIME_DIR", "/nonexistent-kaed-test")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .output()
        .expect("run new-token.sh")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Both streams together — the script's own messages go to stdout and its
/// failures to stderr, and every assertion here is about *what was said*.
fn said(o: &Output) -> String {
    format!("{}{}", stdout(o), stderr(o))
}

fn write_config(dir: &Path, auth: &str) {
    std::fs::write(dir.join("config.toml"), format!("[auth]\n{auth}")).unwrap();
}

fn token(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name))
        .unwrap()
        .trim()
        .into()
}

// --- minting --------------------------------------------------------------

#[test]
fn mint_writes_a_token_and_never_prints_it() {
    let d = TempDir::new().unwrap();
    let o = run(d.path(), &[]);
    assert!(o.status.success(), "{}", said(&o));

    let t = token(d.path(), "token");
    assert_eq!(t.len(), 48);
    assert!(
        !said(&o).contains(&t),
        "the token leaked into the script's own output"
    );
}

#[test]
fn mint_refuses_to_overwrite_a_live_credential() {
    let d = TempDir::new().unwrap();
    assert!(run(d.path(), &[]).status.success());
    let before = token(d.path(), "token");

    let o = run(d.path(), &[]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("--rotate"), "{}", stderr(&o));
    assert_eq!(token(d.path(), "token"), before, "clobbered a live token");
}

// --- the grace-window gate ------------------------------------------------

// The whole of sprint 019: eight of nine credentials were configured with no
// prev_token_file because --rotate only ever *mentioned* it, after the fact.
#[test]
fn rotate_refuses_when_the_config_declares_no_grace_window() {
    let d = TempDir::new().unwrap();
    write_config(d.path(), "claude = { token_file = \"TOKEN\" }\n");
    patch_paths(d.path());
    assert!(run(d.path(), &[]).status.success());
    let before = token(d.path(), "token");

    let o = run(d.path(), &["--rotate"]);
    assert!(!o.status.success(), "{}", said(&o));
    assert!(stderr(&o).contains("prev_token_file"), "{}", stderr(&o));
    assert_eq!(
        token(d.path(), "token"),
        before,
        "refused, but rotated anyway"
    );
}

#[test]
fn force_allows_a_deliberate_hard_cut_and_leaves_no_decoy_prev() {
    let d = TempDir::new().unwrap();
    write_config(d.path(), "claude = { token_file = \"TOKEN\" }\n");
    patch_paths(d.path());
    assert!(run(d.path(), &[]).status.success());
    let before = token(d.path(), "token");

    let o = run(d.path(), &["--rotate", "--force"]);
    assert!(o.status.success(), "{}", said(&o));
    assert_ne!(token(d.path(), "token"), before);
    // No window was opened, so there must be no token.prev sitting there
    // looking like a credential that still works. It would not.
    assert!(!d.path().join("token.prev").exists());
}

#[test]
fn rotate_opens_the_window_when_one_is_configured() {
    let d = TempDir::new().unwrap();
    write_config(
        d.path(),
        "claude = { token_file = \"TOKEN\", prev_token_file = \"TOKEN.prev\" }\n",
    );
    patch_paths(d.path());
    assert!(run(d.path(), &[]).status.success());
    let before = token(d.path(), "token");

    let o = run(d.path(), &["--rotate"]);
    assert!(o.status.success(), "{}", said(&o));
    assert_ne!(token(d.path(), "token"), before);
    assert_eq!(token(d.path(), "token.prev"), before);
    assert!(!said(&o).contains(&before), "the old token leaked");

    let o = run(d.path(), &["--close"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(!d.path().join("token.prev").exists());
}

// With no config at all there is nothing to consult — minting before writing
// config.toml is the normal first-install order, so this must not become a
// refusal.
#[test]
fn rotate_without_a_config_warns_and_proceeds() {
    let d = TempDir::new().unwrap();
    assert!(run(d.path(), &[]).status.success());
    let before = token(d.path(), "token");

    let o = run(d.path(), &["--rotate"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(stdout(&o).contains("no"), "{}", stdout(&o));
    assert_eq!(token(d.path(), "token.prev"), before);
}

// The other flavour of "cannot tell", and the one that caught a wrong
// message when the parser was first run against kai's real config: a config
// that exists but names no entry for this path is a different fact from no
// config at all, and the warning has to say the one it saw.
#[test]
fn rotate_says_which_kind_of_unknown_it_found() {
    let d = TempDir::new().unwrap();
    write_config(d.path(), "other = { token_file = \"TOKEN-other\" }\n");
    patch_paths(d.path());
    assert!(run(d.path(), &[]).status.success());

    let o = run(d.path(), &["--rotate"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(
        stdout(&o).contains("no [auth] entry"),
        "reported the wrong kind of unknown: {}",
        stdout(&o)
    );
}

// --- per-identity ---------------------------------------------------------

#[test]
fn identity_resolves_its_own_token_paths_from_the_config() {
    let d = TempDir::new().unwrap();
    write_config(
        d.path(),
        "claude = { token_file = \"TOKEN\" }\n\
         claude-kai = { token_file = \"TOKEN-claude-kai\", \
         prev_token_file = \"TOKEN-claude-kai.prev\" }\n",
    );
    patch_paths(d.path());

    let o = run(d.path(), &["--identity", "claude-kai"]);
    assert!(o.status.success(), "{}", said(&o));
    let first = token(d.path(), "token-claude-kai");
    assert_eq!(first.len(), 48);
    // the default identity's token is a different credential and untouched
    assert!(!d.path().join("token").exists());

    let o = run(d.path(), &["--identity", "claude-kai", "--rotate"]);
    assert!(o.status.success(), "{}", said(&o));
    assert_eq!(token(d.path(), "token-claude-kai.prev"), first);
    assert!(!said(&o).contains(&first), "the old token leaked");
}

#[test]
fn an_identity_the_config_does_not_declare_is_refused() {
    let d = TempDir::new().unwrap();
    write_config(d.path(), "claude = { token_file = \"TOKEN\" }\n");
    patch_paths(d.path());

    let o = run(d.path(), &["--identity", "claude-kubs0"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("claude-kubs0"), "{}", stderr(&o));
    // Minting to a path no config references would produce a credential that
    // authenticates nothing — and look exactly like one that does.
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
}

#[test]
fn an_env_token_identity_is_refused_rather_than_given_a_file() {
    let d = TempDir::new().unwrap();
    write_config(d.path(), "ci = { token_env = \"KAED_TOKEN_CI\" }\n");
    patch_paths(d.path());

    let o = run(d.path(), &["--identity", "ci"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("token_env"), "{}", stderr(&o));
}

#[test]
fn identity_rotation_is_gated_the_same_way() {
    let d = TempDir::new().unwrap();
    write_config(
        d.path(),
        "claude-kai = { token_file = \"TOKEN-claude-kai\" }\n",
    );
    patch_paths(d.path());
    assert!(
        run(d.path(), &["--identity", "claude-kai"])
            .status
            .success()
    );

    let o = run(d.path(), &["--identity", "claude-kai", "--rotate"]);
    assert!(!o.status.success(), "{}", said(&o));
    assert!(stderr(&o).contains("prev_token_file"), "{}", stderr(&o));
}

/// Rewrite the `TOKEN` placeholders in the test config to absolute paths in
/// the temp dir — real configs name absolute (or `~/`-relative) paths, and a
/// bare filename is not a shape the script should have to guess about.
fn patch_paths(dir: &Path) {
    let p = dir.join("config.toml");
    let text = std::fs::read_to_string(&p)
        .unwrap()
        .replace("\"TOKEN", &format!("\"{}/token", dir.display()));
    std::fs::write(&p, text).unwrap();
}
