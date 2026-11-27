//! Values-free inventory of the credentials in a corpus of Claude Code
//! transcripts (sprint 028; first run 2026-11-10 under korg program 4368).
//!
//! Walks every `*.jsonl` under the given directories with `leak::redact` plus
//! the same credential-context probe as `redact_measure`, and writes one row
//! per distinct value: a fingerprint (`sha256[:12]` — krot's own fingerprint,
//! so the two can be joined), kaed's digest, key names, shape, length, counts,
//! hosts and a file offset to read the value back from in-process.
//!
//! **Never prints a value.** Skeleton samples mask the match and every
//! credential-looking token around it; `a_report_never_holds_a_value` pins
//! that over the whole output, reverse check included.
//!
//! ```sh
//! just leak-inventory .scratch/inventory.json /ai-data/kagviz-data/live/*/projects
//! # or, with the reverse check (a file of krot fingerprints, whitespace-separated):
//! cargo run --release --example leak_inventory -- \
//!     --out .scratch/inventory.json --reverse-fps .scratch/fps.txt <dirs>...
//! ```

use aho_corasick::AhoCorasick;
use clap::Parser;
use kaed::hook;
use kaed::leak::{self, LeakKind, Tiers};
use kaed::secrets;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Parser)]
struct Args {
    /// Transcript directories (searched recursively for *.jsonl).
    #[arg(required = true)]
    dirs: Vec<PathBuf>,
    /// kaed journal holding the known-digest index (opened read-only).
    #[arg(long)]
    journal: Option<PathBuf>,
    /// Where the inventory JSON goes. A reverse check writes `reverse.json`
    /// beside it.
    #[arg(long)]
    out: PathBuf,
    /// File of fingerprints (sha256[:12], whitespace-separated — krot's
    /// registry and the current authorities) to look for anywhere in the
    /// corpus. Catches what a shape detector misses.
    #[arg(long)]
    reverse_fps: Option<PathBuf>,
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
    "requirepass",
    "auth",
];

fn fp(v: &str) -> String {
    fp_bytes(v.as_bytes())
}

