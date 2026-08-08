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
use crate::dotenv;
use crate::errors::{KaedError, RefusalReason, Result, VersionConflictData};
use crate::fsops::{self, Secrecy};
use crate::leak;
use crate::policy;
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
    /// Set `key` (dotenv-shaped files). Existing key: updates the value
    /// and/or replaces its comment block. New key: appended, `value`
    /// required. A `value` that is exactly a placeholder is substituted
    /// with the real value it seals — the agent passes handles through
    /// verbatim and never holds plaintext (D-9). `value_from` writes the
    /// value another file's key holds, by handle (011 D-5) — same rule,
    /// wider reach: same root resolves here; the server resolves other
    /// roots and other hosts before the transaction ever sees it.
    EnvSet {
        path: String,
        key: String,
        #[serde(default)]
        value: Option<String>,
        #[serde(default)]
        value_from: Option<ValueFrom>,
        #[serde(default)]
        comment: Option<String>,
    },
    /// Rename a key; the value (and its digest) ride along unchanged.
    EnvRename {
        path: String,
        from: String,
        to: String,
    },
    /// Delete a key and its attached comment block. Destroying a non-empty
    /// value still requires naming the key in `drop_keys` (D-10).
    EnvDelete { path: String, key: String },
    /// Rearrange entries into this exact order (a permutation of the
    /// current keys). Entry positions and spacing stay; contents move.
    EnvReorder { path: String, keys: Vec<String> },
    /// Regenerate `path`'s example file (default `<path>.example`): keys
    /// and comments preserved, every value stubbed empty (011 D-7). The
    /// safe direction of the operation where an agent used to copy a real
    /// value by accident. An existing example must be declared in `base`.
    EnvSyncExample {
        path: String,
        #[serde(default)]
        example_path: Option<String>,
    },
}

/// A secret reference consumed by `env_set` (011 D-5, PD-3): resolve by
/// location, verify by digest. No digest = current-value semantics; a
/// digest = exact-or-fail-loudly.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValueFrom {
    /// Host-qualified root holding the source. Defaults to the edit's own
    /// root. A root on another host is resolved by the gateway (the value
    /// transits its memory, never the agent's context — PD-3).
    pub root: Option<String>,
    pub path: String,
    pub key: String,
    /// From a handle / placeholder. When given, a changed source value
    /// fails the edit instead of silently writing whatever is current.
    pub digest: Option<String>,
}

