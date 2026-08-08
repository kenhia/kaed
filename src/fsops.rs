//! Filesystem layer: path jailing, content versions, stat/list/read, and
//! the staging primitives the transaction engine builds on.
//!
//! Disk is the source of truth — nothing here caches content. Every read
//! hashes what it serves (R1); every path from the wire goes through the
//! jail before it touches the filesystem.

use crate::addr;
use crate::config::{Limits, ResolvedRoot};
use crate::dotenv;
use crate::errors::{KaedError, RefusalReason, Result};
use crate::policy;
use schemars::JsonSchema;
use serde::Serialize;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Bytes sniffed for NUL when deciding text vs binary.
const BINARY_SNIFF_LEN: usize = 8192;

pub fn version_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex()[..16].to_string()
}

pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_LEN)].contains(&0)
}

// ---------------------------------------------------------------- jailing

/// Lexically clean a wire path: relative, no `..`, `.` stripped. Empty
/// means the root itself.
fn clean_rel(rel: &str) -> Result<PathBuf> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(KaedError::outside_root(format!(
            "absolute paths are not accepted: {rel:?} (paths are relative to a root)"
        )));
    }
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            _ => {
                return Err(KaedError::outside_root(format!(
                    "path {rel:?} escapes its root"
                )));
            }
        }
    }
    Ok(out)
}

/// Refuse a denied path. Called on the lexical join *before* the
/// filesystem is touched, so a denied name answers the same whether or not
/// it exists, and again on the resolved target, so a symlink cannot walk
/// into a denied directory.
///
/// Two layers, in order of absoluteness: the server deny list, then any
/// `.kaedignore` between the root and the path. The in-file marker and
/// classification are content-level and live in `load_text`.
pub fn check_denied(root: &ResolvedRoot, rel: &str, abs: &Path) -> Result<()> {
    if let Some(rule) = root.deny.denied_by(abs) {
        return Err(KaedError::refused(
            rel,
            &rule,
            RefusalReason::ServerDenylist,
            "permanent server policy ([security] deny in kaed's config, or a built-in); \
             no retry or path variation will succeed — if the file is genuinely needed, \
             a human must change the config",
        ));
    }
    if let Some(m) = policy::kaedignore_denied(&root.path, abs) {
        let file = m
            .file
            .strip_prefix(&root.path)
            .unwrap_or(&m.file)
            .display()
            .to_string();
        return Err(KaedError::refused(
            rel,
            &m.pattern,
            RefusalReason::Kaedignore,
            format!(
                "denied by {file} in this root — read that file for the policy; \
                 it can only be changed outside kaed"
            ),
        ));
    }
    Ok(())
}

/// Resolve a path that must already exist. Canonicalizes (so symlinks
/// resolve) and requires the result to stay inside the root.
pub fn resolve_existing(root: &ResolvedRoot, rel: &str) -> Result<PathBuf> {
    let joined = root.path.join(clean_rel(rel)?);
    check_denied(root, rel, &joined)?;
    let canonical = joined.canonicalize().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => KaedError::not_found(format!("{rel}: not found")),
        // An unreadable ancestor directory refuses here, before the file is
        // ever reached; the subject of the refusal is the deepest thing
        // that still stats (014, #1091).
        std::io::ErrorKind::PermissionDenied => {
            crate::perm::not_readable(root, rel, deepest_existing(&joined))
        }
        _ => KaedError::internal(format!("{rel}: {e}")),
    })?;
    if !canonical.starts_with(&root.path) {
        return Err(KaedError::outside_root(format!(
            "{rel:?} resolves outside root {:?}",
            root.name
        )));
    }
    check_denied(root, rel, &canonical)?;
    Ok(canonical)
}

/// The deepest ancestor of `p` (including `p`) that can still be stat'ed.
/// When a permission refusal comes from partway up a path, that ancestor is
/// the thing whose ownership actually explains it — reporting the leaf
/// would name a file kaed never got close enough to see.
pub fn deepest_existing(p: &Path) -> &Path {
    p.ancestors()
        .find(|a| a.symlink_metadata().is_ok())
        .unwrap_or(p)
}

/// Resolve a path that may not exist yet (create targets, staged temps).
/// The deepest existing ancestor must canonicalize inside the root; the
/// not-yet-existing suffix is appended lexically.
pub fn resolve_creatable(root: &ResolvedRoot, rel: &str) -> Result<PathBuf> {
    let cleaned = clean_rel(rel)?;
    if cleaned.as_os_str().is_empty() {
        return Err(KaedError::invalid_input(
            "path must name a file, not the root",
        ));
    }
    let joined = root.path.join(&cleaned);
    let mut existing = joined
        .parent()
        .expect("joined path has a parent")
        .to_path_buf();
    let mut suffix = vec![
        joined
            .file_name()
            .expect("cleaned path has a file name")
            .to_owned(),
    ];
    while !existing.exists() {
        suffix.push(existing.file_name().expect("inside root").to_owned());
        existing = existing.parent().expect("inside root").to_path_buf();
    }
    let canonical = existing
        .canonicalize()
        .map_err(|e| KaedError::internal(format!("{rel}: {e}")))?;
    if !canonical.starts_with(&root.path) {
        return Err(KaedError::outside_root(format!(
            "{rel:?} resolves outside root {:?}",
            root.name
        )));
    }
    let mut out = canonical;
    for seg in suffix.iter().rev() {
        out.push(seg);
    }
    check_denied(root, rel, &out)?;
    Ok(out)
}

// ------------------------------------------------------------------ stat

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct StatResult {
    pub kind: EntryKind,
    pub size: u64,
    /// Present for files up to `max_file_bytes` (binary included — the
    /// staleness probe works on any file kaed could serve or refuse).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub executable: bool,
    /// RFC 3339, second precision.
    pub modified: String,
    pub binary: bool,
    /// Classified secret-bearing: `read` will serve it redacted (dotenv)
    /// or refuse with `classified_opaque`. Lexical, like the deny list.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub classified: bool,
}