/// krot's fingerprint: the first 12 hex characters of sha256.
fn fp_bytes(v: &[u8]) -> String {
    let h = Sha256::digest(v);
    h.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
struct Item {
    value: String, // in memory only
    classes: BTreeSet<String>,
    keys: BTreeMap<String, u64>,
    shape: String,
    hits: u64,
    samples: Vec<String>,
    first: String,
    last: String,
    files: BTreeMap<String, u64>, // host -> transcript files containing it (raw match)
    last_file_mtime: u64,
    loc: Option<(String, usize, usize)>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let journal = args
        .journal
        .unwrap_or_else(kaed::config::default_journal_path);
    let index = hook::load_index(&journal)?;
    eprintln!("index: {} digests", index.len());

    let files = transcripts(&args.dirs);
    eprintln!("files: {}", files.len());

    let (rows, reverse) = inventory(&index, &files, args.reverse_fps.as_deref())?;
    if let Some(reverse) = reverse {
        let rp = args.out.with_file_name("reverse.json");
        std::fs::write(&rp, serde_json::to_string_pretty(&reverse)?)?;
        eprintln!(
            "reverse: {} fps checked, {} found",
            reverse["checked"],
            reverse["found"].as_object().map_or(0, |o| o.len())
        );
    }
    std::fs::write(&args.out, serde_json::to_string_pretty(&rows)?)?;
    eprintln!(
        "wrote {} ({} distinct values)",
        args.out.display(),
        rows.len()
    );
    Ok(())
}

fn transcripts(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for d in dirs {
        for e in ignore::WalkBuilder::new(d)
            .standard_filters(false)
            .build()
            .flatten()
        {
            if e.path().extension().and_then(|x| x.to_str()) == Some("jsonl") {
                files.push(e.path().to_path_buf());
            }
        }
    }
    files.sort();
    files
}

/// The whole report, values-free: the rows, plus the reverse check when a
/// fingerprint list was given. Everything that reaches a file goes through
/// here, which is what lets one test pin "never a value" for all of it.
fn inventory(
    index: &hook::DigestIndex,
    files: &[PathBuf],
    reverse_fps: Option<&Path>,
) -> anyhow::Result<(Vec<Value>, Option<Value>)> {
    let mut items: HashMap<String, Item> = HashMap::new();
    for path in files {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let ts = v
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let mut leaves = Vec::new();
            collect(&v, &mut leaves);
            for text in leaves {
                if text.len() < 12 {
                    continue;
                }
                scan(text, &ts, index, &mut items);
            }
        }
    }
    eprintln!("distinct candidates: {}", items.len());

    // pass 2: which transcripts hold each value at all (raw bytes)
    let mut order: Vec<String> = items.keys().cloned().collect();
    order.sort();
    let pats: Vec<&str> = order.iter().map(|k| items[k].value.as_str()).collect();
    let ac = AhoCorasick::new(&pats)?;
    for path in files {
        let Ok(raw) = std::fs::read(path) else {
            continue;
        };
        let host = host_of(path);
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut seen = BTreeSet::new();
        for m in ac.find_overlapping_iter(&raw) {
            let i = m.pattern().as_usize();
            if seen.insert(i) {
                let it = items.get_mut(&order[i]).unwrap();
                if it.loc.is_none() {
                    it.loc = Some((
                        path.to_string_lossy().into_owned(),
                        m.start(),
                        m.end() - m.start(),
                    ));
                }
            }
        }
        for i in seen {
            let it = items.get_mut(&order[i]).unwrap();
            *it.files.entry(host.clone()).or_default() += 1;
            it.last_file_mtime = it.last_file_mtime.max(mtime);
        }
    }

    // pass 3: reverse check — is any value with one of these fingerprints
    // (krot's registry, current authorities) present anywhere in the corpus?
    let mut reverse = None;
    if let Some(list) = reverse_fps {
        let want: HashSet<String> = std::fs::read_to_string(list)?
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        let mut found: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for path in files {
            let Ok(raw) = std::fs::read(path) else {
                continue;
            };
            let host = host_of(path);
            let mut hit = BTreeSet::new();
            for wide in [false, true] {
                let ok = |c: u8| {
                    c.is_ascii_alphanumeric()
                        || c == b'_'
                        || c == b'-'
                        || (wide
                            && matches!(
                                c,
                                b'+' | b'/'
                                    | b'='
                                    | b'.'
                                    | b':'
                                    | b'@'
                                    | b'!'
                                    | b'#'
                                    | b'%'
                                    | b'^'
                                    | b'*'
                                    | b'~'
                            ))
                };
                let mut i = 0;
                while i < raw.len() {
                    if !ok(raw[i]) {
                        i += 1;
                        continue;
                    }
                    let st = i;
                    while i < raw.len() && ok(raw[i]) {
                        i += 1;
                    }
                    if i - st >= 8 && i - st <= 4096 {
                        let f = fp_bytes(&raw[st..i]);
                        if want.contains(&f) {
                            hit.insert(f);
                        }
                    }
                }
            }
            // whole quoted strings, so a value with a space in it is seen too
            for q in [0x27u8, 0x22u8] {
                let mut st: Option<usize> = None;
                for (i, &c) in raw.iter().enumerate() {
                    if c == 0x0a {
                        st = None;
                    } else if c == q || (q == 0x22 && c == 0x5c) {
                        if let Some(a) = st
                            && (8..=4096).contains(&(i - a))
                        {
                            let f = fp_bytes(&raw[a..i]);
                            if want.contains(&f) {
                                hit.insert(f);
                            }
                        }
                        st = Some(i + 1);
                    }
                }
            }
            for f in hit {
                *found.entry(f).or_default().entry(host.clone()).or_default() += 1;
            }
        }
        reverse = Some(json!({"checked": want.len(), "found": found}));
    }

    let mut rows: Vec<Value> = order
        .iter()
        .map(|k| {
            let it = &items[k];
            json!({
                "fp": k,
                "kaed_digest": secrets::digest_of(&it.value),
                "len": it.value.len(),
                "shape": it.shape,
                "classes": it.classes,
                "keys": it.keys,
                "hits": it.hits,
                "files": it.files,
                "files_total": it.files.values().sum::<u64>(),
                "first": it.first,
                "last": it.last,
                "samples": it.samples,
                "loc": it.loc,
            })
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r["files_total"].as_u64().unwrap_or(0)));
    Ok((rows, reverse))
}

fn host_of(p: &Path) -> String {
    let s = p.to_string_lossy();
    s.split("/live/")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap_or("?")
        .to_owned()
}

fn collect<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
        Value::Object(o) => o.values().for_each(|x| collect(x, out)),
        _ => {}
    }
}

