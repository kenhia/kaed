//! The R4 error model: every failure is a structured `{code, message, data}`.
//!
//! `data` makes the error actionable — `version_conflict` carries the delta
//! of what changed since the agent last looked; `ambiguous_anchor` carries
//! the candidate line numbers. Keep this module free of transport concerns;
//! the MCP layer serializes `KaedError` into an `isError` tool result.

use serde::Serialize;
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    OutsideRoot,
    Denied,
    VersionConflict,
    AmbiguousAnchor,
    AnchorNotFound,
    InvalidInput,
    TooLarge,
    IsBinary,
    ParseUnavailable,
    Internal,
    /// A call routed to a root whose host lacks that capability — reserved
    /// by the contract since 007, live since peer mode (010). The fleet
    /// advertises the *union* of capabilities, so a mid-upgrade fleet can
    /// route a call to a host that does not know the tool yet; this code is
    /// that honesty at call time.
    UnsupportedCapability,
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // serde's snake_case rename is the single source of the wire name
        let s = serde_json::to_value(self).expect("ErrorCode serializes");
        f.write_str(s.as_str().expect("ErrorCode is a string"))
    }
}

/// Which policy layer refused a path (D-5). One wire code (`denied`) for
/// all of them — agents already know `denied` is permanent and not worth
/// retrying — with the layer, and its remedy, in `data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    /// `[security] deny` or a built-in. Changing it is a human's call.
    ServerDenylist,
    /// A `.kaedignore` in the tree; the data names the file.
    Kaedignore,
    /// The file opts out via a `kaedignore` comment in its first lines.
    InFileMarker,
    /// Classified secret-bearing, and not dotenv-shaped, so kaed has no
    /// redacted surface to serve for it.
    ClassifiedOpaque,
    /// `.kaedignore` files are policy and cannot be written through kaed.
    KaedignoreProtected,
    /// `[secrets] allow_reveal = false`: this host refuses `secret_reveal`
    /// outright (011 D-1). The lifecycle verbs still work.
    RevealDisabled,
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // serde's snake_case rename is the single source of the wire name
        let s = serde_json::to_value(self).expect("RefusalReason serializes");
        f.write_str(s.as_str().expect("RefusalReason is a string"))
    }
}

#[derive(Debug, Serialize)]
pub struct KaedError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl fmt::Display for KaedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for KaedError {}

pub type Result<T> = std::result::Result<T, KaedError>;

impl KaedError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: impl Serialize) -> Self {
        self.data = Some(serde_json::to_value(data).expect("error data serializes"));
        self
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn outside_root(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::OutsideRoot, message)
    }

    /// A refusal. Distinct from `outside_root` because the remedy differs:
    /// no path correction makes this one work. Every refusal carries a
    /// structured `reason` naming which policy layer refused, and a `hint`
    /// saying what to do instead — a refusal with no alternative is how an
    /// agent ends up routing around kaed via ssh, and then the journal
    /// loses the edit too (D-5).
    pub fn refused(path: &str, rule: &str, reason: RefusalReason, hint: impl Into<String>) -> Self {
        let hint = hint.into();
        Self::new(
            ErrorCode::Denied,
            format!("{path}: refused ({reason}, rule: {rule}) — {hint}"),
        )
        .with_data(serde_json::json!({
            "path": path,
            "rule": rule,
            "reason": reason,
            "hint": hint,
        }))
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::TooLarge, message)
    }

    pub fn is_binary(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::IsBinary, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn unsupported_capability(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UnsupportedCapability, message)
    }

    pub fn version_conflict(data: VersionConflictData) -> Self {
        Self::new(
            ErrorCode::VersionConflict,
            format!(
                "{} changed since it was read (expected {}, found {})",
                data.path, data.expected_version, data.actual_version
            ),
        )
        .with_data(data)
    }

    pub fn ambiguous_anchor(data: AmbiguousAnchorData) -> Self {
        Self::new(
            ErrorCode::AmbiguousAnchor,
            format!(
                "anchor matches {} locations in {} (lines {:?}); pass `occurrence` to pick one",
                data.occurrences.len(),
                data.path,
                data.occurrences
            ),
        )
        .with_data(data)
    }

    pub fn anchor_not_found(path: impl Into<String>) -> Self {
        let path = path.into();
        Self::new(
            ErrorCode::AnchorNotFound,
            format!("anchor text not found in {path}"),
        )
        .with_data(serde_json::json!({ "path": path }))
    }
}