impl EditOp {
    fn path(&self) -> &str {
        match self {
            EditOp::AnchorReplace { path, .. }
            | EditOp::RangeReplace { path, .. }
            | EditOp::Create { path, .. }
            | EditOp::EnvSet { path, .. }
            | EditOp::EnvRename { path, .. }
            | EditOp::EnvDelete { path, .. }
            | EditOp::EnvReorder { path, .. }
            | EditOp::EnvSyncExample { path, .. } => path,
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
    /// Keys whose values this transaction is allowed to destroy. A write
    /// to a classified dotenv file that makes a value vanish — deleted, or
    /// overwritten by a new literal — refuses unless the key is named here
    /// (D-10): otherwise an agent silently destroys a secret it never saw
    /// and kaed cannot restore (D-11: no shadow).
    pub drop_keys: Vec<String>,
    /// Leak matches this transaction may write anyway (012 D-2): the exact
    /// `detail` strings a `secret_leak` refusal reported — a digest, a
    /// provider prefix, an armor label. Naming the specific match keeps
    /// each secret's disclosure a per-decision act, like `drop_keys`.
    pub allow_secrets: Vec<String>,
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
    /// For classified files this is a diff of redacted renderings — a
    /// redacted `read` with a plaintext diff would be theatre (WI #1048).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// `false` means dry run: nothing touched disk.
    pub applied: bool,
    /// e.g. a classified file that is not gitignored (D-12).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// What the journal needs about one file in a transaction. Audit stats
/// (the diffstat) are decoupled from retained content so that classified
/// content can be journaled redacted — or withheld entirely — without the
/// audit trail losing its numbers (D-11).
pub struct FileTxnRecord {
    pub path: String,
    pub old_version: Option<String>,
    pub new_version: String,
    /// Diffstat over the RAW contents; line counts disclose nothing.
    pub lines_added: i64,
    pub lines_removed: i64,
    /// Content to retain as blobs. For classified files these are the
    /// redacted renderings; `None` withholds the blob entirely (content
    /// kaed cannot redact is never journaled).
    pub blob_old: Option<String>,
    pub blob_new: Option<String>,
    /// Whether the blob strings above are redacted renderings rather than
    /// the file's raw bytes. Stored on the blob row so history tools can
    /// label it structurally (and know a legacy plaintext blob when they
    /// see one).
    pub redacted: bool,
}

/// What the journal needs about an attempt that never applied.
pub struct FailedTxnRecord<'a> {
    pub author: &'a str,
    pub intent: Option<&'a str>,
    pub root: &'a ResolvedRoot,
    /// Paths the ops targeted, in the order they were requested.
    pub paths: Vec<&'a str>,
    pub error: &'a KaedError,
}

/// One write-side leak detection for the secrets audit stream (012 D-3).
/// Refusals never carry a txn id (the write never applied); flags carry
/// the transaction they rode into the file on.
pub struct LeakEvent<'a> {
    /// `leak_refused` or `leak_flagged`.
    pub action: &'a str,
    /// The write target — where the secret was headed, not where it lives.
    pub path: &'a str,
    /// `known_digest` | `provider_prefix` | `private_key_block` |
    /// `secret_shaped`.
    pub kind: &'a str,
    /// The match's disclosable identity: digest, prefix, armor label, or
    /// shape description.
    pub detail: &'a str,
    /// `known_digest` only: the matched digest and the key it belongs to.
    pub digest: Option<&'a str>,
    pub source_key: Option<&'a str>,
    pub txn_id: Option<i64>,
    /// The edit's own `intent` — what the agent said it was doing when the
    /// leak happened. Raw; the recorder redacts.
    pub intent: Option<&'a str>,
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
        files: &[FileTxnRecord],
    ) -> Result<i64>;
    fn complete(&self, txn_id: i64) -> Result<()>;
    /// Content for a version the store still retains, for conflict deltas,
    /// with whether the stored content is a redacted rendering. A `false`
    /// on a now-classified path means a legacy plaintext blob — the caller
    /// must redact on read, never serve it (D-11 corollary).
    fn blob(&self, _version: &str) -> Option<(String, bool)> {
        None
    }
    /// An attempt that failed. Never carries content: a failed attempt has
    /// no new bytes worth keeping, and the pre-image is the file on disk.
    fn fail(&self, _record: &FailedTxnRecord<'_>) {}
    /// Where each digest's value lives, if this store has seen it (012
    /// D-4). Same order as `digests`; `None` = unknown. The default —
    /// nothing is known — degrades detection to the prefix and shape
    /// tiers, never to silence.
    fn known_digests(&self, digests: &[String]) -> Vec<Option<crate::leak::DigestLocation>> {
        vec![None; digests.len()]
    }
    /// Write-side leak detections for the secrets audit stream (012 D-3).
    /// Must never fail the write it rides on.
    fn leak_events(&self, _author: &str, _root: &ResolvedRoot, _events: &[LeakEvent<'_>]) {}
}

/// Recorder for tests and journal-less operation.
pub struct NoopRecorder;

impl TxnRecorder for NoopRecorder {
    fn begin(
        &self,
        _author: &str,
        _intent: Option<&str>,
        _root: &ResolvedRoot,
        _files: &[FileTxnRecord],
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
    /// The classification rule covering this path, when there is one. It
    /// drives the vanish guard, the redacted diff, and what the journal
    /// retains.
    classified: Option<String>,
}

/// Apply a transaction, recording the attempt either way.
///
/// Sprint 001 journaled only successes, so the two most diagnostic events —
/// a `version_conflict` and an atomicity rollback — left no trace at all
/// (#910). Conflicts are exactly the signal you want when two agents
/// contend over a file, or when one loops on a stale base, so every failed
/// attempt is now recorded. Dry runs are not: nothing was attempted.
pub fn apply(
    root: &ResolvedRoot,
    req: &EditRequest,
    limits: &Limits,
    author: &str,
    recorder: &dyn TxnRecorder,
) -> Result<EditOutcome> {
    let outcome = apply_inner(root, req, limits, author, recorder);
    if let Err(e) = &outcome
        && !req.dry_run
    {
        recorder.fail(&FailedTxnRecord {
            author,
            intent: req.intent.as_deref(),
            root,
            paths: req.ops.iter().map(EditOp::path).collect(),
            error: e,
        });
    }
    outcome
}

fn apply_inner(
    root: &ResolvedRoot,
    req: &EditRequest,
    limits: &Limits,
    author: &str,
    recorder: &dyn TxnRecorder,
) -> Result<EditOutcome> {
    if req.ops.is_empty() {
        return Err(KaedError::invalid_input("edit requires at least one op"));
    }
    // `.kaedignore` is policy, and policy files can only be changed
    // outside kaed — otherwise the first thing a compromised agent does is
    // write `!~/.ssh` into one (D-3).
    for op in &req.ops {
        if policy::is_kaedignore_path(op.path()) {
            return Err(KaedError::refused(
                op.path(),
                "policy file",
                RefusalReason::KaedignoreProtected,
                ".kaedignore files are readable through kaed but never writable; \
                 edit them outside kaed",
            ));
        }
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
            // For a classified file the delta must be a diff of redacted
            // renderings on BOTH sides: the retained blob may be a legacy
            // plaintext one (pre-008, or a rule added later), and serving
            // it would leak exactly what redaction exists to hold back.
            let delta = match recorder.blob(&b.version) {
                Some((expected, was_redacted)) => match &loaded.secrecy {
                    Secrecy::Plain => unified_diff(&expected, &loaded.content, &b.path),
                    Secrecy::ClassifiedDotenv { file, .. } => {
                        let expected_view = if was_redacted {
                            expected
                        } else {
                            redact_view(&expected)
                        };
                        unified_diff(&expected_view, &file.redact(), &b.path)
                    }
                },
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
        let classified = match &loaded.secrecy {
            Secrecy::Plain => None,
            Secrecy::ClassifiedDotenv { rule, .. } => Some(rule.clone()),
        };
        bufs.insert(
            b.path.clone(),
            FileBuf {
                abs: loaded.abs,
                old_content: Some(loaded.content.clone()),
                content: loaded.content,
                old_version: Some(loaded.version),
                mode: loaded.mode,
                touched: false,
                classified,
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
                    let classified = root.classify.classified_by(&abs);
                    if abs.exists() {
                        if !overwrite {
                            return Err(KaedError::invalid_input(format!(
                                "{path}: already exists; pass overwrite to replace it, \
                                 or declare it in base and edit it"
                            )));
                        }
                        // overwriting an existing file: load it so the old
                        // version and content are journaled (binary and
                        // classified-opaque refusals come along for free)
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
                                classified,
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
                                classified,
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
                refuse_text_op_on_classified(path, buf)?;
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
                refuse_text_op_on_classified(path, buf)?;
                let mut lines = Lines::split(&buf.content);
                lines
                    .replace_range(*start, *end, new_text)
                    .map_err(|e| KaedError::new(e.code, format!("{path}: {}", e.message)))?;
                buf.content = lines.join();
                buf.touched = true;
            }
            EditOp::EnvSet {
                path,
                key,
                value,
                value_from,
                comment,
            } => {
                if value.is_some() && value_from.is_some() {
                    return Err(KaedError::invalid_input(format!(
                        "{path}: env_set {key:?} takes `value` or `value_from`, not both"
                    )));
                }
                // A same-root reference resolves here, against the on-disk
                // source (or its evolving buffer if this txn is editing
                // it). Other roots and other hosts were resolved by the
                // server before the transaction began (011 D-5).
                let from_value = value_from
                    .as_ref()
                    .map(|vf| resolve_value_from(root, vf, &bufs, limits, path))
                    .transpose()?;
                let buf = declared(&mut bufs, path)?;
                let mut file = parse_dotenv_buf(path, buf)?;
                // placeholders resolve against the buffer as previous ops
                // left it — a value set earlier in this txn is addressable
                let resolved = match (&from_value, value.as_deref()) {
                    (Some(v), _) => Some(v.clone()),
                    (None, Some(v)) => Some(dotenv::resolve_value(&file, path, v)?),
                    (None, None) => None,
                };
                file.set(key, resolved.as_deref(), comment.as_deref())
                    .map_err(|e| KaedError::new(e.code, format!("{path}: {}", e.message)))?;
                buf.content = file.render();
                buf.touched = true;
            }
            EditOp::EnvRename { path, from, to } => {
                let buf = declared(&mut bufs, path)?;
                let mut file = parse_dotenv_buf(path, buf)?;
                file.rename(from, to)
                    .map_err(|e| KaedError::new(e.code, format!("{path}: {}", e.message)))?;
                buf.content = file.render();
                buf.touched = true;
            }
            EditOp::EnvDelete { path, key } => {
                let buf = declared(&mut bufs, path)?;
                let mut file = parse_dotenv_buf(path, buf)?;
                file.delete(key)
                    .map_err(|e| KaedError::new(e.code, format!("{path}: {}", e.message)))?;
                buf.content = file.render();
                buf.touched = true;
            }
            EditOp::EnvReorder { path, keys } => {
                let buf = declared(&mut bufs, path)?;
                let mut file = parse_dotenv_buf(path, buf)?;
                file.reorder(keys)
                    .map_err(|e| KaedError::new(e.code, format!("{path}: {}", e.message)))?;
                buf.content = file.render();
                buf.touched = true;
            }
            EditOp::EnvSyncExample { path, example_path } => {
                let example_rel = example_path
                    .clone()
                    .unwrap_or_else(|| format!("{path}.example"));
                if example_rel == *path {
                    return Err(KaedError::invalid_input(format!(
                        "{path}: env_sync_example cannot target the live file itself"
                    )));
                }
                let source = declared(&mut bufs, path)?;
                let example_content = parse_dotenv_buf(path, source)?.example();
                if let Some(buf) = bufs.get_mut(&example_rel) {
                    buf.content = example_content;
                    buf.touched = true;
                } else {
                    let abs = fsops::resolve_creatable(root, &example_rel)?;
                    if abs.exists() {
                        // A generated file, but silently clobbering hand
                        // edits would break R2's promise: declare it.
                        return Err(KaedError::invalid_input(format!(
                            "{example_rel}: already exists — declare it in `base` with \
                             its version so this sync is an ordinary versioned edit"
                        )));
                    }
                    let classified = root.classify.classified_by(&abs);
                    bufs.insert(
                        example_rel.clone(),
                        FileBuf {
                            abs,
                            old_content: None,
                            content: example_content,
                            old_version: None,
                            mode: CREATE_MODE,
                            touched: true,
                            classified,
                        },
                    );
                }
                // size check below keys off the op's own path; the example
                // shrank from the source, which was already checked
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

    // The vanish guard (D-10): destroying a value the agent may never have
    // seen requires saying so. One uniform transaction-level rule, however
    // the vanish happened — env_delete, env_set with a new literal, or a
    // create/overwrite. Renames and placeholder passthrough preserve the
    // value and trip nothing.
    for (path, buf) in &touched {
        if buf.classified.is_none() {
            continue;
        }
        let before = buf.old_content.as_deref().and_then(dotenv::parse);
        let after = dotenv::parse(&buf.content);
        let vanished: Vec<String> = dotenv::vanished_values(before.as_ref(), after.as_ref())
            .into_iter()
            .filter(|k| !req.drop_keys.contains(k))
            .collect();
        if !vanished.is_empty() {
            return Err(KaedError::invalid_input(format!(
                "{path}: this write destroys the value(s) of {vanished:?} — values you may \
                 never have seen and kaed cannot restore. Pass drop_keys: {vanished:?} to \
                 confirm, or preserve them by passing their placeholders through"
            ))
            .with_data(serde_json::json!({
                "reason": "secret_would_vanish",
                "path": path,
                "keys": vanished,
            })));
        }
    }

    // Write-side leak detection (012 D-1..D-3): secrets heading into files
    // where nothing would redact them. Classified files are exempt — they
    // are where secrets belong. Precise tiers refuse with a named override;
    // the heuristic tier warns; everything detected is journaled.
    let mut leak_refused: Vec<(String, leak::LeakMatch)> = Vec::new();
    let mut leak_applied: Vec<(String, &'static str, leak::LeakMatch)> = Vec::new();
    let mut leak_warnings: Vec<String> = Vec::new();
    if root.leak_checks != leak::LeakChecks::Off {
        for (path, buf) in &touched {
            if buf.classified.is_some() {
                continue;
            }
            let matches = leak::scan(buf.old_content.as_deref(), &buf.content, |ds| {
                recorder.known_digests(ds)
            });
            for m in matches {
                let overridden = req.allow_secrets.contains(&m.detail);
                if m.kind.refuses() && root.leak_checks == leak::LeakChecks::Refuse && !overridden {
                    leak_refused.push(((*path).clone(), m));
                    continue;
                }
                leak_warnings.push(leak_warning(path, &m, overridden));
                let action = if overridden {
                    "leak_allowed"
                } else {
                    "leak_flagged"
                };
                leak_applied.push(((*path).clone(), action, m));
            }
        }
    }
    if !leak_refused.is_empty() {
        if !req.dry_run {
            let events: Vec<LeakEvent<'_>> = leak_refused
                .iter()
                .map(|(path, m)| leak_event(path, "leak_refused", m, None, req.intent.as_deref()))
                .collect();
            recorder.leak_events(author, root, &events);
        }
        return Err(leak_refusal(&leak_refused));
    }

    let files: Vec<FileChange> = touched
        .iter()
        .map(|(path, buf)| FileChange {
            path: (*path).clone(),
            old_version: buf.old_version.clone(),
            new_version: fsops::version_of(buf.content.as_bytes()),
        })
        .collect();
    // For classified files the diff is over redacted renderings — the
    // proof-of-what-changed shows placeholders changing, never values.
    let diff = req.return_diff.then(|| {
        touched
            .iter()
            .map(|(path, buf)| {
                if buf.classified.is_some() {
                    let old_view = buf
                        .old_content
                        .as_deref()
                        .map(redact_view)
                        .unwrap_or_default();
                    unified_diff(&old_view, &redact_view(&buf.content), path)
                } else {
                    unified_diff(buf.old_content.as_deref().unwrap_or(""), &buf.content, path)
                }
            })
            .collect::<Vec<_>>()
            .join("")
    });
    let warnings: Vec<String> = {
        let mut w = leak_warnings;
        w.extend(
            touched
                .iter()
                .filter(|(_, buf)| buf.classified.is_some())
                // an all-empty dotenv (a synced `.env.example`) discloses
                // nothing, and it exists precisely to be committed — no
                // warning for it
                .filter(|(_, buf)| {
                    !dotenv::parse(&buf.content).is_some_and(|f| f.all_values_empty())
                })
                .filter_map(|(path, buf)| policy::gitignore_warning(&buf.abs, path)),
        );
        w
    };

    // Writability, asked BEFORE the dry-run return (014 D-4, korg #1092).
    //
    // `dry_run` used to answer "does this edit apply cleanly to this
    // content?" while presenting as if it answered "will this write
    // succeed?" — anchor resolution, version match and diff generation are
    // all content questions, and writability is an environment question
    // that was never asked. So a root-owned file returned a clean diff and
    // a plausible new_version for a write that could not land.
    //
    // One probe, one vocabulary, both paths: the real write gets the same
    // structured refusal instead of an EACCES out of `stage`, and the
    // prediction is the thing being predicted.
    // The subject is the deepest EXISTING ancestor, not the immediate
    // parent: a create into a not-yet-existing subdirectory needs write
    // permission on the highest directory `create_dir_all` will start from.
    for (path, buf) in &touched {
        let dir = fsops::deepest_existing(buf.abs.parent().unwrap_or(&buf.abs));
        if !crate::perm::dir_is_writable(dir) {
            return Err(crate::perm::not_writable(root, path, dir));
        }
    }

    if req.dry_run {
        return Ok(EditOutcome {
            txn_id: None,
            files,
            diff,
            applied: false,
            warnings,
        });
    }

    // Stage everything before any rename; abort cleanly on any failure.
    let mut staged = Vec::new();
    for (path, buf) in &touched {
        match fsops::stage(&buf.abs, buf.content.as_bytes(), buf.mode & 0o7777) {
            Ok(s) => staged.push(s),
            Err(e) => {
                for s in &staged {
                    fsops::discard(s);
                }
                // Backstop for what the probe cannot see: a race, or a
                // refusal from a layer above DAC. Reclassify rather than
                // let an os-error-13 escape as `internal` again.
                let dir = fsops::deepest_existing(buf.abs.parent().unwrap_or(&buf.abs));
                if e.code == crate::errors::ErrorCode::Internal
                    && !crate::perm::dir_is_writable(dir)
                {
                    return Err(crate::perm::not_writable(root, path, dir));
                }
                return Err(e);
            }
        }
    }

    let records: Vec<FileTxnRecord> = touched
        .iter()
        .zip(&files)
        .map(|((path, buf), change)| {
            let (added, removed) = diffstat(buf.old_content.as_deref().unwrap_or(""), &buf.content);
            let redacted = buf.classified.is_some();
            // classified content is journaled as its redacted rendering;
            // content kaed cannot redact is withheld outright (D-11)
            let (blob_old, blob_new) = if redacted {
                (
                    buf.old_content.as_deref().and_then(try_redact),
                    try_redact(&buf.content),
                )
            } else {
                (buf.old_content.clone(), Some(buf.content.clone()))
            };
            FileTxnRecord {
                path: (*path).clone(),
                old_version: buf.old_version.clone(),
                new_version: change.new_version.clone(),
                lines_added: added,
                lines_removed: removed,
                blob_old,
                blob_new,
                redacted,
            }
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

    // Flagged and override-allowed leaks are journaled with the
    // transaction they rode into the file on (012 D-3).
    if !leak_applied.is_empty() {
        let events: Vec<LeakEvent<'_>> = leak_applied
            .iter()
            .map(|(path, action, m)| {
                leak_event(path, action, m, Some(txn_id), req.intent.as_deref())
            })
            .collect();
        recorder.leak_events(author, root, &events);
    }

    Ok(EditOutcome {
        txn_id: Some(txn_id),
        files,
        diff,
        applied: true,
        warnings,
    })
}

fn declared<'a>(bufs: &'a mut BTreeMap<String, FileBuf>, path: &str) -> Result<&'a mut FileBuf> {
    bufs.get_mut(path).ok_or_else(|| {
        KaedError::invalid_input(format!(
            "{path}: not declared in base (every edited file needs its version there)"
        ))
    })
}

/// Text edits over redacted text have ugly failure modes — an anchor
/// landing mid-placeholder, a truncated placeholder written back as a
/// literal. The typed ops make them unrepresentable (D-9).
fn refuse_text_op_on_classified(path: &str, buf: &FileBuf) -> Result<()> {
    match &buf.classified {
        Some(rule) => Err(KaedError::invalid_input(format!(
            "{path} is classified (rule {rule}) and served redacted; text ops over \
             redacted content are refused — use env_set / env_rename / env_delete / \
             env_reorder (placeholders pass through verbatim, kaed substitutes the \
             real values on write)"
        ))),
        None => Ok(()),
    }
}

/// Resolve a same-root `value_from` (011 D-5): the evolving buffer when
/// this transaction is also editing the source, else the on-disk file
/// through every policy layer (`load_text` — a denied or opaque source
/// refuses exactly as a read would). A digest that no longer matches fails
/// loudly rather than silently writing the current value (PD-3).
fn resolve_value_from(
    root: &ResolvedRoot,
    vf: &ValueFrom,
    bufs: &BTreeMap<String, FileBuf>,
    limits: &Limits,
    target_path: &str,
) -> Result<String> {
    if let Some(r) = vf.root.as_deref()
        && r != root.name
    {
        // The server resolves every reference beyond this root — other
        // local roots and other hosts — before the engine runs. Reaching
        // here means it did not, which is a routing gap, not a file state.
        return Err(KaedError::invalid_input(format!(
            "{target_path}: value_from names root {r:?}, but this transaction runs \
             on {:?} — cross-root references are resolved before the engine, so \
             the instance that accepted this call could not (or did not) resolve \
             {r:?}; check `roots` for whether it can route there",
            root.name
        )));
    }
    let content_owned;
    let content: &str = match bufs.get(&vf.path) {
        Some(buf) => &buf.content,
        None => {
            let loaded = fsops::load_text(root, &vf.path, limits)?;
            content_owned = loaded.content;
            &content_owned
        }
    };
    let file = dotenv::parse(content).ok_or_else(|| {
        KaedError::invalid_input(format!(
            "{target_path}: value_from source {} is not dotenv-shaped",
            vf.path
        ))
    })?;
    let entry = file.get(&vf.key).ok_or_else(|| {
        KaedError::not_found(format!(
            "{target_path}: value_from source {} has no key {:?}",
            vf.path, vf.key
        ))
    })?;
    if let Some(d) = &vf.digest
        && crate::secrets::digest_of(&entry.value) != *d
    {
        return Err(KaedError::invalid_input(format!(
            "{target_path}: value_from digest mismatch for {:?} in {} — the value \
             changed since that handle was taken. Re-describe the key and decide \
             whether current is what you want (omit `digest` for current-value \
             semantics)",
            vf.key, vf.path
        ))
        .with_data(serde_json::json!({
            "reason": "digest_mismatch",
            "path": vf.path,
            "key": vf.key,
            "expected_digest": d,
        })));
    }
    Ok(entry.value.clone())
}

/// What fired, for humans and audit rows. Never the token itself.
fn leak_describe(m: &leak::LeakMatch) -> String {
    match m.kind {
        leak::LeakKind::KnownDigest => match &m.source {
            Some(s) if !s.key.is_empty() => format!(
                "the value of {} from {} {} (digest {})",
                s.key, s.root, s.path, m.detail
            ),
            Some(s) => format!(
                "a value kaed redacted out of {} {} (digest {})",
                s.root, s.path, m.detail
            ),
            None => format!("a known secret (digest {})", m.detail),
        },
        leak::LeakKind::ProviderPrefix => format!("a {}… provider token", m.detail),
        leak::LeakKind::PrivateKeyBlock => format!("a {} block", m.detail),
        leak::LeakKind::SecretShaped => format!("a {}", m.detail),
    }
}

fn leak_warning(path: &str, m: &leak::LeakMatch, overridden: bool) -> String {
    if overridden {
        format!(
            "{path}:{}: {} — written under its allow_secrets override",
            m.line,
            leak_describe(m)
        )
    } else {
        format!(
            "{path}:{}: {} in a file nothing classifies or redacts — if this is a \
             real secret, remove it and reference its variable instead",
            m.line,
            leak_describe(m)
        )
    }
}

fn leak_event<'a>(
    path: &'a str,
    action: &'a str,
    m: &'a leak::LeakMatch,
    txn_id: Option<i64>,
    intent: Option<&'a str>,
) -> LeakEvent<'a> {
    LeakEvent {
        action,
        path,
        kind: m.kind.as_str(),
        detail: &m.detail,
        digest: (m.kind == leak::LeakKind::KnownDigest).then_some(m.detail.as_str()),
        source_key: m.source.as_ref().map(|s| s.key.as_str()),
        txn_id,
        intent,
    }
}

/// The refusal (012 D-2): names every match, what to do instead, and the
/// exact override — the vanish guard's shape, applied to leaks.
fn leak_refusal(refused: &[(String, leak::LeakMatch)]) -> KaedError {
    let described: Vec<String> = refused
        .iter()
        .map(|(p, m)| format!("{p}:{}: {}", m.line, leak_describe(m)))
        .collect();
    let mut overrides: Vec<&str> = refused.iter().map(|(_, m)| m.detail.as_str()).collect();
    overrides.dedup();
    let matches: Vec<serde_json::Value> = refused
        .iter()
        .map(|(p, m)| {
            serde_json::json!({
                "path": p,
                "line": m.line,
                "kind": m.kind,
                "detail": m.detail,
                "source": m.source.as_ref().map(|s| {
                    serde_json::json!({"root": s.root, "path": s.path, "key": s.key})
                }),
            })
        })
        .collect();
    KaedError::invalid_input(format!(
        "this write would put secret content into files nothing classifies or \
         redacts: {} — reference the variable instead of the value (dotenv \
         targets take env_set with value_from or a placeholder). If writing the \
         literal is deliberate, retry with allow_secrets: {overrides:?}; if the \
         target file should hold secrets, add it to [security] classify so kaed \
         serves it redacted",
        described.join("; ")
    ))
    .with_data(serde_json::json!({
        "reason": "secret_leak",
        "matches": matches,
        "allow_secrets": overrides,
    }))
}

/// Parse the evolving buffer for an env op. Env ops work on any strictly
/// dotenv-shaped file, classified or not — classification governs
/// redaction, not the op vocabulary.
fn parse_dotenv_buf(path: &str, buf: &FileBuf) -> Result<dotenv::DotenvFile> {
    dotenv::parse(&buf.content).ok_or_else(|| {
        KaedError::invalid_input(format!(
            "{path}: not dotenv-shaped (every line must be blank, a # comment, or \
             KEY=value at column 0) — env ops do not apply; use text ops"
        ))
    })
}

/// The redacted rendering for diffs and journal blobs; content that cannot
/// be redacted is represented by a marker, never by itself.
fn redact_view(content: &str) -> String {
    try_redact(content)
        .unwrap_or_else(|| "(content withheld: classified and not dotenv-shaped)\n".to_owned())
}

fn try_redact(content: &str) -> Option<String> {
    dotenv::parse(content).map(|f| f.redact())
}

pub(crate) fn diffstat(old: &str, new: &str) -> (i64, i64) {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut added = 0;
    let mut removed = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            similar::ChangeTag::Insert => added += 1,
            similar::ChangeTag::Delete => removed += 1,
            similar::ChangeTag::Equal => {}
        }
    }
    (added, removed)
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
        let root = ResolvedRoot::unrestricted("t", dir.path().canonicalize().unwrap());
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
            drop_keys: Vec::new(),
            allow_secrets: Vec::new(),
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
                _: &[FileTxnRecord],
            ) -> Result<i64> {
                Ok(1)
            }
            fn complete(&self, _: i64) -> Result<()> {
                Ok(())
            }
            fn blob(&self, version: &str) -> Option<(String, bool)> {
                (version == fsops::version_of(b"old line\n"))
                    .then(|| ("old line\n".to_string(), false))
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

    // ------------------------------ the OS layer (014, #1091/#1092)

    /// korg #1092, the finding that outlives every host-side fix: a
    /// `dry_run` against a file in an unwritable directory returned a clean
    /// diff and a plausible `new_version` for a write that could not land.
    /// An agent doing the exact thing `dry_run` exists for learned nothing.
    #[test]
    fn a_dry_run_into_an_unwritable_directory_refuses_instead_of_promising() {
        if crate::perm::running_as_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        std::fs::create_dir(dir.path().join("prometheus")).unwrap();
        let v = write(dir.path(), "prometheus/prometheus.yml", "scrape: 15s\n");
        std::fs::set_permissions(
            dir.path().join("prometheus"),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        let req = |dry_run| EditRequest {
            base: vec![base("prometheus/prometheus.yml", &v)],
            ops: vec![EditOp::AnchorReplace {
                path: "prometheus/prometheus.yml".into(),
                old_text: "15s".into(),
                new_text: "30s".into(),
                occurrence: None,
            }],
            dry_run,
            return_diff: true,
            intent: None,
            drop_keys: Vec::new(),
            allow_secrets: Vec::new(),
        };
        let dry = apply(&root, &req(true), &Limits::default(), "test", &NoopRecorder).unwrap_err();
        let wet = apply(
            &root,
            &req(false),
            &Limits::default(),
            "test",
            &NoopRecorder,
        )
        .unwrap_err();
        let _ = std::fs::set_permissions(
            dir.path().join("prometheus"),
            std::fs::Permissions::from_mode(0o755),
        );

        // The prediction IS the thing predicted — same code, same reason.
        for err in [&dry, &wet] {
            let v = serde_json::to_value(err).unwrap();
            assert_eq!(v["code"], "denied");
            assert_eq!(v["data"]["reason"], "not_writable_by_service_identity");
            assert_eq!(v["data"]["path"], "prometheus/prometheus.yml");
            assert_eq!(v["data"]["directory"], "prometheus");
            assert!(v["data"]["hint"].is_string());
        }
    }

    /// D-2 from the other side: a file kaed cannot open for writing is
    /// still writable *through kaed*, because the atomic path renames over
    /// it. The probe must not refuse a write that would have worked.
    #[test]
    fn a_read_only_file_in_a_writable_directory_still_edits() {
        if crate::perm::running_as_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        let v = write(dir.path(), "f.txt", "x\n");
        std::fs::set_permissions(
            dir.path().join("f.txt"),
            std::fs::Permissions::from_mode(0o444),
        )
        .unwrap();
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
                intent: None,
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "test",
            &NoopRecorder,
        )
        .unwrap();
        assert!(out.applied);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "y\n"
        );
    }

    /// #1091's read half, on the path that produced kubsdb `failure_id 1`:
    /// the file is listed with its size, and opening it used to collapse to
    /// `internal: … Permission denied (os error 13)`.
    #[test]
    fn an_unreadable_file_refuses_with_a_reason_not_a_bare_internal() {
        if crate::perm::running_as_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        write(dir.path(), "docker-compose.yml", "POSTGRES_PASSWORD: x\n");
        std::fs::set_permissions(
            dir.path().join("docker-compose.yml"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();

        let err = fsops::load_text(&root, "docker-compose.yml", &Limits::default()).unwrap_err();
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["code"], "denied");
        assert_eq!(v["data"]["reason"], "not_readable_by_service_identity");
        assert_eq!(v["data"]["path"], "docker-compose.yml");
        assert!(v["data"]["owner"]["mode"].is_string());
        assert!(
            !err.message.contains("os error 13"),
            "the raw errno is not an explanation: {}",
            err.message
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
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
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
                files: &[FileTxnRecord],
            ) -> Result<i64> {
                let f = &files[0];
                self.calls.lock().unwrap().push(format!(
                    "begin author={author} intent={:?} path={} old={:?} new_content={:?}",
                    intent, f.path, f.blob_old, f.blob_new
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
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
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
        assert!(calls[0].contains("new_content=Some(\"y\\n\")"));
        assert_eq!(calls[1], "complete 42");
    }

    // ------------------------------------------------ secrets model (008)

    const TOKEN_VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";

    fn env_content() -> String {
        format!("KLAMS_TOKEN={TOKEN_VALUE}\nDEBUG=true\n")
    }

    fn classified_setup() -> (tempfile::TempDir, ResolvedRoot, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::with_default_classify("t", dir.path().canonicalize().unwrap());
        let v = write(dir.path(), ".env", &env_content());
        (dir, root, v)
    }

    #[test]
    fn env_set_and_placeholder_passthrough_write_real_values() {
        let (dir, root, v) = classified_setup();
        // the agent copies KLAMS_TOKEN into a new key by its placeholder —
        // a sealed handle from the redacted read — and never sees the value
        let ph = crate::secrets::placeholder("KLAMS_TOKEN", TOKEN_VALUE);
        let out = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![
                    EditOp::EnvSet {
                        path: ".env".into(),
                        key: "KLAMS_TOKEN_COPY".into(),
                        value: Some(ph),
                        value_from: None,
                        comment: None,
                    },
                    EditOp::EnvSet {
                        path: ".env".into(),
                        key: "NEW_FLAG".into(),
                        value: Some("on".into()),
                        value_from: None,
                        comment: Some("added by test".into()),
                    },
                ],
            ),
        )
        .unwrap();
        assert!(out.applied);
        let on_disk = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(
            on_disk.contains(&format!("KLAMS_TOKEN_COPY={TOKEN_VALUE}")),
            "the real value landed: {on_disk}"
        );
        assert!(
            on_disk.contains("# added by test\nNEW_FLAG=on"),
            "{on_disk}"
        );
        // …but the proof-of-what-changed shows only placeholders
        let diff = out.diff.unwrap();
        assert!(!diff.contains(TOKEN_VALUE), "diff leaked the value: {diff}");
        assert!(diff.contains("⟨kaed:KLAMS_TOKEN_COPY@"), "{diff}");
    }

    #[test]
    fn a_stale_placeholder_digest_fails_loudly() {
        let (_dir, root, v) = classified_setup();
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "COPY".into(),
                    value: Some("⟨kaed:KLAMS_TOKEN@0000000000000000⟩".into()),
                    value_from: None,
                    comment: None,
                }],
            ),
        )
        .unwrap_err();
        assert!(err.message.contains("digest mismatch"), "{}", err.message);
    }

    #[test]
    fn text_ops_on_a_classified_dotenv_are_refused_with_the_env_alternative() {
        let (dir, root, v) = classified_setup();
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::AnchorReplace {
                    path: ".env".into(),
                    old_text: "DEBUG".into(),
                    new_text: "VERBOSE".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("env_set"), "{}", err.message);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env")).unwrap(),
            env_content()
        );
    }

    /// D-10: destroying a value the agent never saw requires declaring it.
    #[test]
    fn the_vanish_guard_refuses_undeclared_destruction() {
        let (dir, root, v) = classified_setup();
        for ops in [
            vec![EditOp::EnvDelete {
                path: ".env".into(),
                key: "KLAMS_TOKEN".into(),
            }],
            vec![EditOp::EnvSet {
                path: ".env".into(),
                key: "KLAMS_TOKEN".into(),
                value: Some("overwritten".into()),
                value_from: None,
                comment: None,
            }],
        ] {
            let err = apply_noop(&root, &req(vec![base(".env", &v)], ops)).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput);
            let data = err.data.unwrap();
            assert_eq!(data["reason"], "secret_would_vanish");
            assert_eq!(data["keys"], serde_json::json!(["KLAMS_TOKEN"]));
            assert_eq!(
                std::fs::read_to_string(dir.path().join(".env")).unwrap(),
                env_content(),
                "nothing applied"
            );
        }

        // …and drop_keys is the declaration that lets it through
        let out = apply_noop(
            &root,
            &EditRequest {
                drop_keys: vec!["KLAMS_TOKEN".into()],
                allow_secrets: Vec::new(),
                ..req(
                    vec![base(".env", &v)],
                    vec![EditOp::EnvDelete {
                        path: ".env".into(),
                        key: "KLAMS_TOKEN".into(),
                    }],
                )
            },
        )
        .unwrap();
        assert!(out.applied);
        assert!(
            !std::fs::read_to_string(dir.path().join(".env"))
                .unwrap()
                .contains("KLAMS_TOKEN")
        );
    }

    /// Renames move the value, destroy nothing, and need no declaration.
    #[test]
    fn rename_passes_the_vanish_guard_undeclared() {
        let (dir, root, v) = classified_setup();
        apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvRename {
                    path: ".env".into(),
                    from: "KLAMS_TOKEN".into(),
                    to: "KLAMS_API_TOKEN".into(),
                }],
            ),
        )
        .unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(on_disk.contains(&format!("KLAMS_API_TOKEN={TOKEN_VALUE}")));
    }

    /// The DEBUG=true half of the guard: below-floor values are still
    /// values; overwriting one undeclared also refuses.
    #[test]
    fn env_ops_work_on_unclassified_dotenv_without_the_guard() {
        let (dir, root) = setup();
        // dotenv-shaped but unclassified: env ops still apply — they are
        // just the better way to edit dotenv — with no guard, plain diff
        let v = write(dir.path(), "settings.conf", "MODE=fast\n");
        let out = apply_noop(
            &root,
            &req(
                vec![base("settings.conf", &v)],
                vec![EditOp::EnvSet {
                    path: "settings.conf".into(),
                    key: "MODE".into(),
                    value: Some("slow".into()),
                    value_from: None,
                    comment: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        assert!(out.diff.unwrap().contains("+MODE=slow"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.conf")).unwrap(),
            "MODE=slow\n"
        );
    }

    #[test]
    fn kaedignore_files_are_unwritable_through_kaed() {
        let (_dir, root) = setup();
        let err = apply_noop(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "sub/.kaedignore".into(),
                    content: "!*\n".into(),
                    executable: false,
                    overwrite: false,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Denied);
        assert_eq!(err.data.unwrap()["reason"], "kaedignore_protected");
    }

    /// The conflict delta must be redacted on BOTH sides — including when
    /// the retained blob is a legacy plaintext one from before the path
    /// was classified (D-11 corollary).
    #[test]
    fn conflict_delta_on_classified_is_redacted_even_from_a_plaintext_blob() {
        struct LegacyBlobRecorder(String);
        impl TxnRecorder for LegacyBlobRecorder {
            fn begin(
                &self,
                _: &str,
                _: Option<&str>,
                _: &ResolvedRoot,
                _: &[FileTxnRecord],
            ) -> Result<i64> {
                Ok(1)
            }
            fn complete(&self, _: i64) -> Result<()> {
                Ok(())
            }
            fn blob(&self, version: &str) -> Option<(String, bool)> {
                (version == fsops::version_of(self.0.as_bytes())).then(|| (self.0.clone(), false)) // plaintext, pre-008
            }
        }

        let (_dir, root, _) = classified_setup();
        let old = "KLAMS_TOKEN=oldsecretAAAABBBBCCCCDDDDEEEE9999\n".to_owned();
        let stale = fsops::version_of(old.as_bytes());
        let err = apply(
            &root,
            &req(
                vec![base(".env", &stale)],
                vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "X".into(),
                    value: Some("y".into()),
                    value_from: None,
                    comment: None,
                }],
            ),
            &Limits::default(),
            "test",
            &LegacyBlobRecorder(old.clone()),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::VersionConflict);
        let delta = err.data.unwrap()["delta"].as_str().unwrap().to_string();
        assert!(
            !delta.contains("oldsecret") && !delta.contains(TOKEN_VALUE),
            "delta leaked plaintext: {delta}"
        );
        assert!(delta.contains("⟨kaed:KLAMS_TOKEN"), "{delta}");
    }

    // -------------------------------------------- value_from + sync (011)

    #[test]
    fn value_from_copies_across_files_in_the_same_root() {
        let (dir, root, v_env) = classified_setup();
        let v_svc = write(dir.path(), "svc/.env", "CLIENT_TOKEN=\n");
        let out = apply_noop(
            &root,
            &req(
                vec![base(".env", &v_env), base("svc/.env", &v_svc)],
                vec![EditOp::EnvSet {
                    path: "svc/.env".into(),
                    key: "CLIENT_TOKEN".into(),
                    value: None,
                    value_from: Some(ValueFrom {
                        root: None,
                        path: ".env".into(),
                        key: "KLAMS_TOKEN".into(),
                        digest: Some(crate::secrets::digest_of(TOKEN_VALUE)),
                    }),
                    comment: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        assert!(
            std::fs::read_to_string(dir.path().join("svc/.env"))
                .unwrap()
                .contains(&format!("CLIENT_TOKEN={TOKEN_VALUE}"))
        );
        // the proof stays redacted
        assert!(!out.diff.unwrap().contains(TOKEN_VALUE));
    }

    #[test]
    fn value_from_digest_mismatch_fails_loudly_and_applies_nothing() {
        let (dir, root, v_env) = classified_setup();
        let v_svc = write(dir.path(), "svc/.env", "CLIENT_TOKEN=\n");
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v_env), base("svc/.env", &v_svc)],
                vec![EditOp::EnvSet {
                    path: "svc/.env".into(),
                    key: "CLIENT_TOKEN".into(),
                    value: None,
                    value_from: Some(ValueFrom {
                        root: None,
                        path: ".env".into(),
                        key: "KLAMS_TOKEN".into(),
                        digest: Some("0000000000000000".into()),
                    }),
                    comment: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.data.as_ref().unwrap()["reason"], "digest_mismatch");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("svc/.env")).unwrap(),
            "CLIENT_TOKEN=\n"
        );
    }

    #[test]
    fn value_from_and_value_are_mutually_exclusive() {
        let (_dir, root, v) = classified_setup();
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "X".into(),
                    value: Some("literal".into()),
                    value_from: Some(ValueFrom {
                        root: None,
                        path: ".env".into(),
                        key: "KLAMS_TOKEN".into(),
                        digest: None,
                    }),
                    comment: None,
                }],
            ),
        )
        .unwrap_err();
        assert!(err.message.contains("not both"), "{}", err.message);
    }

    #[test]
    fn value_from_source_goes_through_every_policy_layer() {
        let (dir, root, _) = classified_setup();
        // an opaque-classified source (PEM shape under a classify rule)
        std::fs::write(
            dir.path().join("key.pem"),
            "-----BEGIN PRIVATE KEY-----\nzzz\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        let v_svc = write(dir.path(), "svc/.env", "CLIENT_TOKEN=\n");
        let err = apply_noop(
            &root,
            &req(
                vec![base("svc/.env", &v_svc)],
                vec![EditOp::EnvSet {
                    path: "svc/.env".into(),
                    key: "CLIENT_TOKEN".into(),
                    value: None,
                    value_from: Some(ValueFrom {
                        root: None,
                        path: "key.pem".into(),
                        key: "WHATEVER".into(),
                        digest: None,
                    }),
                    comment: None,
                }],
            ),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Denied);
        assert_eq!(err.data.unwrap()["reason"], "classified_opaque");
    }

    #[test]
    fn value_from_a_foreign_root_is_refused_at_the_engine() {
        let (_dir, root, v) = classified_setup();
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "X".into(),
                    value: None,
                    value_from: Some(ValueFrom {
                        root: Some("elsewhere:src".into()),
                        path: ".env".into(),
                        key: "K".into(),
                        digest: None,
                    }),
                    comment: None,
                }],
            ),
        )
        .unwrap_err();
        assert!(err.message.contains("elsewhere:src"), "{}", err.message);
    }

    #[test]
    fn env_sync_example_stubs_values_and_redacts_comments() {
        let (dir, root) = setup();
        let content = format!(
            "# how to run\n# old token: {TOKEN_VALUE} (rotated)\nKLAMS_TOKEN={TOKEN_VALUE}\n\nexport DEBUG=true\n"
        );
        let v = write(dir.path(), ".env", &content);
        let out = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSyncExample {
                    path: ".env".into(),
                    example_path: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        let example = std::fs::read_to_string(dir.path().join(".env.example")).unwrap();
        assert!(example.contains("KLAMS_TOKEN=\n"), "{example}");
        assert!(example.contains("export DEBUG=\n"), "{example}");
        assert!(example.contains("# how to run"), "{example}");
        assert!(
            !example.contains(TOKEN_VALUE),
            "comment token leaked into the example: {example}"
        );
        // the live file is untouched
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".env")).unwrap(),
            content
        );

        // an existing example must be declared in base…
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSyncExample {
                    path: ".env".into(),
                    example_path: None,
                }],
            ),
        )
        .unwrap_err();
        assert!(err.message.contains("base"), "{}", err.message);

        // …and once declared, the sync updates it
        let v_ex = fsops::version_of(example.as_bytes());
        let v2 = write(dir.path(), ".env", "NEW_KEY=x\n");
        let out = apply_noop(
            &root,
            &req(
                vec![base(".env", &v2), base(".env.example", &v_ex)],
                vec![EditOp::EnvSyncExample {
                    path: ".env".into(),
                    example_path: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        let example = std::fs::read_to_string(dir.path().join(".env.example")).unwrap();
        assert_eq!(example, "NEW_KEY=\n");
    }

    #[test]
    fn env_sync_example_on_a_classified_env_warns_nothing() {
        // the example is classified by name (`**/.env.*`) but holds no
        // values, so the gitignore warning stays quiet for it
        let (dir, root, v) = classified_setup();
        let out = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSyncExample {
                    path: ".env".into(),
                    example_path: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        let example = std::fs::read_to_string(dir.path().join(".env.example")).unwrap();
        assert!(!example.contains(TOKEN_VALUE));
        assert!(
            !out.warnings.iter().any(|w| w.contains(".env.example")),
            "an all-empty example must not warn: {:?}",
            out.warnings
        );
    }

    #[test]
    fn env_sync_example_refuses_the_live_file_as_target() {
        let (_dir, root, v) = classified_setup();
        let err = apply_noop(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSyncExample {
                    path: ".env".into(),
                    example_path: Some(".env".into()),
                }],
            ),
        )
        .unwrap_err();
        assert!(err.message.contains("itself"), "{}", err.message);
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

    // ------------------------------------- write-side leak detection (012)

    const ANT_TOKEN: &str = "sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1dFg6jVe3aU";

    fn journal_recorder() -> crate::journal::Journal {
        crate::journal::Journal::open_in_memory().unwrap()
    }

    fn secret_event_rows(j: &crate::journal::Journal) -> Vec<(String, String, Option<i64>)> {
        j.secret_events(&crate::journal::HistoryFilter {
            limit: 50,
            ..Default::default()
        })
        .unwrap()
        .into_iter()
        .map(|r| (r.action, r.key, r.txn_id))
        .collect()
    }

    #[test]
    fn a_provider_token_into_a_plain_file_refuses_then_the_override_applies() {
        let (dir, root) = setup();
        let j = journal_recorder();
        let v = write(dir.path(), "README.md", "# setup\n");
        let mut r = req(
            vec![base("README.md", &v)],
            vec![EditOp::AnchorReplace {
                path: "README.md".into(),
                old_text: "# setup".into(),
                new_text: format!("# setup\ntoken: {ANT_TOKEN}"),
                occurrence: None,
            }],
        );
        let err = apply(&root, &r, &Limits::default(), "test", &j).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        let data = err.data.as_ref().unwrap();
        assert_eq!(data["reason"], "secret_leak");
        assert_eq!(data["allow_secrets"], serde_json::json!(["sk-ant-"]));
        assert_eq!(data["matches"][0]["kind"], "provider_prefix");
        assert!(
            !err.message.contains(ANT_TOKEN),
            "the refusal must not echo the token"
        );
        assert!(
            std::fs::read_to_string(dir.path().join("README.md"))
                .unwrap()
                .starts_with("# setup\n"),
            "nothing applied"
        );
        // the refusal is a countable audit event (D-3)
        assert_eq!(
            secret_event_rows(&j),
            vec![("leak_refused".into(), "sk-ant-".into(), None)]
        );

        // the named override is the retry path, and the write stays visible:
        // a warning plus a leak_allowed row with the txn it rode in on
        r.allow_secrets = vec!["sk-ant-".into()];
        let out = apply(&root, &r, &Limits::default(), "test", &j).unwrap();
        assert!(out.applied);
        assert!(
            out.warnings.iter().any(|w| w.contains("allow_secrets")),
            "{:?}",
            out.warnings
        );
        let rows = secret_event_rows(&j);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].0, "leak_allowed");
        assert_eq!(rows[0].2, out.txn_id);
    }

    #[test]
    fn a_known_secret_refuses_by_digest_and_names_the_variable() {
        let (dir, root) = setup();
        let j = journal_recorder();
        j.record_digests(
            "kai:home",
            ".env",
            &[("KLAMS_TOKEN".into(), crate::secrets::digest_of(TOKEN_VALUE))],
        );
        let err = apply(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "notes.md".into(),
                    content: format!("the value is {TOKEN_VALUE}\n"),
                    executable: false,
                    overwrite: false,
                }],
            ),
            &Limits::default(),
            "test",
            &j,
        )
        .unwrap_err();
        let data = err.data.as_ref().unwrap();
        assert_eq!(data["matches"][0]["kind"], "known_digest");
        assert_eq!(data["matches"][0]["source"]["key"], "KLAMS_TOKEN");
        assert!(
            err.message.contains("KLAMS_TOKEN") && err.message.contains("kai:home"),
            "the hint names the variable to reference instead: {}",
            err.message
        );
        assert!(!err.message.contains(TOKEN_VALUE));
        assert!(!dir.path().join("notes.md").exists());
    }

    #[test]
    fn the_heuristic_tier_warns_applies_and_journals_the_flag() {
        let (dir, root) = setup();
        let j = journal_recorder();
        let out = apply(
            &root,
            &req(
                vec![],
                vec![EditOp::Create {
                    path: "fixture.txt".into(),
                    content: "checksum: Qx9zPaB3cD7eF1Gh2jK4LmN6pRsT8uVwXy0ZaBcDeFg\n".into(),
                    executable: false,
                    overwrite: false,
                }],
            ),
            &Limits::default(),
            "test",
            &j,
        )
        .unwrap();
        assert!(out.applied, "the heuristic tier must never block");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("high-entropy token")),
            "{:?}",
            out.warnings
        );
        assert!(dir.path().join("fixture.txt").exists());
        let rows = secret_event_rows(&j);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "leak_flagged");
        assert_eq!(rows[0].2, out.txn_id);
    }

    #[test]
    fn classified_targets_are_exempt_because_secrets_belong_there() {
        let (_dir, root, v) = classified_setup();
        let j = journal_recorder();
        let out = apply(
            &root,
            &req(
                vec![base(".env", &v)],
                vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "ANTHROPIC_KEY".into(),
                    value: Some(ANT_TOKEN.into()),
                    value_from: None,
                    comment: None,
                }],
            ),
            &Limits::default(),
            "test",
            &j,
        )
        .unwrap();
        assert!(out.applied);
        assert!(
            secret_event_rows(&j)
                .iter()
                .all(|(a, _, _)| !a.starts_with("leak_")),
            "writing a secret into a classified file is the expected case"
        );
    }

    #[test]
    fn a_dry_run_predicts_the_refusal_without_journaling_it() {
        let (dir, root) = setup();
        let j = journal_recorder();
        let v = write(dir.path(), "doc.md", "text\n");
        let mut r = req(
            vec![base("doc.md", &v)],
            vec![EditOp::AnchorReplace {
                path: "doc.md".into(),
                old_text: "text".into(),
                new_text: format!("text {ANT_TOKEN}"),
                occurrence: None,
            }],
        );
        r.dry_run = true;
        let err = apply(&root, &r, &Limits::default(), "test", &j).unwrap_err();
        assert_eq!(err.data.unwrap()["reason"], "secret_leak");
        assert!(
            secret_event_rows(&j).is_empty(),
            "nothing was attempted, nothing is journaled"
        );
    }

    #[test]
    fn a_file_already_holding_a_token_stays_editable() {
        let (dir, root) = setup();
        let existing = format!("intro\ntoken: {ANT_TOKEN}\noutro\n");
        let v = write(dir.path(), "doc.md", &existing);
        // an unrelated edit, and the edit that removes the token, both pass
        let out = apply_noop(
            &root,
            &req(
                vec![base("doc.md", &v)],
                vec![EditOp::AnchorReplace {
                    path: "doc.md".into(),
                    old_text: "intro".into(),
                    new_text: "introduction".into(),
                    occurrence: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    }

    #[test]
    fn the_config_lever_downgrades_and_disables() {
        let dir = tempfile::tempdir().unwrap();
        let mut root = ResolvedRoot::unrestricted("t", dir.path().canonicalize().unwrap());
        let v = write(dir.path(), "doc.md", "text\n");
        let r = |new_text: String| {
            req(
                vec![base("doc.md", &v)],
                vec![EditOp::AnchorReplace {
                    path: "doc.md".into(),
                    old_text: "text".into(),
                    new_text,
                    occurrence: None,
                }],
            )
        };
        root.leak_checks = crate::leak::LeakChecks::Flag;
        let out = apply_noop(&root, &r(format!("text {ANT_TOKEN}"))).unwrap();
        assert!(out.applied, "flag mode warns instead of refusing");
        assert!(!out.warnings.is_empty());

        let v2 = fsops::version_of(format!("text {ANT_TOKEN}\n").as_bytes());
        root.leak_checks = crate::leak::LeakChecks::Off;
        let out = apply_noop(
            &root,
            &req(
                vec![base("doc.md", &v2)],
                vec![EditOp::AnchorReplace {
                    path: "doc.md".into(),
                    old_text: "text ".into(),
                    new_text: format!("also {ANT_TOKEN} and "),
                    occurrence: None,
                }],
            ),
        )
        .unwrap();
        assert!(out.applied && out.warnings.is_empty(), "off means off");
    }
}
