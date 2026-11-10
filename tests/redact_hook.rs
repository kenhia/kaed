//! The 027 hook as Claude Code runs it: a process reading the PostToolUse
//! input on stdin. The library tests cover the redaction; these pin the
//! process contract — the output shape, and failing open.

use std::io::Write;
use std::process::{Command, Stdio};

fn run(stdin: &str) -> (bool, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kaed-redact-hook"))
        .args(["--journal", "/nonexistent/journal.db"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn a_secret_in_bash_stdout_comes_back_sealed() {
    let token = "ghp_Q3x9LmA7vK2pR8sT1wY4zB6nC0dE5fGh2J";
    let input = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Bash",
        "tool_response": {"stdout": format!("GH={token}\n"), "stderr": "", "interrupted": false, "isImage": false},
    });
    let (ok, out, _) = run(&input.to_string());
    assert!(ok);
    assert!(!out.contains(token));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let r = &v["hookSpecificOutput"]["updatedToolOutput"];
    assert!(
        r["stdout"]
            .as_str()
            .unwrap()
            .starts_with("GH=\u{27E8}kaed:ghp@")
    );
    assert_eq!(r["isImage"], false);
}

#[test]
fn clean_output_prints_nothing() {
    let (ok, out, _) =
        run(r#"{"hook_event_name":"PostToolUse","tool_response":{"stdout":"hi","stderr":""}}"#);
    assert!(ok);
    assert!(out.is_empty());
}

#[test]
fn garbage_input_fails_open() {
    let (ok, out, err) = run("not json");
    assert!(ok, "a broken hook must not break the tool call");
    assert!(out.is_empty());
    assert!(err.contains("unredacted"), "{err}");
}
