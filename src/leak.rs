//! Write-side leak detection (012): scan content a transaction is about to
//! write into an **unclassified** file for secrets that do not belong
//! there. Writing the klams token into `.env` is expected; writing it into
//! `README.md` is the incident, and this module is what notices.
//!
//! Three tiers (D-2/D-3):
//! - **known digest** — the token's BLAKE3 digest is in the
//!   `secret_digests` index (a secret kaed has seen). Precise → refuse.
//! - **provider prefix** / **private-key armor** — externally precise
//!   (`sk-ant-`, `ghp_`, `AKIA`, `-----BEGIN … PRIVATE KEY-----`) → refuse.
//! - **secret-shaped** — the generic high-entropy heuristic. It
//!   false-positives on fixtures and checksums, so it flags, never blocks.
//!
//! Only **newly-introduced** tokens trip anything (D-1): candidates come
//! from the diff's added lines, and a token already present anywhere in
//! the old content is skipped — otherwise a file that already contains a
//! token becomes uneditable through kaed, including the edit that would
//! remove it.
//!
//! Pure functions, no I/O: the digest lookup is injected, so the engine
//! wires the journal in and tests wire a map.

use crate::secrets;
use std::collections::HashSet;

/// Tokens shorter than this cannot clear the 80-bit entropy floor and are
/// shorter than any real provider token, so they are never candidates.
const MIN_TOKEN_LEN: usize = 12;

/// Host-wide strictness (D-5): the measured-rollout lever. `Refuse` is
/// the designed behavior — precise tiers refuse, the heuristic flags;
/// `Flag` downgrades everything to warnings for a host where the precise
/// tiers misfire in practice; `Off` disables scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakChecks {
    #[default]
    Refuse,
    Flag,
    Off,
}

/// Where a known digest's value lives, for the "reference the variable,
/// not the value" hint. `key` is empty for a value kaed only ever saw
/// redacted out of a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestLocation {
    pub root: String,
    pub path: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeakKind {
    KnownDigest,
    ProviderPrefix,
    PrivateKeyBlock,
    SecretShaped,
}

impl LeakKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LeakKind::KnownDigest => "known_digest",
            LeakKind::ProviderPrefix => "provider_prefix",
            LeakKind::PrivateKeyBlock => "private_key_block",
            LeakKind::SecretShaped => "secret_shaped",
        }
    }

    /// Refuse-with-override, or flag-and-apply (D-2/D-3).
    pub fn refuses(self) -> bool {
        !matches!(self, LeakKind::SecretShaped)
    }
}

/// One detected leak. Never carries the token itself — `detail` is the
/// disclosable identity of the match (a digest, a prefix, an armor label,
/// a shape description), which is also what `allow_secrets` names.
#[derive(Debug, Clone)]
pub struct LeakMatch {
    /// 1-based line in the NEW content where the match first appears.
    pub line: usize,
    pub kind: LeakKind,
    /// `known_digest` → the 16-hex digest; `provider_prefix` → the prefix;
    /// `private_key_block` → the armor label; `secret_shaped` → a short
    /// description. The refuse tiers' `detail` is the `allow_secrets`
    /// override token.
    pub detail: String,
    /// `known_digest` only: where that value lives.
    pub source: Option<DigestLocation>,
}

