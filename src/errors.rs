//! The R4 error model: every failure is a structured `{code, message, data}`.
//!
//! `data` makes the error actionable — `version_conflict` carries the delta
//! of what changed since the agent last looked; `ambiguous_anchor` carries
//! the candidate line numbers. Keep this module free of transport concerns;
//! the MCP layer serializes `KaedError` into an `isError` tool result.

use serde::Serialize;
use serde_json::Value;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
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
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // serde's snake_case rename is the single source of the wire name
        let s = serde_json::to_value(self).expect("ErrorCode serializes");
        f.write_str(s.as_str().expect("ErrorCode is a string"))
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

    /// A path the deny list refuses. Distinct from `outside_root` because
    /// the remedy differs: no path correction makes this one work.
    pub fn denied(path: &str, rule: &str) -> Self {
        Self::new(
            ErrorCode::Denied,
            format!("{path}: refused by the server's deny list (rule: {rule})"),
        )
        .with_data(serde_json::json!({ "path": path, "rule": rule }))
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
}