pub fn stat(root: &ResolvedRoot, rel: &str, limits: &Limits) -> Result<StatResult> {
    // jail the parent, then look at the entry itself without following it,
    // so a symlink is reported as one instead of chased out of the jail
    let cleaned = clean_rel(rel)?;
    let abs = if cleaned.as_os_str().is_empty() {
        root.path.clone()
    } else {
        let parent_rel = cleaned.parent().unwrap_or(Path::new(""));
        let parent = resolve_existing(root, &parent_rel.to_string_lossy())?;
        parent.join(cleaned.file_name().expect("non-empty cleaned path"))
    };
    // the leaf never went through resolve_existing (a symlink here is
    // reported, not followed), so it needs the deny check of its own
    check_denied(root, rel, &abs)?;
    let meta = std::fs::symlink_metadata(&abs).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => KaedError::not_found(format!("{rel}: not found")),
        _ => KaedError::internal(format!("{rel}: {e}")),
    })?;

    let kind = if meta.is_symlink() {
        EntryKind::Symlink
    } else if meta.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    let modified = humantime::format_rfc3339_seconds(
        meta.modified()
            .map_err(|e| KaedError::internal(e.to_string()))?,
    )
    .to_string();

    let mut out = StatResult {
        kind,
        size: meta.len(),
        version: None,
        line_count: None,
        language: language_of(rel),
        executable: meta.permissions().mode() & 0o111 != 0,
        modified,
        binary: false,
        classified: root.classify.classified_by(&abs).is_some(),
    };
    if kind == EntryKind::File && meta.len() <= limits.max_file_bytes {
        let bytes = std::fs::read(&abs)?;
        out.binary = looks_binary(&bytes) || std::str::from_utf8(&bytes).is_err();
        out.version = Some(version_of(&bytes));
        if !out.binary {
            out.line_count = Some(
                addr::Lines::split(std::str::from_utf8(&bytes).expect("checked utf-8")).count(),
            );
        }
    }
    Ok(out)
}

fn language_of(rel: &str) -> Option<String> {
    let ext = Path::new(rel).extension()?.to_str()?;
    let lang = match ext {
        "rs" => "rust",
        "py" => "python",
        "md" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "sh" | "bash" => "bash",
        "js" | "mjs" => "javascript",
        "ts" | "tsx" => "typescript",
        _ => return None,
    };
    Some(lang.to_owned())
}

// ------------------------------------------------------------------ list

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListEntry {
    /// Root-relative path.
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListResult {
    pub entries: Vec<ListEntry>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    /// Entries the walk produced before `glob` filtered them. Always
    /// present, including zero: an empty `entries` with 41 scanned is a
    /// glob that selected nothing, and with 0 scanned is an empty subtree.
    pub entries_scanned: usize,
    /// Why the result is empty, when the emptiness is more likely to be the
    /// caller's scoping than the truth about the tree (korg #1066).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<EmptyReason>,
    /// Entries the deny list hid (a hidden directory counts once, with its
    /// subtree). Omitted when zero — present so a filtered listing is never
    /// mistaken for the whole directory.
    #[serde(skip_serializing_if = "is_zero")]
    pub denied_hidden: usize,
    /// Entries the OS refused to enumerate — unix permissions, not kaed
    /// policy (014, korg #1088). `list` walks directories itself, so it
    /// needs its own count for the same reason it needs its own deny check
    /// (R7's three-places rule).
    #[serde(skip_serializing_if = "is_zero")]
    pub unreadable_hidden: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// Why an enumerating tool came back empty, when the emptiness is probably
/// self-inflicted. The sibling of `denied_hidden`: both exist so a filtered
/// result is never mistaken for the whole picture (R3, R7).
///
/// `hint` is built from the call's own `glob` and `path` rather than
/// describing the rule in the abstract — the report that produced this
/// (korg #1066) came from an agent that had read the accurate parameter
/// docs and still drew the wrong conclusion from a zero.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EmptyReason {
    pub code: &'static str,
    pub hint: String,
}

/// What an enumerating walk actually looked at, so an honest zero can be
/// told from a self-inflicted one. `list` fills `candidates`/`kept`;
/// `search` fills all three, since it can also skip a glob-matched file for
/// being binary or over the size limit.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScopeTally {
    /// Produced by the walk, before `glob`.
    pub candidates: usize,
    /// …of those, admitted by `glob`.
    pub kept: usize,
    /// …of those, actually opened and read.
    pub opened: usize,
}

impl ScopeTally {
    /// The diagnosis, or `None` when the zero is genuine and there is
    /// nothing useful to add.
    pub fn explain(&self, glob: Option<&str>, path: &str) -> Option<EmptyReason> {
        let scope = if path.is_empty() {
            "the root".to_owned()
        } else {
            format!("{path:?}")
        };
        if self.candidates == 0 {
            return Some(EmptyReason {
                code: "no_files_under_path",
                hint: format!(
                    "nothing was scanned: {scope} contains no readable entries (or they \
                     are all gitignored — pass `ignored: true` to include them)."
                ),
            });
        }
        if let Some(g) = glob
            && self.kept == 0
        {
            let anchored = if path.is_empty() {
                format!("no path under the root matched `glob` {g:?}")
            } else {
                format!(
                    "`glob` is matched against ROOT-relative paths and is not re-anchored \
                     by `path`, so with path {path:?} the glob {g:?} could only ever match \
                     at the root's top level — try {:?} or {:?}",
                    format!("{}/{}", path.trim_end_matches('/'), g),
                    format!("**/{g}"),
                )
            };
            return Some(EmptyReason {
                code: "glob_matched_no_files",
                hint: format!(
                    "{} of {} scanned entries matched: {anchored}.",
                    self.kept, self.candidates
                ),
            });
        }
        if self.kept > 0 && self.opened == 0 {
            return Some(EmptyReason {
                code: "all_files_skipped",
                hint: format!(
                    "{} file(s) matched but none could be searched — all were binary, \
                     over `max_file_bytes`, or unreadable.",
                    self.kept
                ),
            });
        }
        None
    }
}

pub struct ListParams<'a> {
    pub path: &'a str,
    pub glob: Option<&'a str>,
    pub depth: usize,
    pub max: usize,
    pub offset: usize,
    /// Include gitignored entries.
    pub ignored: bool,
}