/// Scan `new` against `old` for newly-introduced secrets. `lookup` maps a
/// batch of candidate digests to their indexed locations (None = unknown);
/// it is only ever called with digests of above-floor tokens.
pub fn scan(
    old: Option<&str>,
    new: &str,
    lookup: impl FnOnce(&[String]) -> Vec<Option<DigestLocation>>,
) -> Vec<LeakMatch> {
    let old_tokens: HashSet<&str> = old.map(all_candidates).unwrap_or_default();
    let mut matches: Vec<LeakMatch> = Vec::new();
    // One group per newly-introduced token run; its variants (trimmed,
    // `=`-split) are alternative readings of the same text, so a group
    // reports at most one match — the strongest tier any variant hit.
    struct Group {
        line: usize,
        variants: Vec<String>,
        /// Digests of the above-floor variants, resolved in batch.
        digests: Vec<String>,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut seen_runs: HashSet<String> = HashSet::new();
    let mut seen_matches: HashSet<String> = HashSet::new();

    for (line_no, line) in added_lines(old, new) {
        if let Some(label) = private_key_armor(line) {
            if old.is_some_and(|o| {
                o.lines()
                    .any(|l| private_key_armor(l) == Some(label.clone()))
            }) {
                continue; // the block was already there
            }
            if seen_matches.insert(format!("armor:{label}")) {
                matches.push(LeakMatch {
                    line: line_no,
                    kind: LeakKind::PrivateKeyBlock,
                    detail: label,
                    source: None,
                });
            }
            continue;
        }
        for run in line
            .split(|c: char| !secrets::is_token_char(c))
            .filter(|t| t.len() >= MIN_TOKEN_LEN)
        {
            if old_tokens.contains(run) || !seen_runs.insert(run.to_owned()) {
                continue;
            }
            let variants: Vec<String> = variants_of(run)
                .into_iter()
                .filter(|v| !old_tokens.contains(v))
                .map(str::to_owned)
                .collect();
            let digests = variants
                .iter()
                .filter(|v| secrets::clears_floor(v))
                .map(|v| secrets::digest_of(v))
                .collect();
            groups.push(Group {
                line: line_no,
                variants,
                digests,
            });
        }
    }

    // Batch the digest lookup: one call per scan, above-floor variants only
    // (the index never holds a below-floor digest — PD-2).
    let all_digests: Vec<String> = groups.iter().flat_map(|g| g.digests.clone()).collect();
    let mut located = if all_digests.is_empty() {
        Vec::new()
    } else {
        lookup(&all_digests)
    }
    .into_iter();

    for group in &groups {
        // consume exactly this group's slots, whether or not one hit
        let mut known: Option<(String, DigestLocation)> = None;
        for digest in &group.digests {
            let loc = located.next().flatten();
            if known.is_none()
                && let Some(loc) = loc
            {
                known = Some((digest.clone(), loc));
            }
        }
        // strongest applicable tier wins: a known digest names the exact
        // variable to reference, a prefix at least names the provider
        let (kind, detail, source, dedupe) = if let Some((digest, loc)) = known {
            let dedupe = format!("digest:{digest}");
            (LeakKind::KnownDigest, digest, Some(loc), dedupe)
        } else if let Some(prefix) = group
            .variants
            .iter()
            .find_map(|v| secrets::provider_prefix(v))
        {
            (
                LeakKind::ProviderPrefix,
                prefix.to_owned(),
                None,
                format!("prefix:{prefix}"),
            )
        } else if let Some(v) = group.variants.iter().find(|v| shaped_for_leak(v)) {
            (
                LeakKind::SecretShaped,
                format!("{}-char high-entropy token", v.chars().count()),
                None,
                // dedupe on the content, not the description — two distinct
                // same-length tokens are two findings
                format!("shape:{}", secrets::digest_of(v)),
            )
        } else {
            continue;
        };
        if seen_matches.insert(dedupe) {
            matches.push(LeakMatch {
                line: group.line,
                kind,
                detail,
                source,
            });
        }
    }

    matches.sort_by_key(|m| m.line);
    matches
}

/// The heuristic tier, tightened for prose and code (D-3): 011's
/// `looks_secret_shaped` alone flags long snake_case identifiers and bare
/// UUIDs — exactly the noise that teaches agents to scroll past warnings.
/// Real minted material (hex, base64, provider tokens) virtually always
/// carries digits; and a UUID that IS a known secret still refuses via the
/// digest tier, so exempting the bare shape here costs nothing precise.
fn shaped_for_leak(token: &str) -> bool {
    secrets::looks_secret_shaped(token)
        && token.chars().any(|c| c.is_ascii_digit())
        && secrets::shape_of(token) != "uuid"
}

/// The 1-based new-content line numbers the diff added or changed. With no
/// old content (a create, an overwrite of a binary), every line is new.
fn added_lines<'a>(old: Option<&str>, new: &'a str) -> Vec<(usize, &'a str)> {
    let new_lines: Vec<&'a str> = new.lines().collect();
    match old {
        None => new_lines
            .iter()
            .enumerate()
            .map(|(i, l)| (i + 1, *l))
            .collect(),
        Some(old) => similar::TextDiff::from_lines(old, new)
            .iter_all_changes()
            .filter(|c| c.tag() == similar::ChangeTag::Insert)
            .filter_map(|c| c.new_index())
            .filter_map(|i| new_lines.get(i).map(|l| (i + 1, *l)))
            .collect(),
    }
}