/// What a `feedback` prompt says when it rides an error (D-5, #1046).
const FEEDBACK_ASK: &str = "Was this refusal actionable? If kaed just cost you a detour — or you are about to \
     route around it — say so in one sentence: feedback(summary: \"…\"). It lands in this \
     host's journal next to the failure above, and it is the only way this contract \
     learns.";

impl KaedError {
    /// Whether this failure is plausibly *kaed's* fault, and so worth
    /// inviting a friction report on (D-5).
    ///
    /// A tool an agent must remember to call gets called by the agents
    /// that were already having a good time; the report worth having is
    /// from the session that hit a wall and gave up. So the invitation
    /// rides the wall.
    ///
    /// Narrow on purpose. `invalid_input` is excluded — high-volume,
    /// schema-shaped, and its message already names the valid fields — as
    /// are `ambiguous_anchor` and `anchor_not_found`, whose `data` already
    /// carries the fix. A `version_conflict` qualifies only when kaed had
    /// no delta to give: a conflict *with* one is the contract working,
    /// one without is the retention window failing an agent.
    fn invites_feedback(&self) -> bool {
        match self.code {
            ErrorCode::Denied | ErrorCode::TooLarge | ErrorCode::Internal => true,
            ErrorCode::VersionConflict => self
                .data
                .as_ref()
                .and_then(|d| d.get("delta"))
                .and_then(Value::as_str)
                .is_none_or(|delta| delta.starts_with("(content for the expected version")),
            _ => false,
        }
    }

    /// Attach the friction invitation, if this error warrants one. Applied
    /// once at the MCP boundary so every tool gets it and none has to
    /// remember to — the same reasoning as redaction living at the store
    /// boundary rather than per tool.
    pub fn with_feedback_invite(mut self) -> Self {
        if !self.invites_feedback() {
            return self;
        }
        let invite = serde_json::json!({
            "ask": FEEDBACK_ASK,
            "tool": "feedback",
            "required": ["summary"],
        });
        match &mut self.data {
            Some(Value::Object(obj)) => {
                obj.insert("feedback_invite".into(), invite);
            }
            None => self.data = Some(serde_json::json!({ "feedback_invite": invite })),
            // `data` is a non-object (nothing produces one today); leave it
            // rather than silently replacing an error's payload.
            Some(_) => {}
        }
        self
    }
}

/// `data` payload for `version_conflict`: enough for the agent to re-anchor
/// and retry without re-reading the file.
#[derive(Debug, Serialize)]
pub struct VersionConflictData {
    pub path: String,
    pub expected_version: String,
    pub actual_version: String,
    /// Unified diff of expected→actual: what changed since the agent looked.
    pub delta: String,
}

/// `data` payload for `ambiguous_anchor`: 1-based lines where the anchor matched.
#[derive(Debug, Serialize)]
pub struct AmbiguousAnchorData {
    pub path: String,
    pub occurrences: Vec<usize>,
}

