//! Sprint 027's measurement: run `leak::redact` (every tier) over the tool
//! results in a corpus of Claude Code transcripts and report what each tier
//! would have redacted — and, through a credential-context probe, what the
//! precise tiers would have missed.
//!
//! **Never prints a value.** Counts, digests-as-identity (internal only),
//! shapes and lengths; context is a *skeleton* in which the match and every
//! token that could plausibly be a credential are replaced by `«shape:len»`.
//!
//! ```sh
//! cargo run --release --example redact_measure -- \
//!     --journal ~/.local/share/kaed/journal.db --committed ~/src \
//!     --out .scratch/measure.json /ai-data/kagviz-data/live/*/projects
//! ```

use clap::Parser;
use kaed::hook::{self, DigestIndex};
use kaed::leak::{self, LeakKind, Tiers};
use kaed::secrets;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    /// Transcript directories (searched recursively for *.jsonl).
    dirs: Vec<PathBuf>,
    #[arg(long)]
    journal: Option<PathBuf>,
    /// Directory holding git repos; every tracked file's redactable values
    /// become the "committed" set (fixtures, published examples).
    #[arg(long)]
    committed: Option<PathBuf>,
    #[arg(long)]
    out: PathBuf,
    /// Skeleton samples kept per class.
    #[arg(long, default_value_t = 12)]
    samples: usize,
}

const KEYWORDS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "api-key",
    "bearer",
    "authorization",
    "private_key",
    "access_key",
    "credential",
];

#[derive(Default)]
struct Class {
    hits: u64,
    distinct: HashSet<String>,
    sessions: HashSet<String>,
    committed_hits: u64,
    committed_distinct: HashSet<String>,
    shapes: BTreeMap<String, u64>,
    tools: BTreeMap<String, u64>,
    keys: BTreeMap<String, u64>,
    samples: Vec<Value>,
    sampled: HashSet<String>,
}