pub fn list(root: &ResolvedRoot, p: &ListParams) -> Result<ListResult> {
    let base = resolve_existing(root, p.path)?;
    if !base.is_dir() {
        return Err(KaedError::invalid_input(format!(
            "{}: not a directory",
            p.path
        )));
    }
    // Addressed, not enumerated: a directory the caller NAMED gets a
    // reason, where one merely walked into is skipped and counted (014
    // D-6). An empty listing for a directory kaed cannot open is honest
    // about the count and useless about the cause.
    if !crate::perm::dir_is_readable(&base) {
        return Err(crate::perm::not_readable(root, p.path, &base));
    }
    let matcher = match p.glob {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| KaedError::invalid_input(format!("bad glob {g:?}: {e}")))?
                .compile_matcher(),
        ),
        None => None,
    };

    // The deny check has to happen here as well as in the resolver: this
    // walk never calls resolve_existing per entry, so a resolver-only check
    // would still enumerate denied paths. `.kaedignore` denials are the
    // same layer (R7) and use the same counter; markers are content-level
    // and invisible to `list`, which never opens files (D-4).
    let denied_hidden = Arc::new(AtomicUsize::new(0));
    let mut tally = ScopeTally::default();
    let mut entries = Vec::new();
    let walker = {
        let deny = root.deny.clone();
        let kaedignore = policy::KaedignoreCache::new(root.path.clone());
        let counter = denied_hidden.clone();
        ignore::WalkBuilder::new(&base)
            .max_depth(Some(p.depth))
            .hidden(false)
            .git_ignore(!p.ignored)
            .git_global(!p.ignored)
            .git_exclude(!p.ignored)
            .ignore(!p.ignored)
            .parents(!p.ignored)
            .filter_entry(move |e| {
                if e.file_name() == ".git" {
                    return false;
                }
                if deny.is_denied(e.path()) || kaedignore.denied(e.path()).is_some() {
                    counter.fetch_add(1, Ordering::Relaxed);
                    return false;
                }
                true
            })
            .build()
    };
    let mut unreadable_hidden = 0usize;
    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            // Same rule as `search` (#1088): the OS is a filter, not a
            // fatality. Other walk errors still fail loudly.
            Err(e) if e.io_error().is_some_and(crate::perm::is_permission_denied) => {
                unreadable_hidden += 1;
                continue;
            }
            Err(e) => return Err(KaedError::internal(e.to_string())),
        };
        if entry.depth() == 0 {
            continue; // the listed dir itself
        }
        let abs = entry.path();
        let rel_to_root = abs
            .strip_prefix(&root.path)
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned();
        tally.candidates += 1;
        if let Some(m) = &matcher
            && !m.is_match(&rel_to_root)
        {
            continue;
        }
        tally.kept += 1;
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) if e.io_error().is_some_and(crate::perm::is_permission_denied) => {
                unreadable_hidden += 1;
                continue;
            }
            Err(e) => return Err(KaedError::internal(e.to_string())),
        };
        let kind = if meta.is_symlink() {
            EntryKind::Symlink
        } else if meta.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        };
        entries.push(ListEntry {
            path: rel_to_root,
            kind,
            size: if kind == EntryKind::File {
                meta.len()
            } else {
                0
            },
        });
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let total = entries.len();
    let page: Vec<ListEntry> = entries.into_iter().skip(p.offset).take(p.max).collect();
    let truncated = p.offset + page.len() < total;
    // Only when nothing matched at all: an empty *page* past the end is
    // already explained by `next_offset`, and diagnosing it as a scoping
    // mistake would be the wrong answer confidently given.
    tally.opened = tally.kept;
    Ok(ListResult {
        next_offset: truncated.then_some(p.offset + page.len()),
        entries: page,
        truncated,
        entries_scanned: tally.candidates,
        reason: (total == 0)
            .then(|| tally.explain(p.glob, p.path))
            .flatten(),
        denied_hidden: denied_hidden.load(Ordering::Relaxed),
        unreadable_hidden,
    })
}

// ------------------------------------------------------------------ read

#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub struct LineRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReadResult {
    pub content: String,
    /// Always the **raw** file's version — a content address of the bytes
    /// on disk (R1), valid as an edit base even when `content` is a
    /// redacted view.
    pub version: String,
    /// The lines actually returned (1-based, inclusive).
    pub range: LineRange,
    pub total_lines: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    /// True when `content` is the redacted rendering of a classified file:
    /// values replaced by sealed placeholders, line-for-line with the raw
    /// file, so ranges, anchors and line numbers all still hold (D-7).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
    /// The typed per-entry view (whole-file reads of classified dotenv).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dotenv: Option<Vec<dotenv::EntryView>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_hint: Option<&'static str>,
    /// e.g. a classified file that is not gitignored (D-12).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub enum ReadMode<'a> {
    Whole,
    Range { start: usize, end: usize },
    WindowLine { line: usize, context: usize },
    WindowAnchor { anchor: &'a str, context: usize },
}

/// What the policy layers decided about a loaded file's content.
#[derive(Debug)]
pub enum Secrecy {
    Plain,
    /// Classified secret-bearing and dotenv-shaped: serve it redacted.
    /// Carries the parse so callers never re-derive it. Classified files
    /// that are *not* dotenv-shaped never load at all — `load_text`
    /// refuses them with `classified_opaque` (D-2).
    ClassifiedDotenv {
        rule: String,
        file: dotenv::DotenvFile,
    },
}

/// A file loaded through the jail: resolved location, exact content, and
/// the version of the bytes served.
#[derive(Debug)]
pub struct Loaded {
    pub abs: PathBuf,
    pub content: String,
    pub version: String,
    /// Unix permission bits, for mode preservation on edit.
    pub mode: u32,
    pub secrecy: Secrecy,
}

