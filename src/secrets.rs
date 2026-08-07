//! Secret placeholder identity and shape detection (PD-2).
//!
//! A placeholder is a sealed handle for a value the agent never sees:
//! `⟨kaed:KEY@a3f9c2d41b7e5860⟩`. The digest is `blake3(value)` truncated
//! to the same 16 hex chars as `fsops::version_of` — one hash story in the
//! codebase, and the same symmetry PD-3 leans on: a `version` is a durable
//! content address for a file, a placeholder digest is one for a value.
//!
//! There is no key and no HMAC. The brute-force objection to bare hashes of
//! low-entropy values (`POSTGRES_PASSWORD=postgres`) is answered by
//! **withholding** the digest below an entropy floor: `⟨kaed:DEBUG⟩`, not
//! `⟨kaed:DEBUG@1a2b⟩`. Withholding only costs change detection on that
//! value, never safety, so every heuristic here errs toward withholding.

use std::fmt::Write as _;

/// Digest truncation, matching `fsops::version_of`. 64 bits is ample for
/// fleet-wide non-collision among a homelab's worth of secrets.
pub const DIGEST_LEN: usize = 16;

/// Below this estimated entropy the digest is withheld. BLAKE3 is fast, so
/// a disclosed digest of a small value space is an offline enumeration —
/// the bar sits where enumeration stops being a weekend project.
pub const ENTROPY_FLOOR_BITS: f64 = 80.0;

pub const PLACEHOLDER_OPEN: &str = "\u{27E8}kaed:";
pub const PLACEHOLDER_CLOSE: char = '\u{27E9}';

pub fn digest_of(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex()[..DIGEST_LEN].to_string()
}

/// Crude but conservative: `len × log2(Σ sizes of character classes
/// present)`, zeroed when the value repeats too few distinct characters to
/// plausibly be random. Over-estimates are impossible to rule out in
/// principle, which is why the floor is 80 bits and not 50.
pub fn estimated_entropy_bits(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut distinct = std::collections::BTreeSet::new();
    let (mut lower, mut upper, mut digit, mut other) = (false, false, false, false);
    for c in value.chars() {
        distinct.insert(c);
        match c {
            'a'..='z' => lower = true,
            'A'..='Z' => upper = true,
            '0'..='9' => digit = true,
            _ => other = true,
        }
    }
    if distinct.len() < 5 {
        return 0.0;
    }
    let charset = [(lower, 26), (upper, 26), (digit, 10), (other, 33)]
        .iter()
        .filter(|(present, _)| *present)
        .map(|(_, n)| *n as f64)
        .sum::<f64>();
    value.chars().count() as f64 * charset.log2()
}

pub fn clears_floor(value: &str) -> bool {
    estimated_entropy_bits(value) >= ENTROPY_FLOOR_BITS
}

/// The placeholder for `key` holding `value`. Empty values get no
/// placeholder at all — that a key is unset is not a secret; callers
/// render `KEY=` plainly.
pub fn placeholder(key: &str, value: &str) -> String {
    let mut s = String::new();
    let _ = write!(s, "{PLACEHOLDER_OPEN}{key}");
    if clears_floor(value) {
        let _ = write!(s, "@{}", digest_of(value));
    }
    s.push(PLACEHOLDER_CLOSE);
    s
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedPlaceholder<'a> {
    pub key: &'a str,
    /// Absent for below-floor values, which never disclosed one.
    pub digest: Option<&'a str>,
}

/// Parse `s` as **exactly** one placeholder — whole-string, nothing around
/// it. Sealed-handle semantics: a placeholder is passed through verbatim or
/// not at all.
pub fn parse_placeholder(s: &str) -> Option<ParsedPlaceholder<'_>> {
    let body = s
        .strip_prefix(PLACEHOLDER_OPEN)?
        .strip_suffix(PLACEHOLDER_CLOSE)?;
    let (key, digest) = match body.split_once('@') {
        Some((k, d)) => (k, Some(d)),
        None => (body, None),
    };
    if key.is_empty() || key.contains('\u{27E8}') || key.contains('\u{27E9}') {
        return None;
    }
    if let Some(d) = digest
        && (d.len() != DIGEST_LEN || !d.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return None;
    }
    Some(ParsedPlaceholder { key, digest })
}

/// Whether `s` contains placeholder syntax anywhere. A value that merely
/// *contains* one is refused by the write path: a truncated or embedded
/// placeholder must never land in a file as a literal.
pub fn contains_placeholder_syntax(s: &str) -> bool {
    s.contains(PLACEHOLDER_OPEN)
}

/// Provider token prefixes worth flagging even when short. The generic
/// entropy rule catches the rest.
const PROVIDER_PREFIXES: &[&str] = &[
    "sk-ant-",
    "sk-",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "AKIA",
    "AIza",
];

fn provider_prefix(token: &str) -> Option<&'static str> {
    PROVIDER_PREFIXES.iter().copied().find(|p| {
        token
            .strip_prefix(p)
            .is_some_and(|rest| rest.len() >= 8 && rest.chars().all(is_token_char))
    })
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '=' | '.')
}

/// Is this whitespace-delimited token secret-shaped? Conservative: provider
/// prefixes, or ≥32 chars of token charset clearing the entropy floor.
/// Tokens containing `/` are exempt unless provider-prefixed — paths and
/// URLs live in comments and are not secrets.
pub fn looks_secret_shaped(token: &str) -> bool {
    if provider_prefix(token).is_some() {
        return true;
    }
    token.chars().count() >= 32 && token.chars().all(is_token_char) && clears_floor(token)
}

