//! The shell-output redaction hook (027, spike): a Claude Code `PostToolUse`
//! command hook that runs `leak::redact` over a tool's result before the
//! model sees it.
//!
//! Redaction only — no recall, no reveal, no service (korg 2372, Ken's
//! answer 2026-11-09). The known-digest tier reads the host's kaed journal
//! **read-only**; on a host without one (cleo) the hook still applies the
//! index-free tiers.
//!
//! The output shape is the contract: `updatedToolOutput` must match the
//! tool's own result shape or Claude Code silently uses the original (korg
//! 2372 comment 3218). So the hook never builds a result — it walks the
//! `tool_response` it was given and rewrites strings in place.

use crate::leak::{self, DigestLocation, Tiers};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;

pub type DigestIndex = HashMap<String, DigestLocation>;

/// Load `secret_digests` from a kaed journal, read-only. The daemon owns
/// the database; this opens it without migrating or writing anything.
pub fn load_index(journal: &Path) -> anyhow::Result<DigestIndex> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(
        journal,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut stmt = conn.prepare(
        "SELECT digest, root, path, key FROM secret_digests ORDER BY first_seen, root, path, key",
    )?;
    let mut index = DigestIndex::new();
    for row in stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            DigestLocation {
                root: r.get(1)?,
                path: r.get(2)?,
                key: r.get(3)?,
            },
        ))
    })? {
        let (digest, loc) = row?;
        index.entry(digest).or_insert(loc); // earliest observation wins
    }
    Ok(index)
}

/// Redact every string inside `v`, in place. Object KEYS are left alone:
/// they are the shape, and a tool's field names are not secrets.
pub fn redact_value(v: &mut Value, tiers: Tiers, index: &DigestIndex) -> Vec<leak::Redacted> {
    let mut spans = Vec::new();
    walk(v, tiers, index, &mut spans);
    spans
}

fn walk(v: &mut Value, tiers: Tiers, index: &DigestIndex, spans: &mut Vec<leak::Redacted>) {
    match v {
        Value::String(s) => {
            let r = leak::redact(s, tiers, |ds| {
                ds.iter().map(|d| index.get(d).cloned()).collect()
            });
            if !r.spans.is_empty() {
                *s = r.text;
                spans.extend(r.spans);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|i| walk(i, tiers, index, spans)),
        Value::Object(map) => map.values_mut().for_each(|i| walk(i, tiers, index, spans)),
        _ => {}
    }
}

/// The hook's whole decision: given the `PostToolUse` input, the JSON to
/// print, or `None` to let the original through untouched (nothing found,
/// or not a PostToolUse event).
pub fn respond(input: &Value, tiers: Tiers, index: &DigestIndex) -> Option<Value> {
    if input.get("hook_event_name").and_then(Value::as_str) != Some("PostToolUse") {
        return None;
    }
    let mut out = input.get("tool_response")?.clone();
    if redact_value(&mut out, tiers, index).is_empty() {
        return None;
    }
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "updatedToolOutput": out,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GHP: &str = "ghp_Q3x9LmA7vK2pR8sT1wY4zB6nC0dE5fG";

    fn bash_input(stdout: &str) -> Value {
        json!({
            "session_id": "s",
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": "env"},
            "tool_response": {"stdout": stdout, "stderr": "", "interrupted": false, "isImage": false},
        })
    }

    #[test]
    fn a_bash_result_keeps_its_exact_shape() {
        let out = respond(
            &bash_input(&format!("GH_TOKEN={GHP}\n")),
            Tiers::PRECISE,
            &DigestIndex::new(),
        )
        .expect("redacted");
        let r = &out["hookSpecificOutput"]["updatedToolOutput"];
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["interrupted", "isImage", "stderr", "stdout"]);
        assert_eq!(r["interrupted"], json!(false));
        let stdout = r["stdout"].as_str().unwrap();
        assert!(!stdout.contains(GHP));
        assert!(stdout.starts_with("GH_TOKEN=\u{27E8}kaed:ghp@"), "{stdout}");
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    }

    #[test]
    fn clean_output_and_other_events_pass_through() {
        let idx = DigestIndex::new();
        assert!(respond(&bash_input("hello\n"), Tiers::PRECISE, &idx).is_none());
        let mut pre = bash_input(GHP);
        pre["hook_event_name"] = json!("PreToolUse");
        assert!(respond(&pre, Tiers::PRECISE, &idx).is_none());
    }

    #[test]
    fn the_index_names_the_variable() {
        let value = "Zq8XvN2mK7pL4rT9wB3yC6dF1gH5jA0s";
        let mut idx = DigestIndex::new();
        idx.insert(
            crate::secrets::digest_of(value),
            DigestLocation {
                root: "kai:src".into(),
                path: "x/.env".into(),
                key: "X_TOKEN".into(),
            },
        );
        let input = json!({"hook_event_name": "PostToolUse", "tool_response": [{"type": "text", "text": format!("t={value}")}]});
        let out = respond(&input, Tiers::PRECISE, &idx).unwrap();
        let text = out["hookSpecificOutput"]["updatedToolOutput"][0]["text"]
            .as_str()
            .unwrap();
        assert!(text.starts_with("t=\u{27E8}kaed:X_TOKEN@"), "{text}");
    }
}