/// Load a file through the jail as UTF-8 text, enforcing size, binary,
/// marker, and classification rules. The version is over the raw bytes,
/// usable as an edit base. **This is the choke point for content-level
/// policy**: a caller holding a `Loaded` either has a plain file or knows
/// it holds a classified dotenv it must serve redacted.
pub fn load_text(root: &ResolvedRoot, rel: &str, limits: &Limits) -> Result<Loaded> {
    let abs = resolve_existing(root, rel)?;
    let meta = std::fs::metadata(&abs).map_err(|e| crate::perm::map_read_io(root, rel, &abs, e))?;
    if meta.is_dir() {
        return Err(KaedError::invalid_input(format!("{rel}: is a directory")));
    }
    if meta.len() > limits.max_file_bytes {
        return Err(KaedError::too_large(format!(
            "{rel}: {} bytes exceeds max_file_bytes {}",
            meta.len(),
            limits.max_file_bytes
        )));
    }
    // The #1091 case: discoverable-then-opaque. `list` showed this file and
    // its size; opening it is where the OS says no, and it must say so with
    // a path, a reason and a route — not `internal: os error 13`.
    let bytes = std::fs::read(&abs).map_err(|e| crate::perm::map_read_io(root, rel, &abs, e))?;
    let version = version_of(&bytes);
    let classified = root.classify.classified_by(&abs);
    let text = if looks_binary(&bytes) {
        Err(KaedError::is_binary(format!("{rel}: binary file")))
    } else {
        String::from_utf8(bytes)
            .map_err(|_| KaedError::is_binary(format!("{rel}: not valid UTF-8")))
    };
    let content = match (text, &classified) {
        (Ok(c), _) => c,
        // a classified binary (.kdbx, .p12) gets the refusal that explains,
        // not a bare is_binary
        (Err(_), Some(rule)) => return Err(classified_opaque(rel, rule)),
        (Err(e), None) => return Err(e),
    };
    if policy::has_ignore_marker(&content) {
        return Err(KaedError::refused(
            rel,
            "in-file `kaedignore` marker",
            RefusalReason::InFileMarker,
            "the file opts out of kaed via a `kaedignore` comment in its first 5 lines; \
             edit it outside kaed if that is wrong",
        ));
    }
    let secrecy = match classified {
        None => Secrecy::Plain,
        Some(rule) => match dotenv::parse(&content) {
            Some(file) => Secrecy::ClassifiedDotenv { rule, file },
            None => return Err(classified_opaque(rel, &rule)),
        },
    };
    Ok(Loaded {
        abs,
        content,
        version,
        mode: meta.permissions().mode(),
        secrecy,
    })
}

fn classified_opaque(rel: &str, rule: &str) -> KaedError {
    KaedError::refused(
        rel,
        rule,
        RefusalReason::ClassifiedOpaque,
        "classified as secret-bearing and not dotenv-shaped, so kaed has no redacted \
         surface for it; if the content is genuinely needed, use your shell — and \
         consider whether a reference to the file would do instead",
    )
}

/// The hint carried on every redacted read: the common paths that feel
/// like they need plaintext usually don't (shipped without `reveal` to
/// measure how much pressure for it actually materialises — PD-1/#1051).
const USAGE_HINT: &str = "values are sealed placeholders; you rarely need plaintext. To *use* a value in a \
     shell: `set -a; . <file>; set +a`, then reference $KEY. To *edit*, use the env ops \
     (env_set / env_rename / env_delete / env_reorder) — a placeholder passed as a value \
     is substituted with the real value on write.";

pub fn read(
    root: &ResolvedRoot,
    rel: &str,
    mode: &ReadMode,
    numbered: bool,
    max_bytes: Option<usize>,
    limits: &Limits,
) -> Result<ReadResult> {
    let loaded = load_text(root, rel, limits)?;
    match &loaded.secrecy {
        Secrecy::Plain => slice_lines(
            &loaded.content,
            rel,
            &loaded.version,
            mode,
            numbered,
            max_bytes,
            limits,
        ),
        Secrecy::ClassifiedDotenv { file, .. } => {
            // the redacted rendering is line-for-line with the raw file
            // (D-7), so every read mode works over it unchanged; the
            // version stays the raw file's — it is the edit base
            let redacted = file.redact();
            let mut out = slice_lines(
                &redacted,
                rel,
                &loaded.version,
                mode,
                numbered,
                max_bytes,
                limits,
            )?;
            out.redacted = true;
            if matches!(mode, ReadMode::Whole) {
                out.dotenv = Some(file.entries());
            }
            out.usage_hint = Some(USAGE_HINT);
            out.warnings
                .extend(policy::gitignore_warning(&loaded.abs, rel));
            Ok(out)
        }
    }
}

/// The shaped-read engine: range/window/budget math over `content`, which
/// is either a file's raw text or its redacted rendering. `rel` is for
/// error messages only.
fn slice_lines(
    content: &str,
    rel: &str,
    version: &str,
    mode: &ReadMode,
    numbered: bool,
    max_bytes: Option<usize>,
    limits: &Limits,
) -> Result<ReadResult> {
    let version = version.to_owned();
    let budget = max_bytes
        .unwrap_or(limits.max_read_bytes)
        .min(limits.max_read_bytes);

    // byte span of each line, terminator included, so slices are byte-exact
    let spans: Vec<(usize, usize)> = {
        let mut spans = Vec::new();
        let mut off = 0;
        for seg in content.split_inclusive('\n') {
            spans.push((off, off + seg.len()));
            off += seg.len();
        }
        spans
    };
    let total_lines = spans.len();

    let (start, end) = match *mode {
        ReadMode::Whole => (1, total_lines),
        ReadMode::Range { start, end } => {
            if start == 0 || end < start {
                return Err(KaedError::invalid_input(format!(
                    "invalid range {start}..{end}: lines are 1-based and ranges inclusive"
                )));
            }
            if start > total_lines {
                return Err(KaedError::invalid_input(format!(
                    "range starts at line {start} but {rel} has {total_lines} lines"
                )));
            }
            (start, end.min(total_lines))
        }
        ReadMode::WindowLine { line, context } => {
            if line == 0 || line > total_lines {
                return Err(KaedError::invalid_input(format!(
                    "line {line} out of range: {rel} has {total_lines} lines"
                )));
            }
            window(line, context, total_lines)
        }
        ReadMode::WindowAnchor { anchor, context } => {
            let hit = addr::resolve_anchor(content, anchor, None, rel)?;
            window(hit.line, context, total_lines)
        }
    };

    // empty file: nothing to slice, range collapses to 0..0
    if total_lines == 0 {
        return Ok(ReadResult {
            content: String::new(),
            version,
            range: LineRange { start: 0, end: 0 },
            total_lines: 0,
            truncated: false,
            next_offset: None,
            redacted: false,
            dotenv: None,
            usage_hint: None,
            warnings: Vec::new(),
        });
    }

    // apply the byte budget line-wise; never serve a partial line unless
    // even the first line alone overflows the budget
    let mut included_end = start - 1; // last included line, 0 = none yet
    let mut used = 0;
    for line_no in start..=end {
        let (s, e) = spans[line_no - 1];
        if used + (e - s) > budget && line_no > start {
            break;
        }
        used += e - s;
        included_end = line_no;
        if used > budget {
            break; // first line alone overflowed; it gets cut below
        }
    }

    let slice_start = spans[start - 1].0;
    let slice_end = spans[included_end - 1].1;
    let mut body = &content[slice_start..slice_end];
    let mut cut_mid_line = false;
    if body.len() > budget {
        let mut cut = budget;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body = &body[..cut];
        cut_mid_line = true;
    }

    let truncated = included_end < end || cut_mid_line;
    let out = if numbered {
        let mut s = String::with_capacity(body.len() + 8 * (included_end - start + 1));
        for (i, seg) in body.split_inclusive('\n').enumerate() {
            s.push_str(&format!("{}\t{}", start + i, seg));
        }
        s
    } else {
        body.to_owned()
    };

    Ok(ReadResult {
        content: out,
        version,
        range: LineRange {
            start,
            end: included_end,
        },
        total_lines,
        truncated,
        next_offset: (included_end < end && !cut_mid_line).then_some(included_end + 1),
        redacted: false,
        dotenv: None,
        usage_hint: None,
        warnings: Vec::new(),
    })
}