impl From<std::io::Error> for KaedError {
    fn from(e: std::io::Error) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::not_found(e.to_string()),
            _ => Self::internal(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_serialize_snake_case() {
        for (code, wire) in [
            (ErrorCode::NotFound, "not_found"),
            (ErrorCode::OutsideRoot, "outside_root"),
            (ErrorCode::Denied, "denied"),
            (ErrorCode::VersionConflict, "version_conflict"),
            (ErrorCode::AmbiguousAnchor, "ambiguous_anchor"),
            (ErrorCode::AnchorNotFound, "anchor_not_found"),
            (ErrorCode::InvalidInput, "invalid_input"),
            (ErrorCode::TooLarge, "too_large"),
            (ErrorCode::IsBinary, "is_binary"),
            (ErrorCode::ParseUnavailable, "parse_unavailable"),
            (ErrorCode::Internal, "internal"),
            (ErrorCode::UnsupportedCapability, "unsupported_capability"),
        ] {
            assert_eq!(code.to_string(), wire);
        }
    }

    #[test]
    fn version_conflict_carries_actionable_data() {
        let err = KaedError::version_conflict(VersionConflictData {
            path: "src/txn.rs".into(),
            expected_version: "9f3ac2d41b7e5860".into(),
            actual_version: "4c11d8aa02e9b371".into(),
            delta: "@@ -38,4 +38,9 @@".into(),
        });
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["code"], "version_conflict");
        assert_eq!(v["data"]["expected_version"], "9f3ac2d41b7e5860");
        assert_eq!(v["data"]["delta"], "@@ -38,4 +38,9 @@");
    }

    #[test]
    fn ambiguous_anchor_lists_candidates() {
        let err = KaedError::ambiguous_anchor(AmbiguousAnchorData {
            path: "a.rs".into(),
            occurrences: vec![3, 41, 97],
        });
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["data"]["occurrences"], serde_json::json!([3, 41, 97]));
        assert!(err.message.contains("occurrence"));
    }

    #[test]
    fn io_not_found_maps_to_not_found() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        assert_eq!(KaedError::from(io).code, ErrorCode::NotFound);
    }

    // ------------------------------------------- the friction prompt (D-5)

    fn invited(e: KaedError) -> bool {
        let v = serde_json::to_value(e.with_feedback_invite()).unwrap();
        v["data"]["feedback_invite"].is_object()
    }

    #[test]
    fn a_refusal_invites_a_friction_report_without_losing_its_own_data() {
        let e = KaedError::refused(
            ".ssh/id_ed25519",
            "**/.ssh/**",
            RefusalReason::ServerDenylist,
            "a human must change the config",
        )
        .with_feedback_invite();
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["data"]["reason"], "server_denylist");
        assert!(v["data"]["hint"].is_string(), "the hint survives");
        assert!(v["data"]["feedback_invite"]["ask"].is_string());
    }

    #[test]
    fn an_error_with_no_data_still_gets_the_invitation() {
        assert!(invited(KaedError::too_large("64 MB exceeds the budget")));
        assert!(invited(KaedError::internal("the disk went away")));
    }

    /// The narrowness is the design: prompting on everything is noise, and
    /// noise is how a feedback channel dies.
    #[test]
    fn malformed_input_and_self_explaining_errors_are_not_prompted() {
        assert!(!invited(KaedError::invalid_input(
            "unknown field `old_string`"
        )));
        assert!(!invited(KaedError::anchor_not_found("src/txn.rs")));
        assert!(!invited(KaedError::ambiguous_anchor(AmbiguousAnchorData {
            path: "a.rs".into(),
            occurrences: vec![3, 41],
        })));
        assert!(!invited(KaedError::not_found("no such file")));
    }

    /// A conflict *with* a usable delta is the contract working as
    /// designed; one where the blob had expired is the retention window
    /// failing an agent, and that is worth hearing about (#1046).
    #[test]
    fn a_version_conflict_is_prompted_only_when_the_delta_was_withheld() {
        let helpful = KaedError::version_conflict(VersionConflictData {
            path: "f.txt".into(),
            expected_version: "a".into(),
            actual_version: "b".into(),
            delta: "@@ -1 +1 @@\n-first\n+second\n".into(),
        });
        assert!(!invited(helpful));

        let useless = KaedError::version_conflict(VersionConflictData {
            path: "f.txt".into(),
            expected_version: "a".into(),
            actual_version: "b".into(),
            delta: "(content for the expected version is not retained; re-read the file)".into(),
        });
        assert!(invited(useless));
    }
}