impl Class {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        digest: &str,
        session: &str,
        tool: &str,
        shape: String,
        committed: bool,
        key: Option<&str>,
        skeleton: impl FnOnce() -> String,
        max: usize,
    ) {
        self.hits += 1;
        self.distinct.insert(digest.to_owned());
        self.sessions.insert(session.to_owned());
        if committed {
            self.committed_hits += 1;
            self.committed_distinct.insert(digest.to_owned());
        }
        *self.shapes.entry(shape.clone()).or_default() += 1;
        *self.tools.entry(tool.to_owned()).or_default() += 1;
        if let Some(k) = key {
            *self.keys.entry(k.to_owned()).or_default() += 1;
        }
        // one sample per distinct value, so a fixture repeated 400 times
        // does not crowd out everything else
        if self.samples.len() < max && self.sampled.insert(digest.to_owned()) {
            self.samples.push(json!({
                "tool": tool, "shape": shape, "committed": committed, "context": skeleton(),
            }));
        }
    }

    fn report(&self) -> Value {
        json!({
            "hits": self.hits,
            "distinct_values": self.distinct.len(),
            "sessions": self.sessions.len(),
            "committed_hits": self.committed_hits,
            "committed_distinct": self.committed_distinct.len(),
            "shapes": self.shapes,
            "tools": self.tools,
            "keys": self.keys,
            "samples": self.samples,
        })
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let index = match &args.journal {
        Some(j) => hook::load_index(j)?,
        None => DigestIndex::new(),
    };
    eprintln!("index: {} digests", index.len());
    let committed = match &args.committed {
        Some(d) => committed_digests(d),
        None => HashSet::new(),
    };
    eprintln!("committed set: {} digests", committed.len());

    let mut classes: BTreeMap<String, Class> = BTreeMap::new();
    let mut seen_results: HashSet<String> = HashSet::new();
    let (mut files, mut results, mut bytes) = (0u64, 0u64, 0u64);
    let mut results_by_tool: BTreeMap<String, u64> = BTreeMap::new();
    let mut results_redacted_precise: BTreeMap<String, u64> = BTreeMap::new();
    let mut results_redacted_shaped: BTreeMap<String, u64> = BTreeMap::new();

    for dir in &args.dirs {
        for entry in ignore::WalkBuilder::new(dir)
            .standard_filters(false)
            .build()
        {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(raw) = std::fs::read_to_string(path) else {
                continue;
            };
            files += 1;
            let session = path.to_string_lossy().into_owned();
            let mut names: HashMap<String, String> = HashMap::new();
            for line in raw.lines() {
                let Ok(v) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
                    continue;
                };
                for item in content {
                    match item.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
                            if let (Some(id), Some(name)) = (
                                item.get("id").and_then(Value::as_str),
                                item.get("name").and_then(Value::as_str),
                            ) {
                                names.insert(id.to_owned(), tool_family(name));
                            }
                        }
                        Some("tool_result") => {
                            let id = item
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or("");
                            if !seen_results.insert(id.to_owned()) {
                                continue; // a resumed session replays history
                            }
                            let text = result_text(item.get("content"));
                            if text.is_empty() {
                                continue;
                            }
                            let tool = names.get(id).cloned().unwrap_or("?".into());
                            results += 1;
                            bytes += text.len() as u64;
                            *results_by_tool.entry(tool.clone()).or_default() += 1;
                            let (p, s) = measure_one(
                                &text,
                                &session,
                                &tool,
                                &index,
                                &committed,
                                &mut classes,
                                args.samples,
                            );
                            if p {
                                *results_redacted_precise.entry(tool.clone()).or_default() += 1;
                            }
                            if s {
                                *results_redacted_shaped.entry(tool).or_default() += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let report = json!({
        "files": files,
        "tool_results": results,
        "bytes": bytes,
        "index_digests": index.len(),
        "committed_digests": committed.len(),
        "results_by_tool": results_by_tool,
        "results_touched_by_precise_tiers": results_redacted_precise,
        "results_touched_by_shaped_tier": results_redacted_shaped,
        "classes": classes.iter().map(|(k, c)| (k.clone(), c.report())).collect::<BTreeMap<_, _>>(),
    });
    std::fs::write(&args.out, serde_json::to_string_pretty(&report)?)?;
    eprintln!(
        "files {files}, tool results {results}, {bytes} bytes → {}",
        args.out.display()
    );
    Ok(())
}

/// Returns (touched by a precise tier, touched by the shaped tier).
fn measure_one(
    text: &str,
    session: &str,
    tool: &str,
    index: &DigestIndex,
    committed: &HashSet<String>,
    classes: &mut BTreeMap<String, Class>,
    max: usize,
) -> (bool, bool) {
    let r = leak::redact(text, Tiers::ALL, |ds| {
        ds.iter().map(|d| index.get(d).cloned()).collect()
    });
    let (mut precise, mut shaped) = (false, false);
    for s in &r.spans {
        let value = &text[s.start..s.end];
        let digest = secrets::digest_of(value);
        let class = match s.kind {
            LeakKind::ProviderPrefix => format!("provider_prefix {}", s.detail),
            LeakKind::PrivateKeyBlock => format!("private_key_block {}", s.detail),
            LeakKind::KnownDigest => {
                // what the index adds: would an index-free tier have caught it?
                let also = if secrets::shape_of(value).starts_with("prefixed:") {
                    "also_prefixed"
                } else {
                    "index_only"
                };
                format!("known_digest {also}")
            }
            LeakKind::SecretShaped => format!("secret_shaped {}", length_bucket(value.len())),
        };
        if s.kind == LeakKind::SecretShaped {
            shaped = true;
        } else {
            precise = true;
        }
        let key = s.source.as_ref().map(|l| l.key.as_str());
        let shape = if s.kind == LeakKind::PrivateKeyBlock {
            format!("{} bytes", value.len())
        } else {
            secrets::shape_of(value)
        };
        classes.entry(class).or_default().add(
            &digest,
            session,
            tool,
            shape,
            committed.contains(&digest),
            key,
            || skeleton(text, s.start, s.end, s.kind.as_str()),
            max,
        );
    }

    // the false-negative probe: credential-looking assignments the precise
    // tiers left in place
    let precise_spans: Vec<(usize, usize)> = r
        .spans
        .iter()
        .filter(|s| s.kind != LeakKind::SecretShaped)
        .map(|s| (s.start, s.end))
        .collect();
    for (vs, ve, kw) in credential_values(text) {
        if precise_spans.iter().any(|&(a, b)| a < ve && vs < b) {
            continue;
        }
        let value = &text[vs..ve];
        let digest = secrets::digest_of(value);
        let class = if index.contains_key(&digest) {
            "missed known_digest (value not one token run)".to_owned()
        } else if r
            .spans
            .iter()
            .any(|s| s.kind == LeakKind::SecretShaped && s.start < ve && vs < s.end)
        {
            "missed (shaped tier only)".to_owned()
        } else {
            format!(
                "missed {}",
                if secrets::clears_floor(value) {
                    "above floor"
                } else {
                    "below floor"
                }
            )
        };
        let shape = format!(
            "{}{}",
            secrets::shape_of(value),
            if value.contains('/') { "+slash" } else { "" }
        );
        classes.entry(class).or_default().add(
            &digest,
            session,
            tool,
            shape,
            false,
            Some(kw),
            || skeleton(text, vs, ve, "candidate"),
            max,
        );
    }
    (precise, shaped)
}

fn length_bucket(n: usize) -> &'static str {
    match n {
        0..=39 => "32-39",
        40 => "40",
        41..=63 => "41-63",
        64 => "64",
        _ => "65+",
    }
}

fn tool_family(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("mcp__") {
        return format!("mcp__{}", rest.split("__").next().unwrap_or(""));
    }
    name.to_owned()
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Values assigned to a credential-named key: `FOO_TOKEN=v`, `"password": "v"`,
/// `Authorization: Bearer v`. Returns byte ranges and the keyword.
fn credential_values(text: &str) -> Vec<(usize, usize, &'static str)> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    for kw in KEYWORDS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(kw) {
            let mut i = from + rel + kw.len();
            from = i;
            let b = text.as_bytes();
            // rest of the identifier
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
                i += 1;
            }
            while i < b.len() && matches!(b[i], b'"' | b'\'' | b' ' | b'\t') {
                i += 1;
            }
            let bearer = *kw == "bearer";
            if !bearer {
                if i >= b.len() || !matches!(b[i], b':' | b'=') {
                    continue;
                }
                i += 1;
                while i < b.len() && matches!(b[i], b'"' | b'\'' | b' ' | b'\t') {
                    i += 1;
                }
                // `Authorization: Bearer x` — the value is after the scheme
                if lower[i..].starts_with("bearer ") || lower[i..].starts_with("basic ") {
                    continue; // the `bearer` keyword pass picks it up
                }
            } else if i == from || !b[from..i].iter().all(|c| *c == b' ') {
                continue;
            }
            let start = i;
            while i < b.len()
                && !matches!(
                    b[i],
                    b'"' | b'\''
                        | b' '
                        | b'\t'
                        | b'\n'
                        | b'\r'
                        | b','
                        | b'}'
                        | b';'
                        | b'\\'
                        | b')'
                        | b'`'
                )
            {
                i += 1;
            }
            if i - start < 8 || !text.is_char_boundary(start) || !text.is_char_boundary(i) {
                continue;
            }
            let v = &text[start..i];
            if v.starts_with(['$', '<', '{', '[', '*', '%'])
                || v.contains(secrets::PLACEHOLDER_OPEN)
                || v.contains("...")
                || v.contains('\u{2026}')
                || secrets::estimated_entropy_bits(v) < 50.0
                || !v.chars().any(|c| c.is_ascii_digit())
            {
                continue;
            }
            out.push((start, i, *kw));
        }
    }
    out.sort();
    out.dedup_by_key(|(s, _, _)| *s);
    out
}

/// A disclosable rendering of the text around `[start, end)`: the match
/// becomes `«label shape:len»`, and every other token that could be a
/// credential — anything with a digit, anything long, anything assigned
/// to a credential-named key — becomes `«shape:len»`.
fn skeleton(text: &str, start: usize, end: usize, label: &str) -> String {
    const W: usize = 70;
    let mut a = start.saturating_sub(W);
    while !text.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = (end + W).min(text.len());
    while !text.is_char_boundary(b) {
        b += 1;
    }
    let before = mask(&text[a..start]);
    let after = mask(&text[end..b]);
    let v = &text[start..end];
    let shape = if label == "private_key_block" {
        "armor".to_owned()
    } else {
        secrets::shape_of(v)
    };
    format!("{before}«{label} {shape}:{}»{after}", v.len()).replace(['\n', '\r'], "⏎")
}

fn mask(s: &str) -> String {
    let mut out = String::new();
    let mut tok = String::new();
    let mut after_assign = false;
    let flush = |tok: &mut String, out: &mut String, after_assign: bool| {
        if tok.is_empty() {
            return;
        }
        let risky = tok.len() >= 16
            || (tok.len() >= 5 && tok.chars().any(|c| c.is_ascii_digit()))
            || (after_assign && tok.len() >= 4);
        if risky {
            out.push_str(&format!("«{}:{}»", secrets::shape_of(tok), tok.len()));
        } else {
            out.push_str(tok);
        }
        tok.clear();
    };
    let lower = s.to_ascii_lowercase();
    let mut last_kw_end = None;
    for kw in KEYWORDS {
        if let Some(i) = lower.rfind(kw) {
            last_kw_end = Some(last_kw_end.map_or(i, |e: usize| e.max(i)));
        }
    }
    for (i, c) in s.char_indices() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.' | '/') {
            tok.push(c);
        } else {
            flush(&mut tok, &mut out, after_assign);
            if matches!(c, '=' | ':') {
                after_assign = last_kw_end.is_some_and(|k| k < i);
            } else if c == '\n' {
                after_assign = false;
            }
            out.push(c);
        }
    }
    flush(&mut tok, &mut out, after_assign);
    out
}

/// Digests of everything `redact` would replace in git-tracked files under
/// `root` — the fixture/published-example oracle.
fn committed_digests(root: &Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let repos: Vec<PathBuf> = ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .max_depth(Some(4))
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == ".git" && e.path().is_dir())
        .filter_map(|e| e.path().parent().map(Path::to_path_buf))
        .collect();
    for repo in repos {
        let Ok(o) = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["ls-files", "-z"])
            .output()
        else {
            continue;
        };
        for f in o.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()) {
            let p = repo.join(String::from_utf8_lossy(f).as_ref());
            if std::fs::metadata(&p)
                .map(|m| m.len() > 2_000_000)
                .unwrap_or(true)
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let r = leak::redact(&text, Tiers::ALL, |ds| vec![None; ds.len()]);
            for s in r.spans {
                out.insert(secrets::digest_of(&text[s.start..s.end]));
            }
        }
    }
    out
}
