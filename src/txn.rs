//! The edit engine (R2): one transactional `edit`, all addressing modes.
//!
//! Every path a non-create op touches must be declared in `base` with its
//! version; extra base entries act as "assert unchanged". All ops apply or
//! none do. Apply order: verify every base version under the transaction
//! lock, run ops in order against evolving in-memory buffers, stage temp
//! files, journal the transaction, rename all, mark it complete.
//!
//! One global transaction lock serializes appliers in v0 — per-file locks
//! are an optimization to buy when contention is observed, not before.
//! External writers (humans, git) can still race between verify and
//! rename; that window is inherent to optimistic versioning.

use crate::addr::{self, Lines};
use crate::config::{Limits, ResolvedRoot};
use crate::errors::{KaedError, Result, VersionConflictData};
use crate::fsops;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

static TXN_LOCK: Mutex<()> = Mutex::new(());

const CREATE_MODE: u32 = 0o644;
const CREATE_MODE_EXEC: u32 = 0o755;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BaseVersion {
    pub path: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOp {
    /// Replace one unique occurrence of `old_text`.
    AnchorReplace {
        path: String,
        old_text: String,
        new_text: String,
        /// 1-based pick when `old_text` matches more than once.
        #[serde(default)]
        occurrence: Option<usize>,
    },
    /// Replace inclusive 1-based lines `start..=end`, numbered against the
    /// buffer as previous ops left it.
    RangeReplace {
        path: String,
        start: usize,
        end: usize,
        new_text: String,
    },
    /// Create a file (parent directories are created as needed).
    Create {
        path: String,
        content: String,
        #[serde(default)]
        executable: bool,
        #[serde(default)]
        overwrite: bool,
    },
}

impl EditOp {
    fn path(&self) -> &str {
        match self {
            EditOp::AnchorReplace { path, .. }
            | EditOp::RangeReplace { path, .. }
            | EditOp::Create { path, .. } => path,
        }
    }
}

#[derive(Debug)]
pub struct EditRequest {
    pub base: Vec<BaseVersion>,
    pub ops: Vec<EditOp>,
    pub dry_run: bool,
    pub return_diff: bool,
    pub intent: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FileChange {
    pub path: String,
    /// Absent for created files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_version: Option<String>,
    pub new_version: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EditOutcome {
    /// Absent on dry runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txn_id: Option<i64>,
    pub files: Vec<FileChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// `false` means dry run: nothing touched disk.
    pub applied: bool,
}

/// What the journal needs about one file in a transaction.
pub struct FileTxnRecord<'a> {
    pub path: &'a str,
    pub old_version: Option<&'a str>,
    pub new_version: &'a str,
    pub old_content: Option<&'a str>,
    pub new_content: &'a str,
}

/// Journal hook. `begin` is called after staging and before any rename —
/// an interrupted transaction is thus always detectable; `complete` after
/// every rename landed.
pub trait TxnRecorder: Sync {
    fn begin(
        &self,
        author: &str,
        intent: Option<&str>,
        root: &ResolvedRoot,
        files: &[FileTxnRecord<'_>],
    ) -> Result<i64>;
    fn complete(&self, txn_id: i64) -> Result<()>;
    /// Content for a version the store still retains, for conflict deltas.
    fn blob(&self, _version: &str) -> Option<String> {
        None
    }
}

/// Recorder for tests and journal-less operation.
pub struct NoopRecorder;

impl TxnRecorder for NoopRecorder {
    fn begin(
        &self,
        _author: &str,
        _intent: Option<&str>,
        _root: &ResolvedRoot,
        _files: &[FileTxnRecord<'_>],
    ) -> Result<i64> {
        Ok(0)
    }
    fn complete(&self, _txn_id: i64) -> Result<()> {
        Ok(())
    }
}

/// One file's evolving state inside the transaction.
struct FileBuf {
    abs: PathBuf,
    content: String,
    old_content: Option<String>,
    old_version: Option<String>,
    /// Mode to write with: preserved from the original, or the create mode.
    mode: u32,
    touched: bool,
}

pub fn apply(
    root: &ResolvedRoot,
    req: &EditRequest,
    limits: &Limits,
    author: &str,
    recorder: &dyn TxnRecorder,
) -> Result<EditOutcome> {
    if req.ops.is_empty() {
        return Err(KaedError::invalid_input("edit requires at least one op"));
    }

    let _guard = TXN_LOCK.lock().expect("txn lock never poisoned");

    // Load and verify every declared base under the lock (R2). A declared
    // file that vanished is reported as a conflict, not a bare not_found —
    // the agent's world model is stale either way.
    let mut bufs: BTreeMap<String, FileBuf> = BTreeMap::new();
    for b in &req.base {
        if bufs.contains_key(&b.path) {
            return Err(KaedError::invalid_input(format!(
                "duplicate base entry for {}",
                b.path
            )));
        }
        let loaded = fsops::load_text(root, &b.path, limits).map_err(|e| {
            if e.code == crate::errors::ErrorCode::NotFound {
                KaedError::version_conflict(VersionConflictData {
                    path: b.path.clone(),
                    expected_version: b.version.clone(),
                    actual_version: "absent".into(),
                    delta: "(file no longer exists)".into(),
                })
            } else {
                e
            }
        })?;
        if loaded.version != b.version {
            let delta = match recorder.blob(&b.version) {
                Some(expected_content) => unified_diff(&expected_content, &loaded.content, &b.path),
                None => {
                    "(content for the expected version is not retained; re-read the file)".into()
                }
            };
            return Err(KaedError::version_conflict(VersionConflictData {
                path: b.path.clone(),
                expected_version: b.version.clone(),
                actual_version: loaded.version,
                delta,
            }));
        }
        bufs.insert(
            b.path.clone(),
            FileBuf {
                abs: loaded.abs,
                old_content: Some(loaded.content.clone()),
                content: loaded.content,
                old_version: Some(loaded.version),
                mode: loaded.mode,
                touched: false,
            },
        );
    }

    // Run ops in order against the evolving buffers.
    for op in &req.ops {
        let path = op.path();
        match op {
            EditOp::Create {
                path,
                content,
                executable,
                overwrite,
            } => {
                if let Some(buf) = bufs.get_mut(path) {
                    // exists in this txn: base-declared, or created earlier
                    if !overwrite {
                        return Err(KaedError::invalid_input(format!(
                            "{path}: already exists; pass overwrite to replace it"
                        )));
                    }
                    buf.content = content.clone();
                    buf.touched = true;
                    if *executable {
                        buf.mode = CREATE_MODE_EXEC;
                    }
                } else {
                    let abs = fsops::resolve_creatable(root, path)?;
                    let mode = if *executable {
                        CREATE_MODE_EXEC
                    } else {
                        CREATE_MODE
                    };
                    if abs.exists() {
                        if !overwrite {
                            return Err(KaedError::invalid_input(format!(
                                "{path}: already exists; pass overwrite to replace it, \
                                 or declare it in base and edit it"
                            )));
                        }
                        // overwriting an existing file: load it so the old
                        // version and content are journaled (binary refusal
                        // comes along for free)
                        let loaded = fsops::load_text(root, path, limits)?;
                        bufs.insert(
                            path.clone(),
                            FileBuf {
                                abs: loaded.abs,
                                old_content: Some(loaded.content),
                                content: content.clone(),
                                old_version: Some(loaded.version),
                                mode,
                                touched: true,
                            },
                        );
                    } else {
                        bufs.insert(
                            path.clone(),
                            FileBuf {
                                abs,
                                old_content: None,
                                content: content.clone(),
                                old_version: None,
                                mode,
                                touched: true,
                            },
                        );
                    }
                }
            }
            EditOp::AnchorReplace {
                path,
                old_text,
                new_text,
                occurrence,
            } => {
                let buf = declared(&mut bufs, path)?;
                let hit = addr::resolve_anchor(&buf.content, old_text, *occurrence, path)?;
                buf.content
                    .replace_range(hit.byte_offset..hit.byte_offset + old_text.len(), new_text);
                buf.touched = true;
            }
            EditOp::RangeReplace {
                path,
                start,
                end,
                new_text,
            } => {
                let buf = declared(&mut bufs, path)?;
                let mut lines = Lines::split(&buf.content);
                lines
                    .replace_range(*start, *end, new_text)
                    .map_err(|e| KaedError::new(e.code, format!("{path}: {}", e.message)))?;
                buf.content = lines.join();
                buf.touched = true;
            }
        }
        let buf = &bufs[path];
        if buf.content.len() as u64 > limits.max_file_bytes {
            return Err(KaedError::too_large(format!(
                "{path}: edit result ({} bytes) exceeds max_file_bytes {}",
                buf.content.len(),
                limits.max_file_bytes
            )));
        }
    }

    // Outcome for every touched file, in path order.
    let touched: Vec<(&String, &FileBuf)> = bufs.iter().filter(|(_, b)| b.touched).collect();
    let files: Vec<FileChange> = touched
        .iter()
        .map(|(path, buf)| FileChange {
            path: (*path).clone(),
            old_version: buf.old_version.clone(),
            new_version: fsops::version_of(buf.content.as_bytes()),
        })
        .collect();
    let diff = req.return_diff.then(|| {
        touched
            .iter()
            .map(|(path, buf)| {
                unified_diff(buf.old_content.as_deref().unwrap_or(""), &buf.content, path)
            })
            .collect::<Vec<_>>()
            .join("")
    });

    if req.dry_run {
        return Ok(EditOutcome {
            txn_id: None,
            files,
            diff,
            applied: false,
        });
    }

    // Stage everything before any rename; abort cleanly on any failure.
    let mut staged = Vec::new();
    for (_, buf) in &touched {
        match fsops::stage(&buf.abs, buf.content.as_bytes(), buf.mode & 0o7777) {
            Ok(s) => staged.push(s),
            Err(e) => {
                for s in &staged {
                    fsops::discard(s);
                }
                return Err(e);
            }
        }
    }

    let records: Vec<FileTxnRecord<'_>> = touched
        .iter()
        .zip(&files)
        .map(|((path, buf), change)| FileTxnRecord {
            path,
            old_version: buf.old_version.as_deref(),
            new_version: &change.new_version,
            old_content: buf.old_content.as_deref(),
            new_content: &buf.content,
        })
        .collect();
    let txn_id = match recorder.begin(author, req.intent.as_deref(), root, &records) {
        Ok(id) => id,
        Err(e) => {
            for s in &staged {
                fsops::discard(s);
            }
            return Err(e);
        }
    };

    // Renames: after the first promote there is no rollback — a failure
    // here leaves the journal entry pending, which is the torn-txn signal
    // startup detection looks for.
    for s in &staged {
        fsops::promote(s)?;
    }
    recorder.complete(txn_id)?;

    Ok(EditOutcome {
        txn_id: Some(txn_id),
        files,
        diff,
        applied: true,
    })
}

fn declared<'a>(bufs: &'a mut BTreeMap<String, FileBuf>, path: &str) -> Result<&'a mut FileBuf> {
    bufs.get_mut(path).ok_or_else(|| {
        KaedError::invalid_input(format!(
            "{path}: not declared in base (every edited file needs its version there)"
        ))
    })
}

/// Unified diff with the file's path in the headers — the edit's proof of
/// what changed (and the conflict delta).
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;
    use std::path::Path;

    fn setup() -> (tempfile::TempDir, ResolvedRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot {
            name: "t".into(),
            path: dir.path().canonicalize().unwrap(),
            description: None,
        };
        (dir, root)
    }

    fn write(dir: &Path, rel: &str, content: &str) -> String {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        fsops::version_of(content.as_bytes())
    }

    fn req(base: Vec<BaseVersion>, ops: Vec<EditOp>) -> EditRequest {
        EditRequest {
            base,
            ops,
            dry_run: false,
            return_diff: true,
            intent: None,
        }
    }

    fn base(path: &str, version: &str) -> BaseVersion {
        BaseVersion {
            path: path.into(),
            version: version.into(),
        }
    }

    fn apply_noop(root: &ResolvedRoot, r: &EditRequest) -> Result<EditOutcome> {
        apply(root, r, &Limits::default(), "test", &NoopRecorder)
    }

    #[test]
    fn anchor_replace_applies_and_proves() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "fn old() {}\nrest\n");
        let out = apply_noop(
            &root,
            &req(
                vec![base("f.txt", &v)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "fn old()".into(),
                    new_text: "fn new()".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "fn new() {}\nrest\n"
        );
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].old_version.as_deref(), Some(v.as_str()));
        assert_eq!(
            out.files[0].new_version,
            fsops::version_of(b"fn new() {}\nrest\n")
        );
        let diff = out.diff.unwrap();
        assert!(diff.contains("-fn old() {}"));
        assert!(diff.contains("+fn new() {}"));
        assert!(diff.contains("a/f.txt"));
    }

    #[test]
    fn ambiguous_anchor_applies_nothing() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "dup\ndup\n");
        let err = apply_noop(
            &root,
            &req(
                vec![base("f.txt", &v)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "dup".into(),
                    new_text: "x".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousAnchor);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "dup\ndup\n"
        );
    }

    #[test]
    fn occurrence_disambiguates() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "dup\ndup\n");
        apply_noop(
            &root,
            &req(
                vec![base("f.txt", &v)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "dup".into(),
                    new_text: "x".into(),
                    occurrence: Some(2),
                }],
            ),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "dup\nx\n"
        );
    }

    #[test]
    fn range_replace_applies() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "1\n2\n3\n4\n");
        apply_noop(
            &root,
            &req(
                vec![base("f.txt", &v)],
                vec![EditOp::RangeReplace {
                    path: "f.txt".into(),
                    start: 2,
                    end: 3,
                    new_text: "two\nthree".into(),
                }],
            ),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "1\ntwo\nthree\n4\n"
        );
    }

    #[test]
    fn create_with_parents_and_exec_bit() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        let out = apply_noop(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "deep/nested/run.sh".into(),
                    content: "#!/bin/sh\n".into(),
                    executable: true,
                    overwrite: false,
                }],
            ),
        )
        .unwrap();
        let p = dir.path().join("deep/nested/run.sh");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "#!/bin/sh\n");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(out.files[0].old_version, None);
    }

    #[test]
    fn create_refuses_existing_without_overwrite() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "old");
        let err = apply_noop(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "f.txt".into(),
                    content: "new".into(),
                    executable: false,
                    overwrite: false,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        // with overwrite it lands, and the old version is journaled
        let out = apply_noop(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "f.txt".into(),
                    content: "new".into(),
                    executable: false,
                    overwrite: true,
                }],
            ),
        )
        .unwrap();
        assert_eq!(
            out.files[0].old_version.as_deref(),
            Some(fsops::version_of(b"old").as_str())
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn ops_apply_in_order_against_evolving_buffer() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "alpha\n");
        apply_noop(
            &root,
            &req(
                vec![base("f.txt", &v)],
                vec![
                    EditOp::AnchorReplace {
                        path: "f.txt".into(),
                        old_text: "alpha".into(),
                        new_text: "beta".into(),
                        occurrence: None,
                    },
                    // anchors on text the previous op just wrote
                    EditOp::AnchorReplace {
                        path: "f.txt".into(),
                        old_text: "beta".into(),
                        new_text: "gamma".into(),
                        occurrence: None,
                    },
                ],
            ),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "gamma\n"
        );
    }

    #[test]
    fn multi_file_failure_touches_nothing() {
        let (dir, root) = setup();
        let va = write(dir.path(), "a.txt", "aaa\n");
        let vb = write(dir.path(), "b.txt", "bbb\n");
        let err = apply_noop(
            &root,
            &req(
                vec![base("a.txt", &va), base("b.txt", &vb)],
                vec![
                    EditOp::AnchorReplace {
                        path: "a.txt".into(),
                        old_text: "aaa".into(),
                        new_text: "AAA".into(),
                        occurrence: None,
                    },
                    EditOp::AnchorReplace {
                        path: "b.txt".into(),
                        old_text: "zzz".into(), // not present
                        new_text: "ZZZ".into(),
                        occurrence: None,
                    },
                ],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::AnchorNotFound);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "aaa\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "bbb\n"
        );
    }

    #[test]
    fn stale_base_is_a_version_conflict() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "current\n");
        let stale = fsops::version_of(b"what the agent saw\n");
        let err = apply_noop(
            &root,
            &req(
                vec![base("f.txt", &stale)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "current".into(),
                    new_text: "x".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::VersionConflict);
        let data = err.data.unwrap();
        assert_eq!(data["expected_version"], stale);
        assert_eq!(data["actual_version"], fsops::version_of(b"current\n"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "current\n"
        );
    }

    #[test]
    fn conflict_delta_comes_from_blob_store_when_retained() {
        struct BlobRecorder;
        impl TxnRecorder for BlobRecorder {
            fn begin(
                &self,
                _: &str,
                _: Option<&str>,
                _: &ResolvedRoot,
                _: &[FileTxnRecord<'_>],
            ) -> Result<i64> {
                Ok(1)
            }
            fn complete(&self, _: i64) -> Result<()> {
                Ok(())
            }
            fn blob(&self, version: &str) -> Option<String> {
                (version == fsops::version_of(b"old line\n")).then(|| "old line\n".to_string())
            }
        }
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "new line\n");
        let stale = fsops::version_of(b"old line\n");
        let err = apply(
            &root,
            &req(
                vec![base("f.txt", &stale)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "old".into(),
                    new_text: "x".into(),
                    occurrence: None,
                }],
            ),
            &Limits::default(),
            "test",
            &BlobRecorder,
        )
        .unwrap_err();
        let delta = err.data.unwrap()["delta"].as_str().unwrap().to_string();
        assert!(delta.contains("-old line"));
        assert!(delta.contains("+new line"));
    }

    #[test]
    fn vanished_base_file_is_a_version_conflict() {
        let (dir, root) = setup();
        let _ = dir;
        let err = apply_noop(
            &root,
            &req(
                vec![base("gone.txt", "0000000000000000")],
                vec![EditOp::AnchorReplace {
                    path: "gone.txt".into(),
                    old_text: "x".into(),
                    new_text: "y".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::VersionConflict);
        assert_eq!(err.data.unwrap()["actual_version"], "absent");
    }

    #[test]
    fn undeclared_path_is_rejected() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "x\n");
        let err = apply_noop(
            &root,
            &req(
                vec![],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "x".into(),
                    new_text: "y".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("base"));
    }

    #[test]
    fn extra_base_entry_asserts_unchanged() {
        let (dir, root) = setup();
        let va = write(dir.path(), "a.txt", "a\n");
        write(dir.path(), "other.txt", "current\n");
        let stale = fsops::version_of(b"stale\n");
        let err = apply_noop(
            &root,
            &req(
                vec![base("a.txt", &va), base("other.txt", &stale)],
                vec![EditOp::AnchorReplace {
                    path: "a.txt".into(),
                    old_text: "a".into(),
                    new_text: "A".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::VersionConflict);
        // and the edit to a.txt did not land
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\n"
        );
    }

    #[test]
    fn dry_run_proves_without_touching_disk() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "x\n");
        let out = apply(
            &root,
            &EditRequest {
                base: vec![base("f.txt", &v)],
                ops: vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "x".into(),
                    new_text: "y".into(),
                    occurrence: None,
                }],
                dry_run: true,
                return_diff: true,
                intent: None,
            },
            &Limits::default(),
            "test",
            &NoopRecorder,
        )
        .unwrap();
        assert!(!out.applied);
        assert_eq!(out.txn_id, None);
        assert!(out.diff.unwrap().contains("+y"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "x\n"
        );
    }

    #[test]
    fn mode_is_preserved_on_edit() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        let v = write(dir.path(), "run.sh", "#!/bin/sh\nold\n");
        let p = dir.path().join("run.sh");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        // version unchanged by chmod: recompute not needed, content same
        apply_noop(
            &root,
            &req(
                vec![base("run.sh", &v)],
                vec![EditOp::AnchorReplace {
                    path: "run.sh".into(),
                    old_text: "old".into(),
                    new_text: "new".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn binary_target_is_refused() {
        let (dir, root) = setup();
        std::fs::write(dir.path().join("b.bin"), b"a\x00b").unwrap();
        let err = apply_noop(
            &root,
            &req(
                vec![base("b.bin", "whatever")],
                vec![EditOp::AnchorReplace {
                    path: "b.bin".into(),
                    old_text: "a".into(),
                    new_text: "x".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::IsBinary);
    }

    #[test]
    fn oversize_result_is_refused() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "x\n");
        let limits = Limits {
            max_file_bytes: 10,
            ..Limits::default()
        };
        let err = apply(
            &root,
            &req(
                vec![base("f.txt", &v)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "x".into(),
                    new_text: "y".repeat(50),
                    occurrence: None,
                }],
            ),
            &limits,
            "test",
            &NoopRecorder,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::TooLarge);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "x\n"
        );
    }

    #[test]
    fn empty_ops_and_duplicate_base_are_invalid() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "x\n");
        assert_eq!(
            apply_noop(&root, &req(vec![], vec![])).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            apply_noop(
                &root,
                &req(
                    vec![base("f.txt", &v), base("f.txt", &v)],
                    vec![EditOp::Create {
                        path: "g.txt".into(),
                        content: String::new(),
                        executable: false,
                        overwrite: false,
                    }]
                )
            )
            .unwrap_err()
            .code,
            ErrorCode::InvalidInput
        );
    }

    #[test]
    fn jailed_paths_apply_to_ops() {
        let (_dir, root) = setup();
        let err = apply_noop(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "../escape.txt".into(),
                    content: "x".into(),
                    executable: false,
                    overwrite: false,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::OutsideRoot);
    }

    #[test]
    fn recorder_sees_begin_then_complete_with_blobs() {
        use std::sync::Mutex as StdMutex;
        #[derive(Default)]
        struct MockRecorder {
            calls: StdMutex<Vec<String>>,
        }
        impl TxnRecorder for MockRecorder {
            fn begin(
                &self,
                author: &str,
                intent: Option<&str>,
                _root: &ResolvedRoot,
                files: &[FileTxnRecord<'_>],
            ) -> Result<i64> {
                let f = &files[0];
                self.calls.lock().unwrap().push(format!(
                    "begin author={author} intent={:?} path={} old={:?} new_content={:?}",
                    intent, f.path, f.old_content, f.new_content
                ));
                Ok(42)
            }
            fn complete(&self, txn_id: i64) -> Result<()> {
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("complete {txn_id}"));
                Ok(())
            }
        }

        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "x\n");
        let rec = MockRecorder::default();
        let out = apply(
            &root,
            &EditRequest {
                base: vec![base("f.txt", &v)],
                ops: vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "x".into(),
                    new_text: "y".into(),
                    occurrence: None,
                }],
                dry_run: false,
                return_diff: false,
                intent: Some("test intent".into()),
            },
            &Limits::default(),
            "claude",
            &rec,
        )
        .unwrap();
        assert_eq!(out.txn_id, Some(42));
        assert_eq!(out.diff, None);
        let calls = rec.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("author=claude"));
        assert!(calls[0].contains("intent=Some(\"test intent\")"));
        assert!(calls[0].contains("old=Some(\"x\\n\")"));
        assert!(calls[0].contains("new_content=\"y\\n\""));
        assert_eq!(calls[1], "complete 42");
    }

    #[test]
    fn two_writers_one_loser_with_coherent_conflict() {
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "shared\n");
        let make_req = |new_text: &str| {
            req(
                vec![base("f.txt", &v)],
                vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "shared".into(),
                    new_text: new_text.into(),
                    occurrence: None,
                }],
            )
        };
        let (r1, r2) = std::thread::scope(|s| {
            let root1 = &root;
            let root2 = &root;
            let h1 = s.spawn(move || apply_noop(root1, &make_req("writer-one")));
            let h2 = s.spawn(move || apply_noop(root2, &make_req("writer-two")));
            (h1.join().unwrap(), h2.join().unwrap())
        });
        let (ok, err) = match (r1, r2) {
            (Ok(o), Err(e)) => (o, e),
            (Err(e), Ok(o)) => (o, e),
            other => panic!("expected exactly one winner, got {other:?}"),
        };
        assert!(ok.applied);
        assert_eq!(err.code, ErrorCode::VersionConflict);
        // the loser is told what the file became
        assert_eq!(err.data.unwrap()["actual_version"], ok.files[0].new_version);
    }
}
