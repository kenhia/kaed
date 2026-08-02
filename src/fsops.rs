//! Filesystem layer: path jailing, content versions, stat/list/read, and
//! the staging primitives the transaction engine builds on.
//!
//! Disk is the source of truth — nothing here caches content. Every read
//! hashes what it serves (R1); every path from the wire goes through the
//! jail before it touches the filesystem.

use crate::addr;
use crate::config::{Limits, ResolvedRoot};
use crate::errors::{KaedError, Result};
use schemars::JsonSchema;
use serde::Serialize;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Resolve a path that must already exist. Canonicalizes (so symlinks
/// resolve) and requires the result to stay inside the root.
pub fn resolve_existing(root: &ResolvedRoot, rel: &str) -> Result<PathBuf> {
    let joined = root.path.join(clean_rel(rel)?);
    let canonical = joined.canonicalize().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => KaedError::not_found(format!("{rel}: not found")),
        _ => KaedError::internal(format!("{rel}: {e}")),
    })?;
    if !canonical.starts_with(&root.path) {
        return Err(KaedError::outside_root(format!(
            "{rel:?} resolves outside root {:?}",
            root.name
        )));
    }
    Ok(canonical)
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

    let mut entries = Vec::new();
    let walker = ignore::WalkBuilder::new(&base)
        .max_depth(Some(p.depth))
        .hidden(false)
        .git_ignore(!p.ignored)
        .git_global(!p.ignored)
        .git_exclude(!p.ignored)
        .ignore(!p.ignored)
        .parents(!p.ignored)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker {
        let entry = entry.map_err(|e| KaedError::internal(e.to_string()))?;
        if entry.depth() == 0 {
            continue; // the listed dir itself
        }
        let abs = entry.path();
        let rel_to_root = abs
            .strip_prefix(&root.path)
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned();
        if let Some(m) = &matcher
            && !m.is_match(&rel_to_root)
        {
            continue;
        }
        let meta = entry
            .metadata()
            .map_err(|e| KaedError::internal(e.to_string()))?;
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
    Ok(ListResult {
        next_offset: truncated.then_some(p.offset + page.len()),
        entries: page,
        truncated,
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
    pub version: String,
    /// The lines actually returned (1-based, inclusive).
    pub range: LineRange,
    pub total_lines: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
}

pub enum ReadMode<'a> {
    Whole,
    Range { start: usize, end: usize },
    WindowLine { line: usize, context: usize },
    WindowAnchor { anchor: &'a str, context: usize },
}

/// Load a file through the jail as UTF-8 text, enforcing size and binary
/// rules. The version is over the raw bytes, usable as an edit base.
pub fn load_text(root: &ResolvedRoot, rel: &str, limits: &Limits) -> Result<(String, String)> {
    let abs = resolve_existing(root, rel)?;
    let meta = std::fs::metadata(&abs)?;
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
    let bytes = std::fs::read(&abs)?;
    if looks_binary(&bytes) {
        return Err(KaedError::is_binary(format!("{rel}: binary file")));
    }
    let version = version_of(&bytes);
    let content = String::from_utf8(bytes)
        .map_err(|_| KaedError::is_binary(format!("{rel}: not valid UTF-8")))?;
    Ok((content, version))
}

pub fn read(
    root: &ResolvedRoot,
    rel: &str,
    mode: &ReadMode,
    numbered: bool,
    max_bytes: Option<usize>,
    limits: &Limits,
) -> Result<ReadResult> {
    let (content, version) = load_text(root, rel, limits)?;
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
            let hit = addr::resolve_anchor(&content, anchor, None, rel)?;
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
        ResolvedRoot {
            name: "t".into(),
            path: dir.canonicalize().unwrap(),
            description: None,
        }
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
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
}
