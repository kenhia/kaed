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
        } else if let Some((i, kind, detail)) = index_free_tier(&group.variants, true, true) {
            let dedupe = match kind {
                LeakKind::ProviderPrefix => format!("prefix:{detail}"),
                _ => format!("shape:{}", secrets::digest_of(&group.variants[i])),
            };
            (kind, detail, None, dedupe)
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
/// The tiers that need no index, strongest first: a provider prefix, then
/// the high-entropy shape. Returns which variant hit, so a caller that
/// rewrites text (the 027 redaction hook) replaces exactly that span —
/// `KLAMS_TOKEN=<value>` loses the value, not the key.
fn index_free_tier<S: AsRef<str>>(
    variants: &[S],
    prefix: bool,
    shaped: bool,
) -> Option<(usize, LeakKind, String)> {
    if prefix
        && let Some((i, prefix)) = variants
            .iter()
            .enumerate()
            .find_map(|(i, v)| secrets::provider_prefix(v.as_ref()).map(|p| (i, p)))
    {
        return Some((i, LeakKind::ProviderPrefix, prefix.to_owned()));
    }
    if !shaped {
        return None;
    }
    variants
        .iter()
        .position(|v| shaped_for_leak(v.as_ref()))
        .map(|i| {
            let n = variants[i].as_ref().chars().count();
            (
                i,
                LeakKind::SecretShaped,
                format!("{n}-char high-entropy token"),
            )
        })
}

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
    is_private_key_label(label).then(|| format!("BEGIN {label}"))
}

/// A real armor label is uppercase words ending `PRIVATE KEY` (`RSA `,
/// `OPENSSH `, `ENCRYPTED `, or none). Measured in 027: every other match
/// was a regex or prose *about* armor — `BEGIN [A-Z ]*PRIVATE KEY`,
/// `BEGIN … PRIVATE KEY` — and an unterminated one swallowed up to 6.5 KB
/// of the output after it.
fn is_private_key_label(label: &str) -> bool {
    label.ends_with("PRIVATE KEY")
        && label
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' ')
}

/// Provider-prefixed tokens shorter than this are left in tool output
/// (027). Measured over 158k tool results: every prefixed match under 20
/// characters was documentation naming the prefix (`sk-ant-admin01-…`) or
/// an agent's own deliberate fingerprint (`first 12 chars`), and every
/// real credential was 40+. AWS access-key IDs are exactly 20 and stay in.
/// The write path keeps `provider_prefix`'s own floor: refusing a short
/// match there costs an override, redacting one here costs the reader.
pub const MIN_PREFIXED_REDACT_LEN: usize = 20;

/// Which tiers a redaction pass applies (027). The hook's default is the
/// precise three; `secret_shaped` is the heuristic 012 D-3 keeps to a
/// warning on the write path, and the 027 measurement says why it stays
/// off here too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiers {
    pub known_digest: bool,
    pub provider_prefix: bool,
    pub private_key: bool,
    pub secret_shaped: bool,
}

impl Tiers {
    pub const PRECISE: Tiers = Tiers {
        known_digest: true,
        provider_prefix: true,
        private_key: true,
        secret_shaped: false,
    };
    pub const ALL: Tiers = Tiers {
        secret_shaped: true,
        ..Tiers::PRECISE
    };
}

/// One span a redaction pass replaced. `start..end` are byte offsets into
/// the ORIGINAL text — the measurement reads context around them; the
/// value itself is never carried.
#[derive(Debug, Clone)]
pub struct Redacted {
    pub start: usize,
    pub end: usize,
    pub kind: LeakKind,
    /// As `LeakMatch::detail`: a digest, a prefix, an armor label, a shape.
    pub detail: String,
    pub source: Option<DigestLocation>,
}

#[derive(Debug, Clone)]
pub struct Redaction {
    pub text: String,
    pub spans: Vec<Redacted>,
}