fn add(
    items: &mut HashMap<String, Item>,
    value: &str,
    class: String,
    key: Option<String>,
    ts: &str,
    sample: impl FnOnce() -> String,
) {
    let it = items.entry(fp(value)).or_default();
    if it.value.is_empty() {
        it.value = value.to_owned();
        it.shape = secrets::shape_of(value);
        it.first = ts.to_owned();
    }
    it.hits += 1;
    it.classes.insert(class);
    if let Some(k) = key {
        *it.keys.entry(k).or_default() += 1;
    }
    if !ts.is_empty() {
        if it.first.is_empty() || ts < it.first.as_str() {
            it.first = ts.to_owned();
        }
        if ts > it.last.as_str() {
            it.last = ts.to_owned();
        }
    }
    if it.samples.len() < 3 {
        let s = sample();
        if !it.samples.contains(&s) {
            it.samples.push(s);
        }
    }
}

fn scan(text: &str, ts: &str, index: &hook::DigestIndex, items: &mut HashMap<String, Item>) {
    let r = leak::redact(text, Tiers::ALL, |ds| {
        ds.iter().map(|d| index.get(d).cloned()).collect()
    });
    let mut precise: Vec<(usize, usize)> = Vec::new();
    for s in &r.spans {
        if s.kind == LeakKind::SecretShaped || s.kind == LeakKind::PrivateKeyBlock {
            continue;
        }
        precise.push((s.start, s.end));
        let value = &text[s.start..s.end];
        let class = match s.kind {
            LeakKind::ProviderPrefix => format!("provider_prefix {}", s.detail),
            _ => "known_digest".to_owned(),
        };
        let key = s
            .source
            .as_ref()
            .map(|l| l.key.clone())
            .or_else(|| key_before(text, s.start));
        add(items, value, class, key, ts, || {
            skeleton(text, s.start, s.end)
        });
    }
    for (vs, ve) in credential_values(text) {
        if precise.iter().any(|&(a, b)| a < ve && vs < b) {
            continue;
        }
        let value = &text[vs..ve];
        if !secrets::clears_floor(value) {
            continue;
        }
        let shaped = r
            .spans
            .iter()
            .any(|s| s.kind == LeakKind::SecretShaped && s.start < ve && vs < s.end);
        if !shaped && !value.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        let class = if shaped {
            "context shaped"
        } else {
            "context unshaped"
        };
        add(
            items,
            value,
            class.to_owned(),
            key_before(text, vs),
            ts,
            || skeleton(text, vs, ve),
        );
    }
}

/// The identifier in key position before a value (never the value).
fn key_before(text: &str, start: usize) -> Option<String> {
    let b = text.as_bytes();
    let mut i = start;
    let lower_tail = |i: usize, w: &str| {
        i >= w.len()
            && text.is_char_boundary(i - w.len())
            && text[i - w.len()..i].eq_ignore_ascii_case(w)
    };
    loop {
        while i > 0 && matches!(b[i - 1], b'"' | b'\'' | b' ' | b'\t' | b':' | b'=' | b'\\') {
            i -= 1;
        }
        if lower_tail(i, "bearer") {
            i -= 6;
            continue;
        }
        break;
    }
    let end = i;
    while i > 0 && (b[i - 1].is_ascii_alphanumeric() || matches!(b[i - 1], b'_' | b'-' | b'.')) {
        i -= 1;
    }
    if end == i || end - i > 48 {
        return None;
    }
    Some(text[i..end].to_owned())
}

fn credential_values(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let b = text.as_bytes();
    let mut out = Vec::new();
    for kw in KEYWORDS {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(kw) {
            let mut i = from + rel + kw.len();
            from = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
                i += 1;
            }
            while i < b.len() && matches!(b[i], b'"' | b'\'' | b' ' | b'\t' | b'\\') {
                i += 1;
            }
            let bearer = *kw == "bearer";
            let requirepass = *kw == "requirepass";
            if bearer || requirepass {
                if i == from {
                    continue;
                }
            } else {
                if i >= b.len() || !matches!(b[i], b':' | b'=') {
                    continue;
                }
                i += 1;
                while i < b.len() && matches!(b[i], b'"' | b'\'' | b' ' | b'\t' | b'\\') {
                    i += 1;
                }
                if lower[i..].starts_with("bearer ") || lower[i..].starts_with("basic ") {
                    continue;
                }
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
            if v.starts_with(['$', '<', '{', '[', '*', '%', '/', '~'])
                || v.contains(secrets::PLACEHOLDER_OPEN)
                || v.contains("...")
                || v.contains('\u{2026}')
                || v.contains("://")
                || secrets::estimated_entropy_bits(v) < 50.0
            {
                continue;
            }
            out.push((start, i));
        }
    }
    out.sort();
    out.dedup_by_key(|(s, _)| *s);
    out
}

