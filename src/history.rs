//! The read path over the journal (R6): `journal`, `diff`, `revert`.
//!
//! R6 has promised durable attributed history since sprint 001, but until
//! this module the only way to redeem it was to ssh to the host and open
//! SQLite — which made it redeemable by Ken and not by the agents the
//! record exists for. `journal.rs` holds the SQL; everything an agent
//! actually sees is decided here.
//!
//! Two rules run through the whole module.
//!
//! **Redaction is enforced at the materialisation boundary** (D-3), not
//! per tool. [`materialise`] is the single door between the blob store and
//! a response, and it redacts a legacy plaintext blob whose path is
//! classified *today* — a case that is live rather than theoretical, since
//! `**/*.env` only became a classification rule in 008 (D-11's corollary).
//!
//! **A partial answer never looks whole.** Every `journal` response states
//! what this history cannot see: reads are not journaled at all (D-2), and
//! failure records begin later than transaction records do. The same rule
//! `files_searched` (#1066) and `denied_hidden` (R7) exist for, applied to
//! time instead of to files.

use crate::config::{Limits, ResolvedRoot};
use crate::errors::{KaedError, Result};
use crate::fsops::{self, Secrecy};
use crate::journal::{Coverage, HistoryFilter, Journal, TxnRow};
use crate::policy;
use crate::txn::{self, BaseVersion, EditOp, EditOutcome, EditRequest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Cap on `journal` entries per call, whatever `max` asks for.
const MAX_ENTRIES_CEILING: usize = 200;
pub const DEFAULT_MAX_ENTRIES: usize = 20;

/// What kaed's history structurally cannot answer. Stated in every
/// `journal` response rather than left for an agent to discover (D-2).
const READS_NOT_JOURNALED: &str = "reads are not journaled: this history covers applied write transactions, failed \
     write attempts and feedback only. A refused, truncated or abandoned *read* leaves \
     no row here, so silence about read-side friction is not evidence of its absence.";

// ------------------------------------------------------------ entry shapes

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    /// A write transaction that was applied (or begun and torn).
    Txn,
    /// A write attempt that never applied.
    Failure,
    /// A friction report filed through `feedback`.
    Feedback,
    /// A secrets-audit event (011): generate / rotate / reveal / transport.
    Secret,
}

/// Why a journalled row names a root this host cannot resolve today.
///
/// R8's corollary: history outlives root names. The rows are true, and are
/// neither rewritten nor aliased back into existence — they are labelled,
/// and `revert` refuses them saying why. Live case: kai's journal holds
/// five transactions naming a `home` root that sprint 002 removed.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Historical {
    /// `unqualified_pre_007` | `root_no_longer_served`
    pub reason: &'static str,
    pub note: String,
}