/// Replace every secret `text` contains with a sealed placeholder (027,
/// the shell-output hook). Same tokenizer, variants and tiers as `scan`,
/// with two differences that follow from the input: there is no old
/// content, so every token is a candidate; and private-key armor is found
/// anywhere in a line, not only as a whole line, because tool output is
/// often JSON with the key's newlines escaped.
pub fn redact(
    text: &str,
    tiers: Tiers,
    lookup: impl FnOnce(&[String]) -> Vec<Option<DigestLocation>>,
) -> Redaction {
    let mut spans: Vec<Redacted> = Vec::new();
    if tiers.private_key {
        spans.extend(armor_blocks(text));
    }

    // one candidate run: its variants as (offset in text, len)
    struct Run {
        variants: Vec<(usize, usize)>,
        digests: Vec<usize>, // indices into `variants` that cleared the floor
    }
    let mut runs: Vec<Run> = Vec::new();
    let mut start: Option<usize> = None;
    let bytes_end = text.len();
    let push_run = |s: usize, e: usize, runs: &mut Vec<Run>| {
        if e - s < MIN_TOKEN_LEN || spans_cover(&spans, s) {
            return;
        }
        let run = &text[s..e];
        let variants: Vec<(usize, usize)> = variants_of(run)
            .into_iter()
            .map(|v| (s + (v.as_ptr() as usize - run.as_ptr() as usize), v.len()))
            .collect();
        let digests = if tiers.known_digest {
            (0..variants.len())
                .filter(|&i| {
                    let (o, n) = variants[i];
                    secrets::clears_floor(&text[o..o + n])
                })
                .collect()
        } else {
            Vec::new()
        };
        runs.push(Run { variants, digests });
    };
    for (i, c) in text.char_indices() {
        match (secrets::is_token_char(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                push_run(s, i, &mut runs);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        push_run(s, bytes_end, &mut runs);
    }

    let all: Vec<String> = runs
        .iter()
        .flat_map(|r| {
            r.digests.iter().map(|&i| {
                let (o, n) = r.variants[i];
                secrets::digest_of(&text[o..o + n])
            })
        })
        .collect();
    let mut located = if all.is_empty() {
        Vec::new()
    } else {
        lookup(&all)
    }
    .into_iter();

    for run in &runs {
        let mut known: Option<(usize, DigestLocation)> = None;
        for &i in &run.digests {
            if let Some(loc) = located.next().flatten()
                && known.is_none()
            {
                known = Some((i, loc));
            }
        }
        let strs: Vec<&str> = run.variants.iter().map(|&(o, n)| &text[o..o + n]).collect();
        let (i, kind, detail, source) = if let Some((i, loc)) = known {
            (
                i,
                LeakKind::KnownDigest,
                secrets::digest_of(strs[i]),
                Some(loc),
            )
        } else if let Some((i, kind, detail)) =
            index_free_tier(&strs, tiers.provider_prefix, tiers.secret_shaped)
        {
            if kind == LeakKind::ProviderPrefix && strs[i].len() < MIN_PREFIXED_REDACT_LEN {
                continue;
            }
            (i, kind, detail, None)
        } else {
            continue;
        };
        let (o, n) = run.variants[i];
        spans.push(Redacted {
            start: o,
            end: o + n,
            kind,
            detail,
            source,
        });
    }

    spans.sort_by_key(|s| s.start);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for s in &spans {
        out.push_str(&text[at..s.start]);
        out.push_str(&secrets::placeholder(&label_of(s), &text[s.start..s.end]));
        at = s.end;
    }
    out.push_str(&text[at..]);
    Redaction { text: out, spans }
}

/// The placeholder's KEY slot: the variable a known digest lives in when
/// kaed knows it, otherwise what kind of thing was there.
fn label_of(s: &Redacted) -> String {
    match s.kind {
        LeakKind::KnownDigest => match &s.source {
            Some(loc) if !loc.key.is_empty() => loc.key.clone(),
            _ => "known".into(),
        },
        LeakKind::ProviderPrefix => s.detail.trim_end_matches(['-', '_']).to_owned(),
        LeakKind::PrivateKeyBlock => "private_key".into(),
        LeakKind::SecretShaped => "shaped".into(),
    }
}

fn spans_cover(spans: &[Redacted], at: usize) -> bool {
    spans.iter().any(|s| s.start <= at && at < s.end)
}

/// `-----BEGIN <label>PRIVATE KEY-----` … through the matching END marker,
/// or to the end of the text when the output was cut off mid-key (a
/// truncated key is still a key).
fn armor_blocks(text: &str) -> Vec<Redacted> {
    const OPEN: &str = "-----BEGIN ";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = text[from..].find(OPEN) {
        let begin = from + rel;
        let after = begin + OPEN.len();
        let Some(close) = text[after..].find("-----") else {
            break;
        };
        let label = &text[after..after + close];
        let header_end = after + close + 5;
        if !is_private_key_label(label) {
            from = after;
            continue;
        }
        let end = armor_block_end(text, header_end, label);
        out.push(Redacted {
            start: begin,
            end,
            kind: LeakKind::PrivateKeyBlock,
            detail: format!("BEGIN {label}"),
            source: None,
        });
        from = end;
    }
    out
}

/// Where a key block that opened at `header_end` stops. Line by line —
/// a line break is raw or `\n`-escaped, since tool output is often JSON —
/// through base64 lines and legacy PEM headers, ending after the matching
/// END marker. Output cut off mid-key ends at the last key line; a prose
/// mention of the armor has no key lines, so only the header goes.
fn armor_block_end(text: &str, header_end: usize, label: &str) -> usize {
    let end_marker = format!("-----END {label}-----");
    let mut end = header_end;
    let mut pos = header_end;
    loop {
        let rest = &text[pos..];
        let brk = ["\r\n", "\n", "\\r\\n", "\\n"]
            .iter()
            .find(|b| rest.starts_with(**b));
        let Some(brk) = brk else { return end };
        pos += brk.len();
        let line_len = text[pos..]
            .find(['\n', '\r', '\\'])
            .unwrap_or(text.len() - pos);
        let line = &text[pos..pos + line_len];
        if line.starts_with(&end_marker) {
            return pos + end_marker.len();
        }
        let key_line = (!line.is_empty()
            && line
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')))
            || line.starts_with("Proc-Type:")
            || line.starts_with("DEK-Info:")
            || line.is_empty();
        if !key_line {
            return end;
        }
        pos += line_len;
        end = pos;
    }
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

    #[test]
    fn redact_replaces_only_the_value_and_names_the_variable() {
        let text = format!("KLAMS_TOKEN={KLAMS}\nother line\n");
        let r = redact(&text, Tiers::PRECISE, known_klams());
        assert_eq!(r.spans.len(), 1, "{:?}", r.spans);
        assert_eq!(r.spans[0].kind, LeakKind::KnownDigest);
        assert!(!r.text.contains(KLAMS));
        assert_eq!(
            r.text,
            format!(
                "KLAMS_TOKEN=\u{27E8}kaed:KLAMS_TOKEN@{}\u{27E9}\nother line\n",
                secrets::digest_of(KLAMS)
            )
        );
    }

    #[test]
    fn redact_catches_provider_prefixes_without_an_index() {
        let text = format!("{{\"api_key\": \"{ANT}\"}}");
        let r = redact(&text, Tiers::PRECISE, no_known);
        assert_eq!(r.spans.len(), 1);
        assert_eq!(r.spans[0].kind, LeakKind::ProviderPrefix);
        assert!(!r.text.contains(ANT));
        assert!(
            r.text.starts_with("{\"api_key\": \"\u{27E8}kaed:sk-ant@"),
            "{}",
            r.text
        );
    }

    #[test]
    fn redact_swallows_a_whole_key_block_even_json_escaped() {
        let body = "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ";
        for text in [
            format!(
                "x\n-----BEGIN OPENSSH PRIVATE KEY-----\n{body}\n-----END OPENSSH PRIVATE KEY-----\ny\n"
            ),
            format!(
                "{{\"k\":\"-----BEGIN RSA PRIVATE KEY-----\\n{body}\\n-----END RSA PRIVATE KEY-----\\n\"}}"
            ),
            format!("cut off: -----BEGIN PRIVATE KEY-----\n{body}"),
        ] {
            let r = redact(&text, Tiers::PRECISE, no_known);
            assert_eq!(r.spans.len(), 1, "{text}: {:?}", r.spans);
            assert_eq!(r.spans[0].kind, LeakKind::PrivateKeyBlock);
            assert!(!r.text.contains(body), "{}", r.text);
            assert!(!r.text.contains("PRIVATE KEY-----"), "{}", r.text);
        }
    }

    #[test]
    fn a_public_key_or_certificate_is_not_redacted() {
        let text = "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----\n";
        assert!(redact(text, Tiers::PRECISE, no_known).spans.is_empty());
    }

    #[test]
    fn precise_tiers_leave_hashes_alone_and_the_shaped_tier_does_not() {
        let sha = "3f786850e387550fdab836ed7e6dc881de23001b";
        let text = format!("commit {sha}\nversion 0123456789abcdef\n");
        let precise = redact(&text, Tiers::PRECISE, no_known);
        assert!(precise.spans.is_empty());
        assert_eq!(precise.text, text);
        let all = redact(&text, Tiers::ALL, no_known);
        assert_eq!(
            all.spans.len(),
            1,
            "a git SHA is what the shaped tier costs"
        );
        assert_eq!(all.spans[0].kind, LeakKind::SecretShaped);
    }

    #[test]
    fn redacting_twice_changes_nothing_the_second_time() {
        let text = format!("a {ANT} b KLAMS_TOKEN={KLAMS}");
        let once = redact(&text, Tiers::PRECISE, known_klams()).text;
        let twice = redact(&once, Tiers::PRECISE, no_known);
        assert!(twice.spans.is_empty(), "{:?}", twice.spans);
        assert_eq!(twice.text, once);
    }

    #[test]
    fn prose_and_regexes_about_armor_swallow_nothing_after_them() {
        for text in [
            "pattern = r\"-----BEGIN [A-Z ]*PRIVATE KEY-----\" then more code",
            "the `-----BEGIN … PRIVATE KEY-----` armor, and the rest of the doc",
            "-----BEGIN RSA PRIVATE KEY----- is what a key starts with.\nnext line",
        ] {
            let r = redact(text, Tiers::PRECISE, no_known);
            let kept: usize = r.spans.iter().map(|s| s.end - s.start).sum();
            assert!(
                kept <= "-----BEGIN RSA PRIVATE KEY-----".len(),
                "{text}: {:?}",
                r.spans
            );
            assert!(r.text.ends_with(&text[text.len() - 8..]), "{}", r.text);
        }
    }

    #[test]
    fn a_legacy_encrypted_pem_is_one_block() {
        let text = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,0A1B\n\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\nafter\n";
        let r = redact(text, Tiers::PRECISE, no_known);
        assert_eq!(r.spans.len(), 1);
        assert!(r.text.ends_with("\u{27E9}\nafter\n"), "{}", r.text);
    }

    #[test]
    fn short_prefixed_mentions_are_left_for_the_reader() {
        let text = "Admin keys start `sk-ant-admin01-` and AKIAIOSFODNN7EXAMPLE is an AWS id";
        let r = redact(text, Tiers::PRECISE, no_known);
        assert_eq!(r.spans.len(), 1, "{:?}", r.spans);
        assert_eq!(r.spans[0].detail, "AKIA");
        // the write path is unchanged: a short prefixed token still refuses
        assert!(
            scan(None, "x sk-ant-admin01-", no_known)
                .iter()
                .any(|m| m.kind == LeakKind::ProviderPrefix)
        );
    }
}