/// Replace secret-shaped tokens in free text (comment lines) with
/// placeholders. `.env` files are full of `# old token: abc123…`, so the
/// shape detector runs over comments, not just values (WI #1048).
pub fn redact_free_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find(|c: char| !c.is_whitespace()) {
        let (ws, tail) = rest.split_at(idx);
        out.push_str(ws);
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        let (token, after) = tail.split_at(end);
        if looks_secret_shaped(token) {
            if clears_floor(token) {
                out.push_str(&format!(
                    "{PLACEHOLDER_OPEN}@{}{PLACEHOLDER_CLOSE}",
                    digest_of(token)
                ));
            } else {
                out.push_str(&format!("{PLACEHOLDER_OPEN}redacted{PLACEHOLDER_CLOSE}"));
            }
        } else {
            out.push_str(token);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Coarse value shape for `meta.shape` in the redacted dotenv view. The
/// named shape *registry* (generate/rotate) is slice 3 (#1051); this is
/// only the disclosable, non-random description of what is there.
pub fn shape_of(value: &str) -> String {
    if value.is_empty() {
        return "empty".into();
    }
    if is_uuid(value) {
        return "uuid".into();
    }
    if let Some(p) = provider_prefix(value) {
        return format!("prefixed:{}", p.trim_end_matches(['-', '_']));
    }
    let n = value.chars().count();
    if n >= 16 && value.chars().all(|c| c.is_ascii_hexdigit()) {
        return "hex".into();
    }
    if n >= 16
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
    {
        return "base64".into();
    }
    "text".into()
}

fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&parts)
            .all(|(len, p)| p.len() == *len && p.chars().all(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_matches_the_version_primitive() {
        // PD-2: same hash, same truncation as fsops::version_of, so the
        // length question is settled by precedent.
        let d = digest_of("some-value");
        assert_eq!(d.len(), DIGEST_LEN);
        assert_eq!(d, blake3::hash(b"some-value").to_hex()[..16].to_string());
    }

    #[test]
    fn low_entropy_values_get_no_digest() {
        for v in ["postgres", "true", "5432", "changeme", "hunter2", ""] {
            assert!(!clears_floor(v), "{v:?} must not disclose a digest");
            let p = placeholder("K", v);
            assert!(!p.contains('@'), "{p}");
        }
    }

    #[test]
    fn real_tokens_clear_the_floor() {
        for v in [
            "sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1dFg6jVe3aUi5oNq0KxWmT",
            "b7f3a9d2c8e14f60b7f3a9d2c8e14f60",     // 32 hex
            "2f6b8a1e-9c4d-4e7a-b3f5-0d8c6a2e9b41", // uuid
        ] {
            assert!(clears_floor(v), "{v:?} should clear the floor");
            let p = placeholder("K", v);
            assert!(p.contains('@'), "{p}");
        }
    }

    #[test]
    fn repeated_characters_do_not_fake_entropy() {
        assert!(!clears_floor(&"ab".repeat(40)));
        assert!(!clears_floor(&"x".repeat(100)));
    }

    #[test]
    fn placeholder_round_trips() {
        let v = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        let p = placeholder("API_KEY", v);
        let parsed = parse_placeholder(&p).unwrap();
        assert_eq!(parsed.key, "API_KEY");
        assert_eq!(parsed.digest, Some(digest_of(v).as_str()));

        let floor = placeholder("DEBUG", "true");
        let parsed = parse_placeholder(&floor).unwrap();
        assert_eq!(parsed.key, "DEBUG");
        assert_eq!(parsed.digest, None);
    }

    #[test]
    fn parse_is_whole_string_only() {
        let p = placeholder("K", "true");
        assert!(parse_placeholder(&format!("x{p}")).is_none());
        assert!(parse_placeholder(&format!("{p}x")).is_none());
        assert!(parse_placeholder("⟨kaed:K@zz⟩").is_none(), "bad digest");
        assert!(parse_placeholder("⟨kaed:⟩").is_none(), "empty key");
        assert!(contains_placeholder_syntax("prefix ⟨kaed:K truncated"));
        assert!(!contains_placeholder_syntax("plain text"));
    }

    #[test]
    fn comment_redaction_replaces_tokens_and_nothing_else() {
        let text = "# old token: sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1dFg6jVe3aU (rotated 2026)";
        let out = redact_free_text(text);
        assert!(!out.contains("sk-ant-"), "{out}");
        assert!(out.contains("# old token:"), "{out}");
        assert!(out.contains("(rotated 2026)"), "{out}");
        assert!(out.contains(PLACEHOLDER_OPEN), "{out}");

        // paths and URLs are not secrets
        let benign = "# see https://example.com/very/long/path/to/documentation/page";
        assert_eq!(redact_free_text(benign), benign);
        let benign2 = "# ordinary words that just keep going for a while here";
        assert_eq!(redact_free_text(benign2), benign2);
    }

    #[test]
    fn shapes_describe_without_disclosing() {
        assert_eq!(shape_of(""), "empty");
        assert_eq!(shape_of("2f6b8a1e-9c4d-4e7a-b3f5-0d8c6a2e9b41"), "uuid");
        assert_eq!(shape_of("b7f3a9d2c8e14f60b7f3a9d2c8e14f60"), "hex");
        assert_eq!(
            shape_of("sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1dFg6jVe3aU"),
            "prefixed:sk-ant"
        );
        assert_eq!(shape_of("dGhpcyBpcyBub3QgYSBzZWNyZXQ="), "base64");
        assert_eq!(shape_of("postgres"), "text");
    }
}