fn window(line: usize, context: usize, total: usize) -> (usize, usize) {
    (
        line.saturating_sub(context).max(1),
        (line + context).min(total),
    )
}

// --------------------------------------------------------------- staging

/// A written-but-not-promoted temp file. The txn engine stages every file
/// in a transaction, then promotes all or discards all.
#[derive(Debug)]
pub struct StagedFile {
    pub tmp: PathBuf,
    pub dest: PathBuf,
}

static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Write `content` to a temp file alongside `dest` (same filesystem, so
/// rename is atomic) and fsync it. Creates missing parent directories.
pub fn stage(dest: &Path, content: &[u8], mode: u32) -> Result<StagedFile> {
    let parent = dest
        .parent()
        .ok_or_else(|| KaedError::internal(format!("{}: no parent", dest.display())))?;
    std::fs::create_dir_all(parent)?;
    let name = dest
        .file_name()
        .ok_or_else(|| KaedError::internal(format!("{}: no file name", dest.display())))?;
    let tmp = parent.join(format!(
        ".{}.kaed-tmp.{}.{}",
        name.to_string_lossy(),
        std::process::id(),
        STAGE_SEQ.fetch_add(1, Ordering::Relaxed),
    ));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)?;
    f.write_all(content)?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    f.sync_all()?;
    Ok(StagedFile {
        tmp,
        dest: dest.to_path_buf(),
    })
}

/// Rename the staged temp over its destination and fsync the directory.
pub fn promote(staged: &StagedFile) -> Result<()> {
    std::fs::rename(&staged.tmp, &staged.dest)?;
    fsync_dir(staged.dest.parent().expect("dest has a parent"))?;
    Ok(())
}

pub fn discard(staged: &StagedFile) {
    let _ = std::fs::remove_file(&staged.tmp);
}