fn historical(root_name: &str, live: &[ResolvedRoot], host: &str) -> Option<Historical> {
    if live.iter().any(|r| r.name == root_name) {
        return None;
    }
    if root_name.contains(':') {
        Some(Historical {
            reason: "root_no_longer_served",
            note: format!(
                "this row names root {root_name:?}, which {host} no longer serves. The row is \
                 true as written and is not rewritten or aliased; `revert` refuses it, and \
                 `diff` can only reach content whose path also resolves under a live root."
            ),
        })
    } else {
        Some(Historical {
            reason: "unqualified_pre_007",
            note: format!(
                "this row predates sprint 007 and names an unqualified root {root_name:?}; \
                 root names are host-qualified ({host}:{root_name}) since then, and the old \
                 spelling is deliberately not accepted as an alias (R8)."
            ),
        })
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TxnFileEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_version: Option<String>,
    pub new_version: String,
    pub lines_added: i64,
    pub lines_removed: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Txn {
        txn_id: i64,
        author: String,
        /// RFC 3339, the time the transaction began.
        time: String,
        /// `applied` — every rename landed. `torn` — begun and never
        /// completed, so some files may not have been written. A torn row
        /// is the startup warning `scan_pending` reports, surfaced here
        /// rather than only in the daemon's log.
        status: &'static str,
        root: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        historical: Option<Historical>,
        #[serde(skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        files: Vec<TxnFileEntry>,
        /// `+added −removed` across the transaction.
        diffstat: (i64, i64),
        #[serde(skip_serializing_if = "Option::is_none")]
        git_head: Option<String>,
    },
    Failure {
        failure_id: i64,
        author: String,
        time: String,
        root: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        historical: Option<Historical>,
        #[serde(skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        paths: Vec<String>,
        code: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expected_version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        actual_version: Option<String>,
    },
    Feedback {
        feedback_id: i64,
        author: String,
        time: String,
        category: String,
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    /// One secrets-audit event: what happened to a value, never the value.
    /// `disclosed: true` means it left kaed — toward the calling agent
    /// (`reveal`) or another host (`transport`, with the claimed
    /// `destination`). This stream is how "has any agent ever seen this
    /// secret?" gets answered.
    Secret {
        event_id: i64,
        author: String,
        time: String,
        /// `generate` | `rotate` | `reveal` | `transport`
        action: String,
        root: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        historical: Option<Historical>,
        path: String,
        key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_digest: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_digest: Option<String>,
        disclosed: bool,
        /// The caller's claim about where the value went (D-6): kaed
        /// cannot verify bytes after they leave, and says so here rather
        /// than implying otherwise.
        #[serde(skip_serializing_if = "Option::is_none")]
        destination: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        txn_id: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
    },
}

impl Entry {
    fn time(&self) -> &str {
        match self {
            Entry::Txn { time, .. }
            | Entry::Failure { time, .. }
            | Entry::Feedback { time, .. }
            | Entry::Secret { time, .. } => time,
        }
    }
}

/// What this history does and does not cover, in the response rather than
/// in a doc an agent will not read (D-2).
#[derive(Debug, Serialize, JsonSchema)]
pub struct CoverageInfo {
    /// Earliest transaction on record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txns_from: Option<String>,
    /// Earliest *failed attempt* on record. Later than `txns_from` on any
    /// host that ran kaed before #910 — failures were not recorded at all
    /// until then, so a window before this is silent, not clean.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failures_from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_from: Option<String>,
    /// Earliest secrets-audit event (011). Same rule as `failures_from`:
    /// the stream begins here, and a window reaching back further is
    /// silent about disclosures, not clean of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secrets_from: Option<String>,
    /// Days blob *content* survives. Transaction metadata is kept forever;
    /// past this window `diff` can name a version it can no longer render.
    pub blob_retention_days: u32,
    /// Always non-empty: the first note says reads are not journaled.
    pub notes: Vec<String>,
}

/// Why an empty result is empty, when the emptiness is more likely the
/// caller's than the store's. Same mechanism `search` and `list` grew in
/// 007 after a silent zero cost a wrong conclusion (#1066).
#[derive(Debug, Serialize, JsonSchema)]
pub struct Reason {
    pub code: &'static str,
    pub hint: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct JournalResult {
    pub entries: Vec<Entry>,
    /// More records matched than `max` returned.
    pub truncated: bool,
    /// Matching records the query pulled across all three kinds, before
    /// merging and `max` cut it down (so it is itself bounded by `max`, and
    /// `truncated` is what says there were more). Always present, including
    /// zero, so "nothing matched" never has to be inferred from an empty
    /// list — the `files_searched` rule (#1066) at the query level.
    pub records_scanned: usize,
    pub coverage: CoverageInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
}

// ------------------------------------------------------------- `journal`

pub struct JournalQuery<'a> {
    /// Optional by design (D-6): `journal` reads a host-wide store rather
    /// than addressing a path, and history names roots that no longer
    /// resolve. A value that does not resolve filters rather than fails.
    pub root: Option<&'a str>,
    pub path: Option<&'a str>,
    pub author: Option<&'a str>,
    pub since: Option<&'a str>,
    pub kinds: Vec<RecordKind>,
    pub max: usize,
}

pub fn journal(
    j: &Journal,
    roots: &[ResolvedRoot],
    host: &str,
    q: &JournalQuery<'_>,
) -> Result<JournalResult> {
    let max = q.max.clamp(1, MAX_ENTRIES_CEILING);
    let kinds = if q.kinds.is_empty() {
        vec![
            RecordKind::Txn,
            RecordKind::Failure,
            RecordKind::Feedback,
            RecordKind::Secret,
        ]
    } else {
        q.kinds.clone()
    };
    // Over-fetch by one per kind so `truncated` reflects the store rather
    // than the arithmetic of merging three sources.
    let filter = HistoryFilter {
        root: q.root,
        path: q.path,
        author: q.author,
        since: q.since,
        limit: max + 1,
    };

    let mut entries: Vec<Entry> = Vec::new();
    let mut scanned = 0usize;
    if kinds.contains(&RecordKind::Txn) {
        let rows = j.txns(&filter)?;
        scanned += rows.len();
        entries.extend(rows.into_iter().map(|t| txn_entry(t, roots, host)));
    }
    if kinds.contains(&RecordKind::Failure) {
        let rows = j.failures(&filter)?;
        scanned += rows.len();
        entries.extend(rows.into_iter().map(|f| Entry::Failure {
            failure_id: f.id,
            author: f.author,
            time: f.failed_at,
            historical: historical(&f.root, roots, host),
            root: f.root,
            intent: f.intent.as_deref().map(redact_note),
            paths: f.paths,
            code: f.code,
            message: redact_note(&f.message),
            expected_version: f.expected_version,
            actual_version: f.actual_version,
        }));
    }
    // A friction report is about the contract, not about a file, so a
    // path- or root-scoped query is not asking for one.
    if kinds.contains(&RecordKind::Feedback) && q.path.is_none() && q.root.is_none() {
        let rows = j.feedback(&filter)?;
        scanned += rows.len();
        entries.extend(rows.into_iter().map(|f| Entry::Feedback {
            feedback_id: f.id,
            author: f.author,
            time: f.created_at,
            category: f.category,
            summary: redact_note(&f.summary),
            detail: f.detail.as_deref().map(redact_note),
            context: f.context.as_deref().map(redact_note),
        }));
    }
    if kinds.contains(&RecordKind::Secret) {
        let rows = j.secret_events(&filter)?;
        scanned += rows.len();
        entries.extend(rows.into_iter().map(|e| Entry::Secret {
            event_id: e.id,
            author: e.author,
            time: e.created_at,
            action: e.action,
            historical: historical(&e.root, roots, host),
            root: e.root,
            path: e.path,
            key: e.key,
            old_digest: e.old_digest,
            new_digest: e.new_digest,
            disclosed: e.disclosed,
            destination: e.destination,
            txn_id: e.txn_id,
            intent: e.intent.as_deref().map(redact_note),
        }));
    }

    entries.sort_by(|a, b| b.time().cmp(a.time()));
    let truncated = entries.len() > max;
    entries.truncate(max);

    let coverage = coverage_info(j, q.since)?;
    let reason = (entries.is_empty())
        .then(|| empty_reason(q, roots))
        .flatten();
    Ok(JournalResult {
        entries,
        truncated,
        records_scanned: scanned,
        coverage,
        reason,
    })
}

/// Free text on its way out of the store. Redacted at write since 009 too,
/// but kai's and kubs0's journals already hold rows written by a kaed that
/// only ever wrote `intent` and never served it — this covers those.
///
/// The leak this closes was found by the sprint's own gate test rather
/// than reasoned about in advance: an agent whose `intent` reads "rotating
/// KLAMS_TOKEN to <the token>" was writing plaintext straight past a
/// redaction model that only ever looked at file *content*.
fn redact_note(s: &str) -> String {
    crate::secrets::redact_free_text(s)
}

fn txn_entry(t: TxnRow, roots: &[ResolvedRoot], host: &str) -> Entry {
    let diffstat = t
        .files
        .iter()
        .fold((0, 0), |(a, r), f| (a + f.lines_added, r + f.lines_removed));
    Entry::Txn {
        txn_id: t.id,
        author: t.author,
        time: t.started_at,
        status: if t.completed_at.is_some() {
            "applied"
        } else {
            "torn"
        },
        historical: historical(&t.root, roots, host),
        root: t.root,
        intent: t.intent.as_deref().map(redact_note),
        files: t
            .files
            .into_iter()
            .map(|f| TxnFileEntry {
                path: f.path,
                old_version: f.old_version,
                new_version: f.new_version,
                lines_added: f.lines_added,
                lines_removed: f.lines_removed,
            })
            .collect(),
        diffstat,
        git_head: t.git_head,
    }
}

fn coverage_info(j: &Journal, since: Option<&str>) -> Result<CoverageInfo> {
    let Coverage {
        txns_from,
        failures_from,
        feedback_from,
        secrets_from,
    } = j.coverage()?;
    let mut notes = vec![READS_NOT_JOURNALED.to_string()];
    // The dated boundary #910 left behind: on any host that ran kaed
    // before it, a window reaching back past `failures_from` is silent
    // about failures rather than clean of them.
    let window_predates_failures = match (&failures_from, since) {
        (Some(first), Some(since)) => since < first.as_str(),
        (Some(first), None) => txns_from.as_deref().is_some_and(|t| t < first.as_str()),
        (None, _) => txns_from.is_some(),
    };
    if window_predates_failures {
        notes.push(match &failures_from {
            Some(first) => format!(
                "failed attempts were only recorded from {first} (korg #910); this window \
                 reaches back further, and an absence of failures before that time is not \
                 evidence there were none."
            ),
            None => "no failed attempt has ever been recorded on this host; if its journal \
                     predates korg #910, early failures were never written at all."
                .to_string(),
        });
    }
    Ok(CoverageInfo {
        txns_from,
        failures_from,
        feedback_from,
        secrets_from,
        blob_retention_days: j.blob_retention_days(),
        notes,
    })
}

fn empty_reason(q: &JournalQuery<'_>, roots: &[ResolvedRoot]) -> Option<Reason> {
    if let Some(root) = q.root
        && !roots.iter().any(|r| r.name == root)
    {
        let known: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
        return Some(Reason {
            code: "unknown_root_filter",
            hint: format!(
                "no record names root {root:?}, and no live root does either (this host \
                 serves {known:?}). `journal` filters on the name as journalled, so a \
                 removed root's history is reachable — but a typo looks exactly like a \
                 quiet week, which is why this says so."
            ),
        });
    }
    Some(Reason {
        code: "no_matching_records",
        hint: "no record matched. `path` is matched against root-relative paths exactly \
               or as a subtree prefix, and only *write* activity is journalled at all — \
               see coverage.notes."
            .to_string(),
    })
}

// ---------------------------------------------------------------- `diff`

/// One side of a diff, after policy and redaction have had their say.
struct Material {
    text: String,
    version: String,
    redacted: bool,
    source: &'static str,
}

/// Which state of a file a `from`/`to` names.
#[derive(Debug, PartialEq, Eq)]
pub enum Side {
    /// A content address (R1).
    Version(String),
    /// The state a transaction *produced* for this path.
    Txn(i64),
    /// The working tree right now.
    Current,
}

/// Parse a `from`/`to` selector. A version is 16 hex chars (R1), which is
/// what makes this unambiguous without a type tag.
pub fn parse_side(s: &str) -> Result<Side> {
    if s == "current" {
        return Ok(Side::Current);
    }
    if s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(Side::Version(s.to_owned()));
    }
    if let Ok(id) = s.parse::<i64>() {
        return Ok(Side::Txn(id));
    }
    Err(KaedError::invalid_input(format!(
        "{s:?} is not a version, a txn id or \"current\": a version is 16 hex characters \
         (as returned by read/search/stat), a txn id is the integer `journal` reports, \
         and \"current\" is the working tree"
    )))
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DiffResult {
    pub diff: String,
    pub path: String,
    pub from_version: String,
    pub to_version: String,
    /// Either side is a redacted rendering rather than the file's bytes.
    /// True whenever the path is classified — and also for a legacy
    /// plaintext blob that was redacted on read (D-3).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
    /// `journal_blob` or `working_tree`, per side.
    pub from_source: &'static str,
    pub to_source: &'static str,
}

pub fn diff(
    root: &ResolvedRoot,
    rel: &str,
    from: &Side,
    to: &Side,
    j: &Journal,
    limits: &Limits,
) -> Result<DiffResult> {
    // Lexical policy first, exactly as R7 requires: applied identically to
    // paths that exist and paths that no longer do, so a refusal is never
    // evidence about the working tree.
    let abs = fsops::resolve_creatable(root, rel)?;
    let classified = root.classify.classified_by(&abs).is_some();

    let from = materialise(root, rel, classified, from, j, limits)?;
    let to = materialise(root, rel, classified, to, j, limits)?;
    Ok(DiffResult {
        diff: txn::unified_diff(&from.text, &to.text, rel),
        path: rel.to_owned(),
        from_version: from.version,
        to_version: to.version,
        redacted: from.redacted || to.redacted,
        from_source: from.source,
        to_source: to.source,
    })
}

/// **The single door between the journal's blob store and a response**
/// (D-3). Every history surface goes through here, so redaction is a
/// property of the store boundary rather than a rule each tool remembers.
/// `classified` is whether the path is classified **today** — the question
/// that matters, since a blob predating the rule is still that file's
/// content (D-3).
fn materialise(
    root: &ResolvedRoot,
    rel: &str,
    classified: bool,
    side: &Side,
    j: &Journal,
    limits: &Limits,
) -> Result<Material> {
    let version = match side {
        Side::Current => {
            let loaded = fsops::load_text(root, rel, limits)?;
            let text = match &loaded.secrecy {
                Secrecy::Plain => loaded.content.clone(),
                Secrecy::ClassifiedDotenv { file, .. } => file.redact(),
            };
            return Ok(Material {
                redacted: !matches!(loaded.secrecy, Secrecy::Plain),
                text,
                version: loaded.version,
                source: "working_tree",
            });
        }
        Side::Version(v) => v.clone(),
        Side::Txn(id) => txn_version_for(j, *id, rel)?,
    };

    let Some((content, was_redacted)) = crate::txn::TxnRecorder::blob(j, &version) else {
        return Err(KaedError::not_found(format!(
            "no retained content for version {version} of {rel}. Transaction metadata is \
             kept forever but blob content expires after {} days (korg #909), so an old \
             version can be named and not rendered — `journal` still shows what changed \
             and when",
            j.blob_retention_days()
        ))
        .with_data(serde_json::json!({
            "reason": "blob_expired_or_absent",
            "path": rel,
            "version": version,
            "blob_retention_days": j.blob_retention_days(),
        })));
    };

    // The in-file marker is content-level, so check the *historical*
    // content too: a file that opted out and was later deleted would
    // otherwise have a readable history (D-3).
    if policy::has_ignore_marker(&content) {
        return Err(KaedError::refused(
            rel,
            "in-file `kaedignore` marker",
            crate::errors::RefusalReason::InFileMarker,
            "the journalled content of this version carries a `kaedignore` comment in its \
             first 5 lines, so kaed will not serve it back",
        ));
    }

    // D-11's corollary. A blob written before 008 is raw; if its path is
    // classified today, redact on read — never serve it. `**/*.env` only
    // became a rule in 008, so pre-008 journals hold real plaintext under
    // paths that are classified now.
    if classified && !was_redacted {
        let Some(view) = crate::dotenv::parse(&content).map(|f| f.redact()) else {
            return Ok(Material {
                text: "(content withheld: classified, and this journalled version is not \
                       dotenv-shaped, so kaed has no redacted rendering for it)\n"
                    .to_owned(),
                version,
                redacted: true,
                source: "journal_blob",
            });
        };
        return Ok(Material {
            text: view,
            version,
            redacted: true,
            source: "journal_blob",
        });
    }
    Ok(Material {
        text: content,
        version,
        redacted: was_redacted,
        source: "journal_blob",
    })
}

/// The version a transaction produced for a path. A txn id names the state
/// it *left behind*; to see what one transaction did, `journal` reports
/// that file's `old_version` and `new_version` — pass those.
fn txn_version_for(j: &Journal, id: i64, rel: &str) -> Result<String> {
    let Some(row) = j.txn(id)? else {
        return Err(KaedError::not_found(format!(
            "no transaction {id} in this host's journal"
        )));
    };
    row.files
        .iter()
        .find(|f| f.path == rel)
        .map(|f| f.new_version.clone())
        .ok_or_else(|| {
            let touched: Vec<&str> = row.files.iter().map(|f| f.path.as_str()).collect();
            KaedError::not_found(format!(
                "transaction {id} did not touch {rel:?}; it touched {touched:?}"
            ))
        })
}

// -------------------------------------------------------------- `revert`

pub struct RevertRequest<'a> {
    pub txn_id: i64,
    pub dry_run: bool,
    /// The agent's own reason, appended to the automatic "revert of txn N".
    pub intent: Option<&'a str>,
    /// The identity the revert is journalled under — its own, not that of
    /// the transaction being undone.
    pub author: &'a str,
    /// Leak-match overrides (012 D-2), passed through to the engine: a
    /// revert that re-introduces a secret the current content lacks is a
    /// re-leak, and refuses like any other write until named here.
    pub allow_secrets: &'a [String],
}

/// Undo a transaction as a **new** transaction. Never history rewriting:
/// it runs through `txn::apply` with `base` set to the version the target
/// transaction produced, so a file that moved since gets a
/// `version_conflict` with a delta like any other edit, and the revert is
/// itself journalled and itself revertible.
pub fn revert(
    root: &ResolvedRoot,
    req: &RevertRequest<'_>,
    j: &Journal,
    limits: &Limits,
    host: &str,
    roots: &[ResolvedRoot],
) -> Result<EditOutcome> {
    let RevertRequest {
        txn_id,
        dry_run,
        intent,
        author,
        allow_secrets,
    } = *req;
    let Some(row) = j.txn(txn_id)? else {
        return Err(KaedError::not_found(format!(
            "no transaction {txn_id} in this host's journal"
        )));
    };
    let refuse = |reason: &'static str, message: String, extra: serde_json::Value| {
        let mut data = serde_json::json!({ "reason": reason, "txn_id": txn_id });
        if let (Some(b), Some(e)) = (data.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        KaedError::invalid_input(message).with_data(data)
    };

    // R8's corollary, spelled out in the contract: a row naming a root
    // this host no longer serves is true and is not aliased back into
    // existence, so it cannot be reverted.
    if let Some(h) = historical(&row.root, roots, host) {
        return Err(refuse(
            h.reason,
            format!(
                "transaction {txn_id} was made against root {:?}, which {host} no longer \
                 serves — reverting it would mean resolving a root name that is history. \
                 {} Use `diff` to see what it did, and a fresh `edit` under a live root if \
                 the change should be undone.",
                row.root, h.note
            ),
            serde_json::json!({ "journalled_root": row.root }),
        ));
    }
    if row.root != root.name {
        return Err(refuse(
            "wrong_root",
            format!(
                "transaction {txn_id} was made against root {:?}, not {:?}",
                row.root, root.name
            ),
            serde_json::json!({ "journalled_root": row.root }),
        ));
    }
    if row.files.is_empty() {
        return Err(refuse(
            "nothing_to_revert",
            format!("transaction {txn_id} recorded no files"),
            serde_json::json!({}),
        ));
    }

    let mut base = Vec::new();
    let mut ops = Vec::new();
    for f in &row.files {
        // Undoing a create is a delete, and the `delete` op is a later
        // slice. Refuse with the reason named rather than skip the file
        // and report a partial revert as a whole one.
        let Some(old_version) = &f.old_version else {
            return Err(refuse(
                "revert_of_create_needs_delete",
                format!(
                    "transaction {txn_id} created {:?}; undoing a create means deleting the \
                     file, and kaed has no `delete` op yet. Delete it outside kaed, or leave \
                     it — `journal` records that the create happened either way.",
                    f.path
                ),
                serde_json::json!({ "path": f.path }),
            ));
        };
        let abs = fsops::resolve_creatable(root, &f.path)?;
        // D-4: for a classified file the retained blob is a *rendering*.
        // Restoring it would write `⟨kaed:KEY@digest⟩` into the file as a
        // literal — destroying the value while reporting success, which is
        // precisely what the D-10 vanish guard exists to prevent.
        if root.classify.classified_by(&abs).is_some() {
            return Err(refuse(
                "no_plaintext_history",
                format!(
                    "{:?} is classified, so what the journal retains for it is a redacted \
                     rendering, not its bytes (008 D-11: kaed keeps no plaintext shadow). \
                     Restoring that would write placeholders into the file as literals. Use \
                     `diff` to see which keys changed, then set them explicitly with env ops.",
                    f.path
                ),
                serde_json::json!({ "path": f.path }),
            ));
        }
        let Some((content, was_redacted)) = crate::txn::TxnRecorder::blob(j, old_version) else {
            return Err(refuse(
                "blob_expired_or_absent",
                format!(
                    "the pre-image of {:?} (version {old_version}) is no longer retained — \
                     blob content expires after {} days (korg #909) while the metadata above \
                     is kept forever. `journal` still shows what changed; the content to \
                     restore is gone.",
                    f.path,
                    j.blob_retention_days()
                ),
                serde_json::json!({ "path": f.path, "version": old_version }),
            ));
        };
        if was_redacted {
            return Err(refuse(
                "no_plaintext_history",
                format!(
                    "the pre-image of {:?} was journalled as a redacted rendering, so kaed \
                     does not hold the bytes a revert would restore",
                    f.path
                ),
                serde_json::json!({ "path": f.path }),
            ));
        }
        base.push(BaseVersion {
            path: f.path.clone(),
            version: f.new_version.clone(),
        });
        ops.push(EditOp::Create {
            path: f.path.clone(),
            content,
            executable: false,
            overwrite: true,
        });
    }

    let intent = match intent {
        Some(i) => format!("revert of txn {txn_id}: {i}"),
        None => format!("revert of txn {txn_id}"),
    };
    txn::apply(
        root,
        &EditRequest {
            base,
            ops,
            dry_run,
            return_diff: true,
            intent: Some(intent),
            drop_keys: Vec::new(),
            allow_secrets: allow_secrets.to_vec(),
        },
        limits,
        author,
        j,
    )
}

// ------------------------------------------------------------- `feedback`

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCategory {
    /// The contract got in the way. The default, because it is the report
    /// worth having and a category choice is a thinking step.
    #[default]
    Friction,
    Bug,
    Wish,
    Praise,
}

impl FeedbackCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Friction => "friction",
            Self::Bug => "bug",
            Self::Wish => "wish",
            Self::Praise => "praise",
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FeedbackResult {
    pub id: i64,
    pub recorded: bool,
    /// Says what happened to the text, because it is not stored verbatim.
    pub note: &'static str,
}

/// File a friction report into the same store as the events it is about,
/// attributed and timestamped like everything else (R6).
///
/// Free text is redacted first: the likeliest thing an agent pastes into a
/// report is the error it just got, and every other derived surface in
/// kaed is already redacted (D-5).
pub fn feedback(
    j: &Journal,
    author: &str,
    category: FeedbackCategory,
    summary: &str,
    detail: Option<&str>,
    context: Option<&str>,
) -> Result<FeedbackResult> {
    let summary = summary.trim();
    if summary.is_empty() {
        return Err(KaedError::invalid_input(
            "feedback needs a summary — one sentence is enough, and it is the only \
             required field",
        ));
    }
    let id = j.add_feedback(
        author,
        category.as_str(),
        &redact_note(summary),
        detail.map(redact_note).as_deref(),
        context.map(redact_note).as_deref(),
    )?;
    Ok(FeedbackResult {
        id,
        recorded: true,
        note: "filed into this host's journal beside the transactions and failures it is \
               about; read it back with `journal` (kind: feedback). Secret-shaped tokens \
               in the text were replaced with placeholders before storage.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedRoot;

    fn root_at(name: &str, dir: &std::path::Path) -> ResolvedRoot {
        ResolvedRoot::unrestricted(name, dir.canonicalize().unwrap())
    }

    fn edit_file(
        root: &ResolvedRoot,
        j: &Journal,
        rel: &str,
        old: &str,
        new: &str,
        intent: Option<&str>,
    ) -> EditOutcome {
        let version = fsops::version_of(std::fs::read(root.path.join(rel)).unwrap().as_slice());
        txn::apply(
            root,
            &EditRequest {
                base: vec![BaseVersion {
                    path: rel.into(),
                    version,
                }],
                ops: vec![EditOp::AnchorReplace {
                    path: rel.into(),
                    old_text: old.into(),
                    new_text: new.into(),
                    occurrence: None,
                }],
                dry_run: false,
                return_diff: false,
                intent: intent.map(str::to_owned),
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "claude",
            j,
        )
        .unwrap()
    }

    fn query(max: usize) -> JournalQuery<'static> {
        JournalQuery {
            root: None,
            path: None,
            author: None,
            since: None,
            kinds: Vec::new(),
            max,
        }
    }

    // ----------------------------------------------------------- journal

    #[test]
    fn journal_merges_txns_failures_and_feedback_into_one_stream() {
        // D-1: "what did agents complain about, and what were they doing
        // when they did" is one call, not three.
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        edit_file(&root, &j, "f.txt", "first", "second", Some("the edit"));
        // a failed attempt: stale base
        let _ = txn::apply(
            &root,
            &EditRequest {
                base: vec![BaseVersion {
                    path: "f.txt".into(),
                    version: fsops::version_of(b"stale"),
                }],
                ops: vec![EditOp::AnchorReplace {
                    path: "f.txt".into(),
                    old_text: "second".into(),
                    new_text: "third".into(),
                    occurrence: None,
                }],
                dry_run: false,
                return_diff: false,
                intent: None,
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "claude",
            &j,
        );
        feedback(
            &j,
            "claude",
            FeedbackCategory::Friction,
            "the conflict delta did not help",
            None,
            None,
        )
        .unwrap();

        let out = journal(&j, std::slice::from_ref(&root), "kai", &query(20)).unwrap();
        let kinds: Vec<&str> = out
            .entries
            .iter()
            .map(|e| match e {
                Entry::Txn { .. } => "txn",
                Entry::Failure { .. } => "failure",
                Entry::Feedback { .. } => "feedback",
                Entry::Secret { .. } => "secret",
            })
            .collect();
        assert!(kinds.contains(&"txn"), "{kinds:?}");
        assert!(kinds.contains(&"failure"), "{kinds:?}");
        assert!(kinds.contains(&"feedback"), "{kinds:?}");
        assert!(out.reason.is_none());
        assert_eq!(out.records_scanned, 3);
    }

    #[test]
    fn every_response_says_reads_are_not_journaled() {
        // D-2: the gap is accepted, and disclosed in the response rather
        // than left for an agent to discover.
        let j = Journal::open_in_memory().unwrap();
        let out = journal(&j, &[], "kai", &query(20)).unwrap();
        assert!(
            out.coverage.notes[0].contains("reads are not journaled"),
            "{:?}",
            out.coverage.notes
        );
    }

    #[test]
    fn a_window_predating_the_failure_log_says_so() {
        // kai's failure record starts when #910 shipped; txns #1–#5 are
        // older. A window reaching back that far is silent about failures,
        // not clean of them, and must say which.
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        edit_file(&root, &j, "f.txt", "first", "second", None);
        // age the transaction to before any failure was ever recorded
        j.raw_execute_for_test("UPDATE txns SET started_at = '2026-08-01T00:00:00Z'");
        j.raw_execute_for_test(
            "INSERT INTO txn_failures (author, intent, root, paths, code, message, failed_at)
             VALUES ('claude', NULL, 'kai:t', 'f.txt', 'version_conflict', 'x',
                     '2026-08-02T18:02:00Z')",
        );

        let out = journal(&j, std::slice::from_ref(&root), "kai", &query(20)).unwrap();
        assert!(
            out.coverage.notes.iter().any(|n| n.contains("#910")),
            "{:?}",
            out.coverage.notes
        );
        assert_eq!(
            out.coverage.failures_from.as_deref(),
            Some("2026-08-02T18:02:00Z")
        );
    }

    #[test]
    fn a_row_naming_a_removed_root_is_labelled_historical_not_rewritten() {
        // R8's corollary. kai's journal really does hold five transactions
        // naming a `home` root sprint 002 removed.
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:home", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        edit_file(&root, &j, "f.txt", "first", "second", None);

        // now this host serves a different set
        let live = [root_at("kai:src", dir.path())];
        let out = journal(&j, &live, "kai", &query(20)).unwrap();
        let Entry::Txn {
            historical,
            root: r,
            ..
        } = &out.entries[0]
        else {
            panic!("expected a txn entry: {:?}", out.entries[0])
        };
        assert_eq!(r, "kai:home", "the row is not rewritten");
        assert_eq!(
            historical.as_ref().map(|h| h.reason),
            Some("root_no_longer_served")
        );
    }

    #[test]
    fn a_pre_007_unqualified_root_is_labelled_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("home", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        edit_file(&root, &j, "f.txt", "first", "second", None);

        let live = [root_at("kai:src", dir.path())];
        let out = journal(&j, &live, "kai", &query(20)).unwrap();
        let Entry::Txn { historical, .. } = &out.entries[0] else {
            panic!("expected a txn entry")
        };
        assert_eq!(
            historical.as_ref().map(|h| h.reason),
            Some("unqualified_pre_007")
        );
    }

    #[test]
    fn an_empty_result_from_a_bad_root_filter_explains_itself() {
        // D-6 / #1066: a typo must not look like a quiet week.
        let dir = tempfile::tempdir().unwrap();
        let live = [root_at("kai:src", dir.path())];
        let j = Journal::open_in_memory().unwrap();
        let out = journal(
            &j,
            &live,
            "kai",
            &JournalQuery {
                root: Some("kai:srcc"),
                ..query(20)
            },
        )
        .unwrap();
        assert!(out.entries.is_empty());
        assert_eq!(
            out.reason.as_ref().map(|r| r.code),
            Some("unknown_root_filter")
        );
    }

    #[test]
    fn a_torn_transaction_reads_as_torn_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        edit_file(&root, &j, "f.txt", "first", "second", None);
        j.raw_execute_for_test("UPDATE txns SET completed_at = NULL");

        let out = journal(&j, std::slice::from_ref(&root), "kai", &query(20)).unwrap();
        let Entry::Txn { status, .. } = &out.entries[0] else {
            panic!("expected a txn entry")
        };
        assert_eq!(*status, "torn");
    }

    #[test]
    fn a_path_filter_matches_a_subtree_and_keeps_the_whole_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "a\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
        txn::apply(
            &root,
            &EditRequest {
                base: vec![
                    BaseVersion {
                        path: "src/a.rs".into(),
                        version: fsops::version_of(b"a\n"),
                    },
                    BaseVersion {
                        path: "b.txt".into(),
                        version: fsops::version_of(b"b\n"),
                    },
                ],
                ops: vec![
                    EditOp::AnchorReplace {
                        path: "src/a.rs".into(),
                        old_text: "a".into(),
                        new_text: "A".into(),
                        occurrence: None,
                    },
                    EditOp::AnchorReplace {
                        path: "b.txt".into(),
                        old_text: "b".into(),
                        new_text: "B".into(),
                        occurrence: None,
                    },
                ],
                dry_run: false,
                return_diff: false,
                intent: None,
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap();

        let out = journal(
            &j,
            std::slice::from_ref(&root),
            "kai",
            &JournalQuery {
                path: Some("src"),
                ..query(20)
            },
        )
        .unwrap();
        let Entry::Txn { files, .. } = &out.entries[0] else {
            panic!("expected a txn entry")
        };
        assert_eq!(
            files.len(),
            2,
            "a path filter selects transactions, and must not hide the other files one touched"
        );
    }

    // -------------------------------------------------------------- diff

    #[test]
    fn diff_between_a_journalled_version_and_current() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let v1 = fsops::version_of(b"first\n");
        edit_file(&root, &j, "f.txt", "first", "second", None);

        let out = diff(
            &root,
            "f.txt",
            &Side::Version(v1.clone()),
            &Side::Current,
            &j,
            &Limits::default(),
        )
        .unwrap();
        assert!(out.diff.contains("-first"), "{}", out.diff);
        assert!(out.diff.contains("+second"), "{}", out.diff);
        assert_eq!(out.from_version, v1);
        assert_eq!(out.from_source, "journal_blob");
        assert_eq!(out.to_source, "working_tree");
        assert!(!out.redacted);
    }

    #[test]
    fn a_txn_id_names_the_state_that_transaction_produced() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let out = edit_file(&root, &j, "f.txt", "first", "second", None);
        let id = out.txn_id.unwrap();

        let d = diff(
            &root,
            "f.txt",
            &Side::Txn(id),
            &Side::Current,
            &j,
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(d.from_version, out.files[0].new_version);
        assert_eq!(d.diff, "", "the txn's output is the current content");
    }

    #[test]
    fn a_txn_that_never_touched_the_path_says_what_it_did_touch() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        std::fs::write(dir.path().join("g.txt"), "g\n").unwrap();
        let out = edit_file(&root, &j, "f.txt", "first", "second", None);

        let err = diff(
            &root,
            "g.txt",
            &Side::Txn(out.txn_id.unwrap()),
            &Side::Current,
            &j,
            &Limits::default(),
        )
        .unwrap_err();
        assert!(err.message.contains("f.txt"), "{}", err.message);
    }

    #[test]
    fn an_expired_blob_explains_retention_instead_of_shrugging() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::in_memory_with_retention(7).unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let v1 = fsops::version_of(b"first\n");
        edit_file(&root, &j, "f.txt", "first", "second", None);
        j.raw_execute_for_test("UPDATE blobs SET created_at = '2020-01-01T00:00:00Z'");
        assert!(j.gc_blobs().unwrap() > 0);

        let err = diff(
            &root,
            "f.txt",
            &Side::Version(v1),
            &Side::Current,
            &j,
            &Limits::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::NotFound);
        assert_eq!(
            err.data.as_ref().unwrap()["reason"],
            "blob_expired_or_absent"
        );
        assert!(err.message.contains("kept forever"), "{}", err.message);
    }

    #[test]
    fn parse_side_distinguishes_versions_txn_ids_and_current() {
        assert_eq!(parse_side("current").unwrap(), Side::Current);
        assert_eq!(
            parse_side("9f3ac2d41b7e5860").unwrap(),
            Side::Version("9f3ac2d41b7e5860".into())
        );
        assert_eq!(parse_side("42").unwrap(), Side::Txn(42));
        assert!(parse_side("HEAD~1").is_err());
    }

    // ------------------------------------------------ the 008 hard gate

    /// The gate the proposal set on this sprint: these tools are precisely
    /// the mechanism that would turn a *retained* plaintext blob into a
    /// *served* one. A known secret must not appear in any history output.
    #[test]
    fn a_known_secret_never_appears_in_journal_or_diff_output() {
        const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::with_default_classify("kai:t", dir.path().canonicalize().unwrap());
        let j = Journal::open_in_memory().unwrap();
        let content = format!("KLAMS_TOKEN={VALUE}\n");
        std::fs::write(dir.path().join(".env"), &content).unwrap();
        let v1 = fsops::version_of(content.as_bytes());

        let out = txn::apply(
            &root,
            &EditRequest {
                base: vec![BaseVersion {
                    path: ".env".into(),
                    version: v1.clone(),
                }],
                ops: vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "NEW_FLAG".into(),
                    value: Some("on".into()),
                    value_from: None,
                    comment: None,
                }],
                dry_run: false,
                return_diff: false,
                intent: Some(format!("rotating to {VALUE}")),
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap();

        let listing = journal(&j, std::slice::from_ref(&root), "kai", &query(50)).unwrap();
        let rendered = serde_json::to_string(&listing).unwrap();
        assert!(!rendered.contains(VALUE), "journal leaked plaintext");

        for (from, to) in [
            (Side::Version(v1.clone()), Side::Current),
            (
                Side::Version(v1.clone()),
                Side::Version(out.files[0].new_version.clone()),
            ),
            (Side::Txn(out.txn_id.unwrap()), Side::Current),
        ] {
            let d = diff(&root, ".env", &from, &to, &j, &Limits::default()).unwrap();
            let rendered = serde_json::to_string(&d).unwrap();
            assert!(!rendered.contains(VALUE), "diff leaked plaintext: {d:?}");
            assert!(d.redacted, "diff of a classified file must say so: {d:?}");
        }
        // …and the real value is still on disk, untouched
        assert!(
            std::fs::read_to_string(dir.path().join(".env"))
                .unwrap()
                .contains(VALUE)
        );
    }

    /// D-11's corollary, and the reason it is not theoretical: `**/*.env`
    /// only became a classification rule in 008, so a pre-008 journal
    /// holds raw `korg.env` content under a path classified today.
    #[test]
    fn a_legacy_plaintext_blob_is_redacted_on_read() {
        const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::with_default_classify("kai:t", dir.path().canonicalize().unwrap());
        let j = Journal::open_in_memory().unwrap();
        let legacy = format!("KORG_TOKEN={VALUE}\n");
        let v = fsops::version_of(legacy.as_bytes());
        // exactly what a pre-008 kaed wrote: raw bytes, redacted = 0
        j.insert_legacy_blob_for_test(&v, &legacy);
        std::fs::write(
            dir.path().join("korg.env"),
            "KORG_TOKEN=rotated-value-here\n",
        )
        .unwrap();

        let d = diff(
            &root,
            "korg.env",
            &Side::Version(v),
            &Side::Current,
            &j,
            &Limits::default(),
        )
        .unwrap();
        assert!(!d.diff.contains(VALUE), "served a legacy plaintext blob");
        assert!(d.diff.contains("⟨kaed:KORG_TOKEN@"), "{}", d.diff);
        assert!(d.redacted);
    }

    /// Found by the gate test above, not reasoned about in advance: 008's
    /// redaction model looked only at file *content*, and `intent` is
    /// agent-supplied free text that sprint 009 turned from write-only into
    /// served. An agent writing "rotating KLAMS_TOKEN to <token>" walked
    /// straight past it.
    #[test]
    fn an_intent_carrying_a_token_is_redacted_in_the_store_and_on_read() {
        const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        edit_file(
            &root,
            &j,
            "f.txt",
            "first",
            "second",
            Some(&format!("rotating the value to {VALUE}")),
        );

        // redacted at the store, so a 0600 SQLite file no longer holds it
        let stored = j.txn(1).unwrap().unwrap().intent.unwrap();
        assert!(!stored.contains(VALUE), "plaintext reached the store");
        assert!(stored.contains("rotating the value to"), "{stored}");

        // …and again on the way out, which is what covers the rows kai and
        // kubs0 already wrote under a pre-009 kaed
        j.raw_execute_for_test(&format!(
            "UPDATE txns SET intent = 'legacy row with {VALUE} in it'"
        ));
        let out = journal(&j, std::slice::from_ref(&root), "kai", &query(5)).unwrap();
        let rendered = serde_json::to_string(&out).unwrap();
        assert!(
            !rendered.contains(VALUE),
            "served a pre-009 plaintext intent"
        );
    }

    /// D-3: a file that opted out of kaed and was later deleted would
    /// otherwise have a perfectly readable history.
    #[test]
    fn history_of_a_file_that_opted_out_via_the_marker_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        let content = "# kaedignore\nsomething private\n";
        let v = fsops::version_of(content.as_bytes());
        j.insert_legacy_blob_for_test(&v, content);
        // the file itself is gone from the working tree
        assert!(!dir.path().join("gone.txt").exists());

        let err = diff(
            &root,
            "gone.txt",
            &Side::Version(v),
            &Side::Version(fsops::version_of(b"x")),
            &j,
            &Limits::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::Denied);
        assert_eq!(err.data.as_ref().unwrap()["reason"], "in_file_marker");
    }

    #[test]
    fn feedback_text_is_redacted_before_storage() {
        let j = Journal::open_in_memory().unwrap();
        const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        feedback(
            &j,
            "claude",
            FeedbackCategory::Bug,
            "edit refused my token",
            Some(&format!("I passed {VALUE} and it broke")),
            None,
        )
        .unwrap();
        let rows = j
            .feedback(&HistoryFilter {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert!(!rows[0].detail.as_ref().unwrap().contains(VALUE));
        assert!(rows[0].detail.as_ref().unwrap().contains("⟨kaed:"));
    }

    #[test]
    fn feedback_needs_only_a_summary() {
        let j = Journal::open_in_memory().unwrap();
        let out = feedback(
            &j,
            "claude",
            FeedbackCategory::default(),
            "ambiguous_anchor gave me no way forward",
            None,
            None,
        )
        .unwrap();
        assert!(out.recorded);
        let rows = j
            .feedback(&HistoryFilter {
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows[0].category, "friction", "the default category");
    }

    #[test]
    fn empty_feedback_is_refused_with_the_one_field_named() {
        let j = Journal::open_in_memory().unwrap();
        let err =
            feedback(&j, "claude", FeedbackCategory::default(), "   ", None, None).unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::InvalidInput);
        assert!(err.message.contains("summary"));
    }

    // ------------------------------------------------------------ revert

    fn live(root: &ResolvedRoot) -> Vec<ResolvedRoot> {
        vec![root_at(&root.name, &root.path)]
    }

    #[test]
    fn revert_restores_content_as_a_new_journalled_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let out = edit_file(&root, &j, "f.txt", "first", "second", None);
        let id = out.txn_id.unwrap();

        let rev = revert(
            &root,
            &RevertRequest {
                txn_id: id,
                dry_run: false,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&root),
        )
        .unwrap();
        assert!(rev.applied);
        assert_ne!(rev.txn_id, Some(id), "a revert is a new transaction");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "first\n"
        );
        // …and it is itself revertible
        let back = revert(
            &root,
            &RevertRequest {
                txn_id: rev.txn_id.unwrap(),
                dry_run: false,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&root),
        )
        .unwrap();
        assert!(back.applied);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "second\n"
        );
    }

    #[test]
    fn reverting_a_file_that_moved_since_is_a_version_conflict_with_a_delta() {
        // Not a force-overwrite: a revert that bypassed the versioning
        // contract would be a hole in it.
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let out = edit_file(&root, &j, "f.txt", "first", "second", None);
        edit_file(&root, &j, "f.txt", "second", "third", None);

        let err = revert(
            &root,
            &RevertRequest {
                txn_id: out.txn_id.unwrap(),
                dry_run: false,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&root),
        )
        .unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::VersionConflict);
        let delta = err.data.unwrap()["delta"].as_str().unwrap().to_string();
        assert!(delta.contains("third"), "{delta}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "third\n",
            "nothing applied"
        );
    }

    #[test]
    fn revert_refuses_a_transaction_under_a_root_this_host_no_longer_serves() {
        let dir = tempfile::tempdir().unwrap();
        let old = root_at("kai:home", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let out = edit_file(&old, &j, "f.txt", "first", "second", None);

        let now = root_at("kai:src", dir.path());
        let err = revert(
            &now,
            &RevertRequest {
                txn_id: out.txn_id.unwrap(),
                dry_run: false,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&now),
        )
        .unwrap_err();
        assert_eq!(
            err.data.as_ref().unwrap()["reason"],
            "root_no_longer_served"
        );
    }

    #[test]
    fn revert_refuses_a_classified_file_rather_than_writing_placeholders_back() {
        // D-4: the retained blob is a rendering. Restoring it would write
        // ⟨kaed:KEY@digest⟩ into the file as a literal.
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::with_default_classify("kai:t", dir.path().canonicalize().unwrap());
        let j = Journal::open_in_memory().unwrap();
        let content = "KLAMS_TOKEN=b7f3a9d2c8e14f60b7f3a9d2c8e14f60\n";
        std::fs::write(dir.path().join(".env"), content).unwrap();
        let out = txn::apply(
            &root,
            &EditRequest {
                base: vec![BaseVersion {
                    path: ".env".into(),
                    version: fsops::version_of(content.as_bytes()),
                }],
                ops: vec![EditOp::EnvSet {
                    path: ".env".into(),
                    key: "FLAG".into(),
                    value: Some("on".into()),
                    value_from: None,
                    comment: None,
                }],
                dry_run: false,
                return_diff: false,
                intent: None,
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap();

        let err = revert(
            &root,
            &RevertRequest {
                txn_id: out.txn_id.unwrap(),
                dry_run: false,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &[ResolvedRoot::with_default_classify(
                "kai:t",
                dir.path().canonicalize().unwrap(),
            )],
        )
        .unwrap_err();
        assert_eq!(err.data.as_ref().unwrap()["reason"], "no_plaintext_history");
        assert!(
            std::fs::read_to_string(dir.path().join(".env"))
                .unwrap()
                .contains("b7f3a9d2c8e14f60b7f3a9d2c8e14f60"),
            "the real value is untouched"
        );
    }

    #[test]
    fn revert_of_a_create_refuses_and_names_the_missing_op() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        let out = txn::apply(
            &root,
            &EditRequest {
                base: Vec::new(),
                ops: vec![EditOp::Create {
                    path: "new.txt".into(),
                    content: "x\n".into(),
                    executable: false,
                    overwrite: false,
                }],
                dry_run: false,
                return_diff: false,
                intent: None,
                drop_keys: Vec::new(),
                allow_secrets: Vec::new(),
            },
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap();

        let err = revert(
            &root,
            &RevertRequest {
                txn_id: out.txn_id.unwrap(),
                dry_run: false,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&root),
        )
        .unwrap_err();
        assert_eq!(
            err.data.as_ref().unwrap()["reason"],
            "revert_of_create_needs_delete"
        );
        assert!(dir.path().join("new.txt").exists());
    }

    #[test]
    fn revert_dry_run_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let out = edit_file(&root, &j, "f.txt", "first", "second", None);

        let rev = revert(
            &root,
            &RevertRequest {
                txn_id: out.txn_id.unwrap(),
                dry_run: true,
                intent: None,
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&root),
        )
        .unwrap();
        assert!(!rev.applied);
        assert!(rev.txn_id.is_none());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "second\n"
        );
    }

    #[test]
    fn a_reverts_intent_names_the_transaction_it_undoes() {
        let dir = tempfile::tempdir().unwrap();
        let root = root_at("kai:t", dir.path());
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let out = edit_file(&root, &j, "f.txt", "first", "second", None);
        let id = out.txn_id.unwrap();
        revert(
            &root,
            &RevertRequest {
                txn_id: id,
                dry_run: false,
                intent: Some("the change broke the build"),
                author: "claude",
                allow_secrets: &[],
            },
            &j,
            &Limits::default(),
            "kai",
            &live(&root),
        )
        .unwrap();

        let out = journal(&j, std::slice::from_ref(&root), "kai", &query(5)).unwrap();
        let Entry::Txn { intent, .. } = &out.entries[0] else {
            panic!("expected a txn entry")
        };
        let intent = intent.as_deref().unwrap();
        assert!(intent.contains(&format!("revert of txn {id}")), "{intent}");
        assert!(intent.contains("broke the build"), "{intent}");
    }
}