/// Alternative readings of one token run: the run itself, the run trimmed
/// of the punctuation prose and syntax attach (`=` and `.` are token
/// chars, so `KEY=…`, `…==` padding and a sentence-final period all land
/// inside the run), and its `=`-split parts.
fn variants_of(run: &str) -> Vec<&str> {
    let mut out = vec![run];
    let trimmed = run.trim_matches(|c| matches!(c, '.' | '=' | '+'));
    if trimmed.len() >= MIN_TOKEN_LEN && !out.contains(&trimmed) {
        out.push(trimmed);
    }
    if run.contains('=') {
        for p in run.split('=') {
            if p.len() >= MIN_TOKEN_LEN && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Every candidate in a whole text, for the "was it already there" set.
fn all_candidates(text: &str) -> HashSet<&str> {
    text.lines()
        .flat_map(|line| {
            line.split(|c: char| !secrets::is_token_char(c))
                .filter(|t| t.len() >= MIN_TOKEN_LEN)
                .flat_map(variants_of)
        })
        .collect()
}

/// A PEM/OpenSSH private-key armor line, e.g.
/// `-----BEGIN OPENSSH PRIVATE KEY-----`. Returns the label between the
/// dashes. Public keys and certificates are not secrets and do not match.
fn private_key_armor(line: &str) -> Option<String> {
    let s = line.trim();
    let label = s.strip_prefix("-----BEGIN ")?.strip_suffix("-----")?;
    label
        .ends_with("PRIVATE KEY")
        .then(|| format!("BEGIN {label}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const KLAMS: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
    const ANT: &str = "sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1dFg6jVe3aU";

    fn no_known(digests: &[String]) -> Vec<Option<DigestLocation>> {
        vec![None; digests.len()]
    }

    fn known_klams() -> impl FnOnce(&[String]) -> Vec<Option<DigestLocation>> {
        let map: HashMap<String, DigestLocation> = [(
            secrets::digest_of(KLAMS),
            DigestLocation {
                root: "kai:home".into(),
                path: ".env".into(),
                key: "KLAMS_TOKEN".into(),
            },
        )]
        .into();
        move |digests: &[String]| digests.iter().map(|d| map.get(d).cloned()).collect()
    }

    #[test]
    fn a_known_digest_in_prose_refuses_and_names_the_variable() {
        let new = format!("# setup\n\nexport the token: {KLAMS}\n");
        let m = scan(None, &new, known_klams());
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].kind, LeakKind::KnownDigest);
        assert!(m[0].kind.refuses());
        assert_eq!(m[0].line, 3);
        assert_eq!(m[0].detail, secrets::digest_of(KLAMS));
        let src = m[0].source.as_ref().unwrap();
        assert_eq!(src.key, "KLAMS_TOKEN");
        assert!(
            !format!("{m:?}").contains(KLAMS),
            "the token itself must never be echoed"
        );
    }

    #[test]
    fn the_known_digest_wins_in_quotes_json_and_env_contexts() {
        for ctx in [
            format!("token = \"{KLAMS}\""),
            format!("{{\"token\": \"{KLAMS}\"}}"),
            format!("KLAMS_TOKEN={KLAMS}"),
            format!("prose then {KLAMS}."),
        ] {
            let m = scan(None, &ctx, known_klams());
            assert!(
                m.iter().any(|m| m.kind == LeakKind::KnownDigest),
                "{ctx}: {m:?}"
            );
        }
    }

    #[test]
    fn a_provider_prefix_refuses_without_any_index() {
        let new = format!("api_key = {ANT}\n");
        let m = scan(None, &new, no_known);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].kind, LeakKind::ProviderPrefix);
        assert_eq!(m[0].detail, "sk-ant-");
        assert!(m[0].kind.refuses());
    }

    #[test]
    fn a_private_key_block_refuses() {
        let new = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n";
        let m = scan(None, new, no_known);
        assert!(
            m.iter()
                .any(|m| m.kind == LeakKind::PrivateKeyBlock
                    && m.detail == "BEGIN OPENSSH PRIVATE KEY"),
            "{m:?}"
        );
        // a public key or a certificate is not a secret
        let benign = "-----BEGIN PUBLIC KEY-----\nMFkw\n-----END PUBLIC KEY-----\n";
        assert!(
            scan(None, benign, no_known)
                .iter()
                .all(|m| m.kind != LeakKind::PrivateKeyBlock),
            "public keys must not match"
        );
    }

    #[test]
    fn a_high_entropy_stranger_flags_but_does_not_refuse() {
        let new = "checksum: Qx9zPaB3cD7eF1Gh2jK4LmN6pRsT8uVwXy0ZaBcDeFg\n";
        let m = scan(None, new, no_known);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].kind, LeakKind::SecretShaped);
        assert!(!m[0].kind.refuses());
    }

    #[test]
    fn benign_prose_paths_uuids_and_short_values_match_nothing() {
        let new = "\
# ordinary readme prose about configuration\n\
see https://example.com/deeply/nested/documentation/path\n\
id: 2f6b8a1e-9c4d-4e7a-b3f5-0d8c6a2e9b41\n\
POSTGRES_PASSWORD=postgres\n\
a_snake_case_identifier_that_is_long = true\n";
        let m = scan(None, new, no_known);
        assert!(m.is_empty(), "{m:?}");
    }

    #[test]
    fn a_token_already_in_the_file_does_not_trip_the_edit_that_touches_it() {
        let old = format!("intro\ntoken: {ANT}\noutro\n");
        // unrelated edit
        let new = format!("intro (edited)\ntoken: {ANT}\noutro\n");
        assert!(scan(Some(&old), &new, no_known).is_empty());
        // the edit that REMOVES the token must also pass
        let removed = "intro\ntoken: (rotated away)\noutro\n";
        assert!(scan(Some(&old), removed, no_known).is_empty());
        // but the same token moving into a NEW line of a file that never
        // had it still trips
        let fresh = format!("notes\n{ANT}\n");
        assert_eq!(scan(Some("notes\n"), &fresh, no_known).len(), 1);
    }

    #[test]
    fn duplicate_matches_collapse_to_the_first_line() {
        let new = format!("{ANT}\nagain: {ANT}\n");
        let m = scan(None, &new, no_known);
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(m[0].line, 1);
    }

    #[test]
    fn below_floor_tokens_never_reach_the_digest_lookup() {
        let new = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        scan(None, new, |digests: &[String]| {
            assert!(digests.is_empty(), "below-floor digests were looked up");
            vec![None; digests.len()]
        });
    }
}