pub fn fsync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;

    fn test_root(dir: &Path) -> ResolvedRoot {
        ResolvedRoot::unrestricted("t", dir.canonicalize().unwrap())
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// A root whose deny list carries `globs`, as `Config::resolve` builds it.
    fn denied_root(dir: &Path, globs: &[&str]) -> ResolvedRoot {
        ResolvedRoot {
            deny: Arc::new(
                crate::deny::DenyList::new(
                    Vec::new(),
                    &globs.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                )
                .unwrap(),
            ),
            ..test_root(dir)
        }
    }

    #[test]
    fn version_is_16_hex() {
        let v = version_of(b"hello");
        assert_eq!(v.len(), 16);
        assert!(v.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(v, version_of(b"hello2"));
    }

    #[test]
    fn jail_rejects_absolute_and_dotdot() {
        let dir = tempfile::tempdir().unwrap();
        let root = test_root(dir.path());
        for bad in ["/etc/passwd", "../x", "a/../../x", "a/../.."] {
            let err = resolve_existing(&root, bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::OutsideRoot, "{bad}");
        }
    }

    #[test]
    fn jail_rejects_symlink_escape() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "x").unwrap();
        let root = test_root(dir.path());
        std::os::unix::fs::symlink(outside.path(), dir.path().join("leak")).unwrap();
        let err = resolve_existing(&root, "leak/secret").unwrap_err();
        assert_eq!(err.code, ErrorCode::OutsideRoot);
    }

    #[test]
    fn jail_allows_internal_symlink() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real/f.txt", "hi");
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("alias")).unwrap();
        let root = test_root(dir.path());
        assert!(resolve_existing(&root, "alias/f.txt").is_ok());
    }

    #[test]
    fn resolve_creatable_handles_missing_parents() {
        let dir = tempfile::tempdir().unwrap();
        let root = test_root(dir.path());
        let p = resolve_creatable(&root, "a/b/c.txt").unwrap();
        assert!(p.starts_with(&root.path));
        assert!(resolve_creatable(&root, "../x").is_err());
        assert!(resolve_creatable(&root, "").is_err());
    }

    // ------------------------------------------------------- deny list
    //
    // The proposal for #908 called the path resolver "one choke point" for
    // this. It isn't: `list` and `search` enumerate with their own walkers
    // and never call the resolver per entry. These tests pin all three
    // enforcement points, because a regression in the walkers would leak
    // silently — the addressed-path tests would still pass.

    #[test]
    fn addressed_reads_of_a_denied_path_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".config/kaed/env", "KAED_TOKEN_CLAUDE=sekrit\n");
        let root = denied_root(dir.path(), &["**/.config/kaed"]);

        for err in [
            stat(&root, ".config/kaed/env", &Limits::default()).unwrap_err(),
            read(
                &root,
                ".config/kaed/env",
                &ReadMode::Whole,
                false,
                None,
                &Limits::default(),
            )
            .unwrap_err(),
            load_text(&root, ".config/kaed/env", &Limits::default()).unwrap_err(),
            resolve_existing(&root, ".config/kaed/env").unwrap_err(),
            // the directory itself, and a write target inside it
            stat(&root, ".config/kaed", &Limits::default()).unwrap_err(),
            resolve_creatable(&root, ".config/kaed/newfile").unwrap_err(),
        ] {
            assert_eq!(err.code, ErrorCode::Denied, "{}", err.message);
        }
    }

    #[test]
    fn list_hides_denied_entries_and_says_how_many() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        write(dir.path(), ".ssh/id_ed25519", "PRIVATE KEY\n");
        write(dir.path(), ".ssh/known_hosts", "\n");
        write(dir.path(), ".env", "SECRET=1\n");
        let root = denied_root(dir.path(), &["**/.ssh", "**/.env"]);

        let out = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 3,
                max: 100,
                offset: 0,
                ignored: true,
            },
        )
        .unwrap();
        let paths: Vec<_> = out.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["src", "src/main.rs"]);
        // .ssh is pruned as a subtree (one hidden entry), .env is a second
        assert_eq!(out.denied_hidden, 2);
    }

    #[test]
    fn a_symlink_cannot_walk_into_a_denied_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".ssh/id_ed25519", "PRIVATE KEY\n");
        std::os::unix::fs::symlink(dir.path().join(".ssh"), dir.path().join("keys")).unwrap();
        let root = denied_root(dir.path(), &["**/.ssh"]);
        // the name `keys/id_ed25519` matches nothing; the resolved target does
        let err = read(
            &root,
            "keys/id_ed25519",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Denied);
    }

    #[test]
    fn a_denied_path_is_not_an_existence_oracle() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "real.pem", "cert\n");
        let root = denied_root(dir.path(), &["**/*.pem"]);
        let present = stat(&root, "real.pem", &Limits::default()).unwrap_err();
        let absent = stat(&root, "imaginary.pem", &Limits::default()).unwrap_err();
        assert_eq!(present.code, ErrorCode::Denied);
        // same code for a file that isn't there: the check never hit the disk
        assert_eq!(absent.code, ErrorCode::Denied);
    }

    #[test]
    fn stat_file_carries_version_and_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.rs", "fn main() {}\n");
        let root = test_root(dir.path());
        let s = stat(&root, "f.rs", &Limits::default()).unwrap();
        assert_eq!(s.kind, EntryKind::File);
        assert_eq!(s.line_count, Some(1));
        assert_eq!(s.language.as_deref(), Some("rust"));
        assert!(!s.binary);
        assert!(!s.executable);
        assert_eq!(s.version.as_deref().unwrap().len(), 16);
        assert!(s.modified.ends_with('Z'));
    }

    #[test]
    fn stat_detects_binary() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.bin"), b"ab\x00cd").unwrap();
        let root = test_root(dir.path());
        let s = stat(&root, "b.bin", &Limits::default()).unwrap();
        assert!(s.binary);
        assert!(s.version.is_some());
        assert_eq!(s.line_count, None);
    }

    #[test]
    fn stat_symlink_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "hi");
        std::os::unix::fs::symlink("f.txt", dir.path().join("ln")).unwrap();
        let root = test_root(dir.path());
        assert_eq!(
            stat(&root, "ln", &Limits::default()).unwrap().kind,
            EntryKind::Symlink
        );
    }

    #[test]
    fn stat_root_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = test_root(dir.path());
        assert_eq!(
            stat(&root, "", &Limits::default()).unwrap().kind,
            EntryKind::Dir
        );
    }

    #[test]
    fn list_depth_glob_and_pagination() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "");
        write(dir.path(), "b.txt", "");
        write(dir.path(), "sub/c.rs", "");
        let root = test_root(dir.path());

        let all = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 2,
                max: 100,
                offset: 0,
                ignored: false,
            },
        )
        .unwrap();
        let paths: Vec<_> = all.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a.rs", "b.txt", "sub", "sub/c.rs"]);
        assert!(!all.truncated);

        let rs = list(
            &root,
            &ListParams {
                path: "",
                glob: Some("**/*.rs"),
                depth: 2,
                max: 100,
                offset: 0,
                ignored: false,
            },
        )
        .unwrap();
        let paths: Vec<_> = rs.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a.rs", "sub/c.rs"]);

        let page1 = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 2,
                max: 3,
                offset: 0,
                ignored: false,
            },
        )
        .unwrap();
        assert!(page1.truncated);
        assert_eq!(page1.next_offset, Some(3));
        let page2 = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 2,
                max: 3,
                offset: 3,
                ignored: false,
            },
        )
        .unwrap();
        assert_eq!(page2.entries.len(), 1);
        assert!(!page2.truncated);
        // A page past the end is explained by pagination, not by scoping —
        // diagnosing it as a bad glob would be a confident wrong answer.
        assert!(page2.reason.is_none());
    }

    /// `list` has #1066's trap too — the same root-relative `glob`, the same
    /// `path` scoping, and an empty `entries` that reads as "empty directory"
    /// (D-7).
    #[test]
    fn list_explains_an_empty_result_it_caused_itself() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ai/kaed/README.md", "");
        write(dir.path(), "ai/kaed/src/main.rs", "");
        let root = test_root(dir.path());
        let scoped = |glob| ListParams {
            path: "ai/kaed",
            glob,
            depth: 3,
            max: 100,
            offset: 0,
            ignored: false,
        };

        let trap = list(&root, &scoped(Some("README.md"))).unwrap();
        assert!(trap.entries.is_empty());
        assert_eq!(trap.entries_scanned, 3, "src/, src/main.rs, README.md");
        let reason = trap
            .reason
            .expect("an empty listing this shape explains itself");
        assert_eq!(reason.code, "glob_matched_no_files");
        assert!(reason.hint.contains("ai/kaed/README.md"), "{}", reason.hint);

        let fixed = list(&root, &scoped(Some("**/README.md"))).unwrap();
        assert_eq!(fixed.entries.len(), 1);
        assert_eq!(fixed.entries_scanned, 3);
        assert!(fixed.reason.is_none());

        // A directory that really is empty says that instead.
        std::fs::create_dir_all(dir.path().join("void")).unwrap();
        let empty = list(
            &root,
            &ListParams {
                path: "void",
                ..scoped(None)
            },
        )
        .unwrap();
        assert_eq!(empty.entries_scanned, 0);
        assert_eq!(empty.reason.unwrap().code, "no_files_under_path");
    }

    #[test]
    fn list_respects_gitignore_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".gitignore", "ignored.txt\n");
        write(dir.path(), "ignored.txt", "");
        write(dir.path(), "kept.txt", "");
        // .git dir contents are always hidden
        write(dir.path(), ".git/HEAD", "ref: refs/heads/main\n");
        let root = test_root(dir.path());

        let default = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 1,
                max: 100,
                offset: 0,
                ignored: false,
            },
        )
        .unwrap();
        let paths: Vec<_> = default.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, [".gitignore", "kept.txt"]);

        let with_ignored = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 1,
                max: 100,
                offset: 0,
                ignored: true,
            },
        )
        .unwrap();
        let paths: Vec<_> = with_ignored
            .entries
            .iter()
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(paths, [".gitignore", "ignored.txt", "kept.txt"]);
    }

    #[test]
    fn read_whole_is_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "a\nb\nc\n");
        let root = test_root(dir.path());
        let r = read(
            &root,
            "f.txt",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.content, "a\nb\nc\n");
        assert_eq!(r.total_lines, 3);
        assert_eq!((r.range.start, r.range.end), (1, 3));
        assert!(!r.truncated);
        assert_eq!(r.version, version_of(b"a\nb\nc\n"));
    }

    #[test]
    fn read_range_clamps_end_but_rejects_bad_start() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "1\n2\n3\n4\n5\n");
        let root = test_root(dir.path());
        let r = read(
            &root,
            "f.txt",
            &ReadMode::Range { start: 4, end: 99 },
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.content, "4\n5\n");
        assert_eq!((r.range.start, r.range.end), (4, 5));

        let err = read(
            &root,
            "f.txt",
            &ReadMode::Range { start: 9, end: 10 },
            false,
            None,
            &Limits::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn read_window_around_line_and_anchor() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "1\n2\n3\nneedle\n5\n6\n7\n");
        let root = test_root(dir.path());
        let by_line = read(
            &root,
            "f.txt",
            &ReadMode::WindowLine {
                line: 4,
                context: 1,
            },
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(by_line.content, "3\nneedle\n5\n");
        let by_anchor = read(
            &root,
            "f.txt",
            &ReadMode::WindowAnchor {
                anchor: "needle",
                context: 2,
            },
            true,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(by_anchor.content, "2\t2\n3\t3\n4\tneedle\n5\t5\n6\t6\n");
        assert_eq!((by_anchor.range.start, by_anchor.range.end), (2, 6));
    }

    #[test]
    fn read_budget_truncates_at_line_boundary_with_continuation() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "aaaa\nbbbb\ncccc\n");
        let root = test_root(dir.path());
        let r = read(
            &root,
            "f.txt",
            &ReadMode::Whole,
            false,
            Some(11),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.content, "aaaa\nbbbb\n");
        assert!(r.truncated);
        assert_eq!(r.next_offset, Some(3));
    }

    #[test]
    fn read_giant_single_line_is_cut_mid_line() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", &"x".repeat(100));
        let root = test_root(dir.path());
        let r = read(
            &root,
            "f.txt",
            &ReadMode::Whole,
            false,
            Some(10),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.content.len(), 10);
        assert!(r.truncated);
        assert_eq!(r.next_offset, None);
    }

    #[test]
    fn read_rejects_binary_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.bin"), b"a\x00b").unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let root = test_root(dir.path());
        assert_eq!(
            read(
                &root,
                "b.bin",
                &ReadMode::Whole,
                false,
                None,
                &Limits::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::IsBinary
        );
        assert_eq!(
            read(
                &root,
                "d",
                &ReadMode::Whole,
                false,
                None,
                &Limits::default()
            )
            .unwrap_err()
            .code,
            ErrorCode::InvalidInput
        );
    }

    #[test]
    fn read_too_large_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "0123456789");
        let root = test_root(dir.path());
        let limits = Limits {
            max_file_bytes: 5,
            ..Limits::default()
        };
        assert_eq!(
            read(&root, "f.txt", &ReadMode::Whole, false, None, &limits)
                .unwrap_err()
                .code,
            ErrorCode::TooLarge
        );
    }

    #[test]
    fn read_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "f.txt", "");
        let root = test_root(dir.path());
        let r = read(
            &root,
            "f.txt",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.content, "");
        assert_eq!(r.total_lines, 0);
        assert!(!r.truncated);
    }

    #[test]
    fn stage_promote_replaces_atomically_and_discard_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("f.txt");
        std::fs::write(&dest, "old").unwrap();

        let staged = stage(&dest, b"new", 0o644).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "old");
        promote(&staged).unwrap();
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");

        let staged2 = stage(&dest, b"never", 0o644).unwrap();
        discard(&staged2);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "new");
        assert!(!staged2.tmp.exists());
    }

    #[test]
    fn stage_sets_mode_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("deep/nested/run.sh");
        let staged = stage(&dest, b"#!/bin/sh\n", 0o755).unwrap();
        promote(&staged).unwrap();
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    // ------------------------------------------------ secrets model (008)

    const ENV: &str = "# service\nKLAMS_TOKEN=b7f3a9d2c8e14f60b7f3a9d2c8e14f60\nDEBUG=true\n";

    fn classified_root(dir: &Path) -> ResolvedRoot {
        ResolvedRoot::with_default_classify("t", dir.canonicalize().unwrap())
    }

    /// korg #1093's open question, answered by observation rather than
    /// assumption, and pinned so the answer cannot drift silently.
    ///
    /// kubsdb's five secret-bearing service files are **not** dotenv-shaped
    /// — a YAML compose file and a TOML-ish `up.conf` — and 008's redaction
    /// surface is dotenv-typed. The expected and desired outcome was
    /// `classified_opaque`: refused with a reason, no attempted redaction
    /// that might pass a value through. It is what happens, and the reason
    /// is structural: the strict grammar needs every line to be blank, a
    /// `#` comment, or `KEY=value` at column 0, and both shapes open with a
    /// line (`services:`, `[unifi.defaults]`) that is none of those.
    #[test]
    fn kubsdbs_secret_bearing_service_files_classify_opaque_not_redacted() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "docker-compose.yml",
            concat!(
                "services:\n",
                "  postgres:\n",
                "    image: postgres:16\n",
                "    environment:\n",
                "      - POSTGRES_PASSWORD=b7f3a9d2c8e14f60b7f3a9d2c8e14f60\n",
            ),
        );
        write(
            dir.path(),
            "up.conf",
            concat!(
                "[unifi.defaults]\n",
                "  url = \"https://unifi.example\"\n",
                "  user = \"unpoller\"\n",
                "  pass = \"b7f3a9d2c8e14f60b7f3a9d2c8e14f60\"\n",
            ),
        );
        // Neither name matches DEFAULT_CLASSIFY: on kubsdb these are
        // covered by explicit `[security] classify` globs, which is the
        // whole point of the item.
        let root = ResolvedRoot {
            classify: Arc::new(
                crate::policy::Classifier::new(&[
                    "**/docker-compose.yml".to_string(),
                    "**/up.conf".to_string(),
                ])
                .unwrap(),
            ),
            ..ResolvedRoot::unrestricted("t", dir.path().canonicalize().unwrap())
        };

        for path in ["docker-compose.yml", "up.conf"] {
            let err = load_text(&root, path, &Limits::default()).unwrap_err();
            let v = serde_json::to_value(&err).unwrap();
            assert_eq!(v["code"], "denied", "{path}");
            assert_eq!(v["data"]["reason"], "classified_opaque", "{path}");
            assert!(
                !err.message.contains("b7f3a9d2c8e14f60"),
                "{path}: the refusal must not quote the file: {}",
                err.message
            );
        }

        // The value probe (008 D-8) over the same files: an opaque
        // classified file is skipped whole, so the value is unreachable by
        // search as well as by read.
        let hits = crate::search::search(
            &root,
            &crate::search::SearchParams {
                pattern: "b7f3a9d2c8e14f60",
                regex: false,
                glob: None,
                path: "",
                context: 0,
                max_results: 50,
            },
            &Limits::default(),
        )
        .unwrap();
        assert!(hits.matches.is_empty(), "{:?}", hits.matches);
        assert_eq!(hits.classified_hidden, 2);
    }

    #[test]
    fn read_of_a_classified_dotenv_is_redacted_and_carries_the_raw_version() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".env", ENV);
        let root = classified_root(dir.path());
        let r = read(
            &root,
            ".env",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert!(r.redacted);
        assert!(
            !r.content.contains("b7f3a9d2c8e14f60b7f3a9d2c8e14f60"),
            "value leaked: {}",
            r.content
        );
        assert!(r.content.contains("⟨kaed:KLAMS_TOKEN@"), "{}", r.content);
        assert!(r.content.contains("⟨kaed:DEBUG⟩"), "{}", r.content);
        // R1: the version is a content address of the bytes on disk — the
        // redacted view has no identity of its own, and this version is
        // directly usable as an edit base
        assert_eq!(r.version, version_of(ENV.as_bytes()));
        assert_eq!(r.total_lines, 3);
        let entries = r.dotenv.expect("whole-file read carries the typed view");
        assert_eq!(entries[0].key, "KLAMS_TOKEN");
        assert_eq!(entries[0].meta.shape, "hex");
        assert!(r.usage_hint.unwrap().contains("set -a"));
    }

    /// D-7: redaction is line-preserving, so shaped reads keep meaning.
    #[test]
    fn shaped_reads_of_redacted_content_keep_line_numbers() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".env", ENV);
        let root = classified_root(dir.path());
        let r = read(
            &root,
            ".env",
            &ReadMode::Range { start: 2, end: 2 },
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert!(r.redacted);
        assert!(r.content.starts_with("KLAMS_TOKEN=⟨kaed:"), "{}", r.content);
        assert_eq!((r.range.start, r.range.end), (2, 2));
        assert!(r.dotenv.is_none(), "typed view only on whole-file reads");

        // anchors resolve against the redacted text (which is what the
        // agent saw); "DEBUG" alone would be ambiguous — it appears in the
        // key AND inside its own placeholder ⟨kaed:DEBUG⟩
        let by_anchor = read(
            &root,
            ".env",
            &ReadMode::WindowAnchor {
                anchor: "DEBUG=",
                context: 0,
            },
            false,
            None,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!((by_anchor.range.start, by_anchor.range.end), (3, 3));
    }

    /// D-5: every refusal names its policy layer and what to do instead.
    #[test]
    fn refusals_carry_their_reason_and_a_hint() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "opted-out.txt", "# kaedignore\ncontent\n");
        write(
            dir.path(),
            "server.pem",
            "-----BEGIN CERTIFICATE-----\nMIIB\n",
        );
        write(dir.path(), ".kaedignore", "private/\n");
        write(dir.path(), "private/notes.md", "x");
        let root = classified_root(dir.path());

        let reason_of = |err: &KaedError| {
            let d = err.data.as_ref().unwrap();
            (
                d["reason"].as_str().unwrap().to_owned(),
                d["hint"].as_str().unwrap().to_owned(),
            )
        };

        let marker = read(
            &root,
            "opted-out.txt",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap_err();
        assert_eq!(marker.code, crate::errors::ErrorCode::Denied);
        let (reason, hint) = reason_of(&marker);
        assert_eq!(reason, "in_file_marker");
        assert!(hint.contains("first 5 lines"), "{hint}");

        let opaque = read(
            &root,
            "server.pem",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap_err();
        let (reason, hint) = reason_of(&opaque);
        assert_eq!(reason, "classified_opaque");
        assert!(hint.contains("shell"), "{hint}");

        let ignored = read(
            &root,
            "private/notes.md",
            &ReadMode::Whole,
            false,
            None,
            &Limits::default(),
        )
        .unwrap_err();
        let (reason, hint) = reason_of(&ignored);
        assert_eq!(reason, "kaedignore");
        assert!(hint.contains(".kaedignore"), "{hint}");

        // …and the policy file itself stays readable, so the hint works
        assert!(
            read(
                &root,
                ".kaedignore",
                &ReadMode::Whole,
                false,
                None,
                &Limits::default()
            )
            .is_ok()
        );
    }

    #[test]
    fn stat_reports_classification_without_opening() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".env", ENV);
        write(dir.path(), "plain.txt", "x\n");
        let root = classified_root(dir.path());
        assert!(stat(&root, ".env", &Limits::default()).unwrap().classified);
        assert!(
            !stat(&root, "plain.txt", &Limits::default())
                .unwrap()
                .classified
        );
    }

    #[test]
    fn list_hides_kaedignore_denied_entries_and_counts_them() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), ".kaedignore", "private/\n");
        write(dir.path(), "private/journal.md", "x");
        write(dir.path(), "kept.txt", "x");
        let root = classified_root(dir.path());
        let out = list(
            &root,
            &ListParams {
                path: "",
                glob: None,
                depth: 3,
                max: 100,
                offset: 0,
                ignored: true,
            },
        )
        .unwrap();
        let paths: Vec<_> = out.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, [".kaedignore", "kept.txt"]);
        assert_eq!(out.denied_hidden, 1, "the pruned subtree counts once");
    }
}