fn skeleton(text: &str, start: usize, end: usize) -> String {
    const W: usize = 90;
    let mut a = start.saturating_sub(W);
    while !text.is_char_boundary(a) {
        a -= 1;
    }
    let mut b = (end + W).min(text.len());
    while !text.is_char_boundary(b) {
        b += 1;
    }
    let v = &text[start..end];
    format!(
        "{}«VALUE {}:{}»{}",
        mask(&text[a..start]),
        secrets::shape_of(v),
        v.len(),
        mask(&text[end..b])
    )
    .replace(['\n', '\r'], "⏎")
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

#[cfg(test)]
mod tests {
    use super::*;
    use kaed::leak::DigestLocation;

    /// Fake, freshly shaped values: a provider-prefixed token, a value only
    /// the known-digest index can name, and one only the credential-context
    /// probe finds.
    const GHP: &str = "ghp_Q3x9LmA7vK2pR8sT1wY4zB6nC0dE5fGh2J";
    const INDEXED: &str = "Zq8XvN2mK7pL4rT9wB3yC6dF1gH5jA0s";
    const CONTEXT: &str = "k3Pz9QmW2xLr7Vt4Nb8Yc1Hd6Fs0Jg5A";

    fn corpus() -> (tempfile::TempDir, Vec<PathBuf>) {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("live/kai/projects/p");
        std::fs::create_dir_all(&host).unwrap();
        let lines = [
            json!({"timestamp": "2026-11-01T00:00:00Z", "toolUseResult": {"stdout": format!("export GH_TOKEN={GHP}\n")}}),
            json!({"timestamp": "2026-11-02T00:00:00Z", "message": {"content": format!("X_TOKEN={INDEXED} and more")}}),
            json!({"timestamp": "2026-11-03T00:00:00Z", "message": {"content": format!("db_password: {CONTEXT}")}}),
        ];
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(host.join("s.jsonl"), body).unwrap();
        let files = transcripts(&[dir.path().to_path_buf()]);
        (dir, files)
    }

    fn index() -> hook::DigestIndex {
        let mut idx = hook::DigestIndex::new();
        idx.insert(
            secrets::digest_of(INDEXED),
            DigestLocation {
                root: "kai:src".into(),
                path: "x/.env".into(),
                key: "X_TOKEN".into(),
            },
        );
        idx
    }

    #[test]
    fn a_report_never_holds_a_value() {
        let (dir, files) = corpus();
        let fps = dir.path().join("fps.txt");
        std::fs::write(&fps, format!("{} 000000000000\n", fp(GHP))).unwrap();

        let (rows, reverse) = inventory(&index(), &files, Some(&fps)).unwrap();
        let report = serde_json::to_string(&json!({"rows": rows, "reverse": reverse})).unwrap();

        for (name, v) in [("ghp", GHP), ("indexed", INDEXED), ("context", CONTEXT)] {
            assert!(!report.contains(v), "{name} value leaked into the report");
            // and no long slice of it either — a skeleton that kept 12
            // characters of a value would be a partial disclosure
            for w in v.as_bytes().windows(12) {
                let w = std::str::from_utf8(w).unwrap();
                assert!(!report.contains(w), "{name} value fragment {w:?} in report");
            }
        }
        // ...while still saying what it found.
        let fps_found: BTreeSet<&str> = rows.iter().filter_map(|r| r["fp"].as_str()).collect();
        let expect: BTreeSet<String> = [GHP, INDEXED, CONTEXT].iter().map(|v| fp(v)).collect();
        for e in &expect {
            assert!(fps_found.contains(e.as_str()), "missing {e}: {report}");
        }
        assert!(report.contains("GH_TOKEN"), "key names are reported");
        let reverse = reverse.unwrap();
        assert_eq!(reverse["checked"], 2);
        assert_eq!(reverse["found"][fp(GHP)]["kai"], 1);
    }
}
