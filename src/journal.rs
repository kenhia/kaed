//! The durable, attributed history (R6): SQLite, one DB per host.
//!
//! `begin` lands before any rename and `complete` after the last one, so
//! an interrupted transaction survives as a row with `completed_at IS
//! NULL` — the torn-state signal `scan_pending` reports at startup. Blobs
//! are content-addressed by version and power conflict deltas and `diff`.
//!
//! Sprint 009 added the **read** path this store existed for: the query
//! surface below is what `src/history.rs` serves as `journal`/`diff`/
//! `revert`, plus the `feedback` writes that land beside it. Everything
//! here is SQL and row structs; policy, redaction and the shape agents see
//! belong to `history.rs`.
//!
//! Two things are retained on different clocks (#909): transaction
//! *metadata* forever — it is the audit trail, it is tiny — and blob
//! *content* for `journal.retention_days`, because that content is a copy
//! of whatever was in the file, secrets included. This DB is as sensitive
//! as the most sensitive file kaed is allowed to edit; the deny list is
//! what keeps that bound small.

use crate::config::ResolvedRoot;
use crate::errors::{KaedError, Result};
use crate::txn::{FailedTxnRecord, FileTxnRecord, TxnRecorder};
use rusqlite::{Connection, OptionalExtension};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS txns (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  author        TEXT NOT NULL,
  intent        TEXT,
  root          TEXT NOT NULL,
  git_head      TEXT,
  started_at    TEXT NOT NULL,
  completed_at  TEXT
);
CREATE TABLE IF NOT EXISTS txn_files (
  txn_id        INTEGER NOT NULL REFERENCES txns(id),
  path          TEXT NOT NULL,
  old_version   TEXT,
  new_version   TEXT NOT NULL,
  lines_added   INTEGER NOT NULL,
  lines_removed INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS txn_files_by_path ON txn_files(path);
-- `redacted` (008): the content is a redacted rendering, not the file's
-- raw bytes. A row with redacted = 0 whose path is *now* classified is a
-- legacy plaintext blob — history tools must redact it on read, never
-- serve it (D-11 corollary, flagged for #1049).
CREATE TABLE IF NOT EXISTS blobs (
  version    TEXT PRIMARY KEY,
  content    BLOB NOT NULL,
  created_at TEXT NOT NULL,
  redacted   INTEGER NOT NULL DEFAULT 0
);
-- Attempts that never applied. No blobs by design (#909): a failed attempt
-- produced no new content, and its pre-image is still the file on disk.
CREATE TABLE IF NOT EXISTS txn_failures (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  author           TEXT NOT NULL,
  intent           TEXT,
  root             TEXT NOT NULL,
  paths            TEXT NOT NULL,
  code             TEXT NOT NULL,
  message          TEXT NOT NULL,
  expected_version TEXT,
  actual_version   TEXT,
  failed_at        TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS txn_failures_by_author
  ON txn_failures(author, failed_at);
CREATE TABLE IF NOT EXISTS feedback (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  author     TEXT NOT NULL,
  category   TEXT NOT NULL,
  summary    TEXT NOT NULL,
  detail     TEXT,
  context    TEXT,
  created_at TEXT NOT NULL
);
-- The secrets audit stream (011): every generate / rotate / reveal /
-- transport, as events and claims, NEVER payloads. Digests only, and only
-- above the entropy floor (PD-2). `disclosed` = the value left kaed toward
-- the caller or another host; `destination` is the caller's *claim* about
-- where (D-6). Metadata clock: kept forever, like txns — this stream is
-- what makes \"has any agent ever seen this secret?\" answerable.
CREATE TABLE IF NOT EXISTS secret_events (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  author      TEXT NOT NULL,
  action      TEXT NOT NULL,
  root        TEXT NOT NULL,
  path        TEXT NOT NULL,
  key         TEXT NOT NULL,
  old_digest  TEXT,
  new_digest  TEXT,
  disclosed   INTEGER NOT NULL DEFAULT 0,
  destination TEXT,
  txn_id      INTEGER,
  intent      TEXT,
  created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS secret_events_by_key
  ON secret_events(root, path, key);
";

/// How often blob GC runs while the daemon is up. Retention is measured in
/// days; sweeping more often than hourly buys nothing.
const GC_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3600);

pub struct Journal {
    conn: Mutex<Connection>,
    /// Days a blob's content is kept. Transaction *metadata* is kept
    /// indefinitely — it is the audit trail, it is tiny, and ageing it out
    /// loses history for no gain. Only content expires.
    blob_retention_days: u32,
    last_gc: Mutex<std::time::Instant>,
}

#[derive(Debug)]
pub struct PendingTxn {
    pub id: i64,
    pub author: String,
    pub started_at: String,
    pub files: Vec<String>,
}

impl Journal {
    pub fn open(path: &Path, blob_retention_days: u32) -> anyhow::Result<Journal> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        // This file holds the content of everything kaed has edited, so it
        // is as sensitive as the most sensitive file under any root. Set
        // the mode outright rather than inheriting whatever umask gave us.
        for f in [
            path.to_path_buf(),
            wal_sibling(path, "-wal"),
            wal_sibling(path, "-shm"),
        ] {
            if f.exists() {
                let _ = std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600));
            }
        }
        let j = Journal {
            conn: Mutex::new(conn),
            blob_retention_days,
            last_gc: Mutex::new(std::time::Instant::now()),
        };
        let dropped = j.gc_blobs()?;
        if dropped > 0 {
            tracing::info!(dropped, days = blob_retention_days, "expired journal blobs");
        }
        Ok(j)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Journal> {
        Self::in_memory_with_retention(30)
    }

    /// Drive the schema directly, for tests that need to reconstruct a
    /// state kaed itself cannot produce today — an aged blob, a torn
    /// transaction, a pre-#910 journal with no failure rows.
    #[cfg(test)]
    pub fn raw_execute_for_test(&self, sql: &str) {
        self.lock().execute_batch(sql).expect("test sql");
    }

    /// A blob exactly as a pre-008 kaed wrote it: raw bytes, `redacted`
    /// unset. The state D-11's corollary is about.
    #[cfg(test)]
    pub fn insert_legacy_blob_for_test(&self, version: &str, content: &str) {
        self.lock()
            .execute(
                "INSERT INTO blobs (version, content, created_at, redacted)
                 VALUES (?1, ?2, ?3, 0)",
                rusqlite::params![version, content.as_bytes(), now()],
            )
            .expect("test blob");
    }

    #[cfg(test)]
    pub fn in_memory_with_retention(blob_retention_days: u32) -> anyhow::Result<Journal> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Journal {
            conn: Mutex::new(conn),
            blob_retention_days,
            last_gc: Mutex::new(std::time::Instant::now()),
        })
    }

    /// Drop blob content past its retention window, leaving every
    /// transaction row intact. Returns how many blobs went.
    ///
    /// Sprint 001 shipped `retention_days` with GC deferred — a retention
    /// setting that retains forever is worse than none, because it reads
    /// as a guarantee. This makes it true.
    pub fn gc_blobs(&self) -> Result<usize> {
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(
                u64::from(self.blob_retention_days) * 86_400,
            ))
            .map(|t| humantime::format_rfc3339_seconds(t).to_string());
        let Some(cutoff) = cutoff else {
            return Ok(0); // retention longer than the epoch: nothing expires
        };
        // RFC 3339 with a fixed Z offset sorts lexicographically by time.
        let n = self
            .lock()
            .execute("DELETE FROM blobs WHERE created_at < ?1", [&cutoff])
            .map_err(db_err)?;
        *self.last_gc.lock().expect("gc clock never poisoned") = std::time::Instant::now();
        Ok(n)
    }

    /// Run GC if the interval has elapsed. Called off the completion path,
    /// so a long-lived daemon sweeps without needing a timer task.
    fn gc_if_due(&self) {
        let due = {
            let last = self.last_gc.lock().expect("gc clock never poisoned");
            last.elapsed() >= GC_INTERVAL
        };
        if due && let Err(e) = self.gc_blobs() {
            tracing::warn!(error = %e, "journal blob gc failed");
        }
    }

    /// Transactions whose renames may not all have landed (begun, never
    /// completed). Reported at startup; repair stays a human/agent call
    /// until dogfooding shows what torn states actually look like.
    pub fn scan_pending(&self) -> Result<Vec<PendingTxn>> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.author, t.started_at,
                        (SELECT group_concat(f.path, ', ')
                           FROM txn_files f WHERE f.txn_id = t.id)
                 FROM txns t WHERE t.completed_at IS NULL ORDER BY t.id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PendingTxn {
                    id: row.get(0)?,
                    author: row.get(1)?,
                    started_at: row.get(2)?,
                    files: row
                        .get::<_, Option<String>>(3)?
                        .map(|s| s.split(", ").map(str::to_owned).collect())
                        .unwrap_or_default(),
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("journal lock never poisoned")
    }

    /// Days blob *content* survives, so `diff` can explain a missing
    /// version instead of shrugging at it.
    pub fn blob_retention_days(&self) -> u32 {
        self.blob_retention_days
    }
}

// ------------------------------------------------------- the read surface
//
// Row structs, not tool shapes: `history.rs` decides what an agent sees.

#[derive(Debug)]
pub struct TxnFileRow {
    pub path: String,
    pub old_version: Option<String>,
    pub new_version: String,
    pub lines_added: i64,
    pub lines_removed: i64,
}

#[derive(Debug)]
pub struct TxnRow {
    pub id: i64,
    pub author: String,
    pub intent: Option<String>,
    pub root: String,
    pub git_head: Option<String>,
    pub started_at: String,
    /// `None` = begun but never completed: the torn state `scan_pending`
    /// reports. History must show it as such rather than as applied.
    pub completed_at: Option<String>,
    pub files: Vec<TxnFileRow>,
}

#[derive(Debug)]
pub struct FailureRow {
    pub id: i64,
    pub author: String,
    pub intent: Option<String>,
    pub root: String,
    pub paths: Vec<String>,
    pub code: String,
    pub message: String,
    pub expected_version: Option<String>,
    pub actual_version: Option<String>,
    pub failed_at: String,
}

#[derive(Debug)]
pub struct FeedbackRow {
    pub id: i64,
    pub author: String,
    pub category: String,
    pub summary: String,
    pub detail: Option<String>,
    pub context: Option<String>,
    pub created_at: String,
}

/// One row of the secrets audit stream (011). An *event*, never a payload:
/// digests and locations, `disclosed` when the value left kaed, and the
/// caller's claimed `destination` for transports.
#[derive(Debug)]
pub struct SecretEventRow {
    pub id: i64,
    pub author: String,
    /// `generate` | `rotate` | `reveal` | `transport`
    pub action: String,
    pub root: String,
    pub path: String,
    pub key: String,
    pub old_digest: Option<String>,
    pub new_digest: Option<String>,
    pub disclosed: bool,
    pub destination: Option<String>,
    pub txn_id: Option<i64>,
    pub intent: Option<String>,
    pub created_at: String,
}

/// What `add_secret_event` records. Digest fields must already respect the
/// entropy floor (pass `None` below it) — the journal stores what it is
/// given and discloses nothing on its own.
#[derive(Debug, Default)]
pub struct SecretEvent<'a> {
    pub author: &'a str,
    pub action: &'a str,
    pub root: &'a str,
    pub path: &'a str,
    pub key: &'a str,
    pub old_digest: Option<&'a str>,
    pub new_digest: Option<&'a str>,
    pub disclosed: bool,
    pub destination: Option<&'a str>,
    pub txn_id: Option<i64>,
    pub intent: Option<&'a str>,
}

/// The earliest record of each kind. What `journal` reports as `coverage`
/// (D-2): a window reaching back before `failures_from` predates #910 and
/// its silence about failures is not evidence there were none.
#[derive(Debug, Default)]
pub struct Coverage {
    pub txns_from: Option<String>,
    pub failures_from: Option<String>,
    pub feedback_from: Option<String>,
    /// Earliest secrets-audit event (011). Same honesty rule as
    /// `failures_from`: a window reaching back past it is silent, not clean.
    pub secrets_from: Option<String>,
}

/// The query axes sprint 006's evidence pass actually used, minus the
/// aggregates (those stay in `scripts/journal-report.py`).
#[derive(Debug, Default)]
pub struct HistoryFilter<'a> {
    /// Matched literally against the journalled root name — including
    /// names no longer served, which is the point (R8's corollary).
    pub root: Option<&'a str>,
    /// Exact path, or a subtree when the row's path sits under it.
    pub path: Option<&'a str>,
    pub author: Option<&'a str>,
    /// RFC 3339; timestamps are fixed-offset so a lexical `>=` is a
    /// chronological one.
    pub since: Option<&'a str>,
    pub limit: usize,
}

impl<'a> HistoryFilter<'a> {
    fn path_matches(&self, path: &str) -> bool {
        match self.path {
            None => true,
            Some(p) => {
                let p = p.trim_end_matches('/');
                p.is_empty() || path == p || path.starts_with(&format!("{p}/"))
            }
        }
    }
}

impl Journal {
    /// Applied (and torn) transactions, newest first.
    pub fn txns(&self, f: &HistoryFilter<'_>) -> Result<Vec<TxnRow>> {
        let conn = self.lock();
        // The path filter lives in a subquery so a matching transaction
        // still comes back with *all* its files: "which txn touched this
        // path" must not silently hide that the txn touched three others.
        let mut sql = String::from(
            "SELECT id, author, intent, root, git_head, started_at, completed_at
               FROM txns WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(root) = f.root {
            args.push(Box::new(root.to_owned()));
            sql.push_str(&format!(" AND root = ?{}", args.len()));
        }
        if let Some(author) = f.author {
            args.push(Box::new(author.to_owned()));
            sql.push_str(&format!(" AND author = ?{}", args.len()));
        }
        if let Some(since) = f.since {
            args.push(Box::new(since.to_owned()));
            sql.push_str(&format!(" AND started_at >= ?{}", args.len()));
        }
        if let Some(path) = f.path {
            let path = path.trim_end_matches('/');
            args.push(Box::new(path.to_owned()));
            let exact = args.len();
            args.push(Box::new(format!("{path}/%")));
            sql.push_str(&format!(
                " AND EXISTS (SELECT 1 FROM txn_files f WHERE f.txn_id = txns.id
                              AND (f.path = ?{exact} OR f.path LIKE ?{}))",
                args.len()
            ));
        }
        args.push(Box::new(f.limit as i64));
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", args.len()));

        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(AsRef::as_ref).collect();
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let mut rows: Vec<TxnRow> = stmt
            .query_map(params.as_slice(), |r| {
                Ok(TxnRow {
                    id: r.get(0)?,
                    author: r.get(1)?,
                    intent: r.get(2)?,
                    root: r.get(3)?,
                    git_head: r.get(4)?,
                    started_at: r.get(5)?,
                    completed_at: r.get(6)?,
                    files: Vec::new(),
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;

        let mut files = conn
            .prepare(
                "SELECT path, old_version, new_version, lines_added, lines_removed
                   FROM txn_files WHERE txn_id = ?1 ORDER BY rowid",
            )
            .map_err(db_err)?;
        for row in &mut rows {
            row.files = files
                .query_map([row.id], |r| {
                    Ok(TxnFileRow {
                        path: r.get(0)?,
                        old_version: r.get(1)?,
                        new_version: r.get(2)?,
                        lines_added: r.get(3)?,
                        lines_removed: r.get(4)?,
                    })
                })
                .map_err(db_err)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(db_err)?;
        }
        Ok(rows)
    }

    /// One transaction by id, files included. `revert`'s entry point.
    pub fn txn(&self, id: i64) -> Result<Option<TxnRow>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, author, intent, root, git_head, started_at, completed_at
                   FROM txns WHERE id = ?1",
                [id],
                |r| {
                    Ok(TxnRow {
                        id: r.get(0)?,
                        author: r.get(1)?,
                        intent: r.get(2)?,
                        root: r.get(3)?,
                        git_head: r.get(4)?,
                        started_at: r.get(5)?,
                        completed_at: r.get(6)?,
                        files: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(db_err)?;
        let Some(mut row) = row else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT path, old_version, new_version, lines_added, lines_removed
                   FROM txn_files WHERE txn_id = ?1 ORDER BY rowid",
            )
            .map_err(db_err)?;
        row.files = stmt
            .query_map([id], |r| {
                Ok(TxnFileRow {
                    path: r.get(0)?,
                    old_version: r.get(1)?,
                    new_version: r.get(2)?,
                    lines_added: r.get(3)?,
                    lines_removed: r.get(4)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(Some(row))
    }

    /// Attempts that never applied — the half #1049 calls higher-value:
    /// "did my last edit fail, and why" beats "what did I successfully do".
    pub fn failures(&self, f: &HistoryFilter<'_>) -> Result<Vec<FailureRow>> {
        let conn = self.lock();
        let mut sql = String::from(
            "SELECT id, author, intent, root, paths, code, message,
                    expected_version, actual_version, failed_at
               FROM txn_failures WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(root) = f.root {
            args.push(Box::new(root.to_owned()));
            sql.push_str(&format!(" AND root = ?{}", args.len()));
        }
        if let Some(author) = f.author {
            args.push(Box::new(author.to_owned()));
            sql.push_str(&format!(" AND author = ?{}", args.len()));
        }
        if let Some(since) = f.since {
            args.push(Box::new(since.to_owned()));
            sql.push_str(&format!(" AND failed_at >= ?{}", args.len()));
        }
        // `paths` is a joined string, so SQL can only prefilter; the exact
        // match happens below. Over-fetch so the filter cannot silently
        // return a short page.
        let limit = if f.path.is_some() {
            args.push(Box::new(format!("%{}%", f.path.unwrap_or_default())));
            sql.push_str(&format!(" AND paths LIKE ?{}", args.len()));
            f.limit.saturating_mul(4).saturating_add(32)
        } else {
            f.limit
        };
        args.push(Box::new(limit as i64));
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", args.len()));

        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(AsRef::as_ref).collect();
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(FailureRow {
                    id: r.get(0)?,
                    author: r.get(1)?,
                    intent: r.get(2)?,
                    root: r.get(3)?,
                    paths: r
                        .get::<_, String>(4)?
                        .split(", ")
                        .map(str::to_owned)
                        .collect(),
                    code: r.get(5)?,
                    message: r.get(6)?,
                    expected_version: r.get(7)?,
                    actual_version: r.get(8)?,
                    failed_at: r.get(9)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .filter(|row| row.paths.iter().any(|p| f.path_matches(p)))
            .take(f.limit)
            .collect())
    }

    /// Friction reports. `root`/`path` do not apply — a report is about the
    /// contract, not about a file — so those filters are ignored here
    /// rather than silently emptying the result.
    pub fn feedback(&self, f: &HistoryFilter<'_>) -> Result<Vec<FeedbackRow>> {
        let conn = self.lock();
        let mut sql = String::from(
            "SELECT id, author, category, summary, detail, context, created_at
               FROM feedback WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(author) = f.author {
            args.push(Box::new(author.to_owned()));
            sql.push_str(&format!(" AND author = ?{}", args.len()));
        }
        if let Some(since) = f.since {
            args.push(Box::new(since.to_owned()));
            sql.push_str(&format!(" AND created_at >= ?{}", args.len()));
        }
        args.push(Box::new(f.limit as i64));
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", args.len()));

        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(AsRef::as_ref).collect();
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(FeedbackRow {
                    id: r.get(0)?,
                    author: r.get(1)?,
                    category: r.get(2)?,
                    summary: r.get(3)?,
                    detail: r.get(4)?,
                    context: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// The secrets audit stream (011), newest first. `path` filters exactly
    /// (an event names one file, so subtree matching rides the same rule as
    /// txns via LIKE).
    pub fn secret_events(&self, f: &HistoryFilter<'_>) -> Result<Vec<SecretEventRow>> {
        let conn = self.lock();
        let mut sql = String::from(
            "SELECT id, author, action, root, path, key, old_digest, new_digest,
                    disclosed, destination, txn_id, intent, created_at
               FROM secret_events WHERE 1=1",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(root) = f.root {
            args.push(Box::new(root.to_owned()));
            sql.push_str(&format!(" AND root = ?{}", args.len()));
        }
        if let Some(author) = f.author {
            args.push(Box::new(author.to_owned()));
            sql.push_str(&format!(" AND author = ?{}", args.len()));
        }
        if let Some(since) = f.since {
            args.push(Box::new(since.to_owned()));
            sql.push_str(&format!(" AND created_at >= ?{}", args.len()));
        }
        if let Some(path) = f.path {
            let path = path.trim_end_matches('/');
            args.push(Box::new(path.to_owned()));
            let exact = args.len();
            args.push(Box::new(format!("{path}/%")));
            sql.push_str(&format!(
                " AND (path = ?{exact} OR path LIKE ?{})",
                args.len()
            ));
        }
        args.push(Box::new(f.limit as i64));
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", args.len()));

        let params: Vec<&dyn rusqlite::ToSql> = args.iter().map(AsRef::as_ref).collect();
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let rows = stmt
            .query_map(params.as_slice(), |r| {
                Ok(SecretEventRow {
                    id: r.get(0)?,
                    author: r.get(1)?,
                    action: r.get(2)?,
                    root: r.get(3)?,
                    path: r.get(4)?,
                    key: r.get(5)?,
                    old_digest: r.get(6)?,
                    new_digest: r.get(7)?,
                    disclosed: r.get::<_, i64>(8)? != 0,
                    destination: r.get(9)?,
                    txn_id: r.get(10)?,
                    intent: r.get(11)?,
                    created_at: r.get(12)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Record one secrets-audit event. `intent` arrives already redacted —
    /// the caller owns that, as with feedback (D-5 of 009).
    pub fn add_secret_event(&self, e: &SecretEvent<'_>) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO secret_events
               (author, action, root, path, key, old_digest, new_digest,
                disclosed, destination, txn_id, intent, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                e.author,
                e.action,
                e.root,
                e.path,
                e.key,
                e.old_digest,
                e.new_digest,
                e.disclosed as i64,
                e.destination,
                e.txn_id,
                e.intent,
                now()
            ],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// File a friction report. Text arrives already redacted — the caller
    /// owns that, because the caller knows it is free text (D-5).
    pub fn add_feedback(
        &self,
        author: &str,
        category: &str,
        summary: &str,
        detail: Option<&str>,
        context: Option<&str>,
    ) -> Result<i64> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO feedback (author, category, summary, detail, context, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![author, category, summary, detail, context, now()],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// Where each record kind actually begins. Computed, never hardcoded:
    /// the boundary differs per host and moves as old rows are added.
    pub fn coverage(&self) -> Result<Coverage> {
        let conn = self.lock();
        let first = |sql: &str| -> Result<Option<String>> {
            conn.query_row(sql, [], |r| r.get::<_, Option<String>>(0))
                .optional()
                .map(Option::flatten)
                .map_err(db_err)
        };
        Ok(Coverage {
            txns_from: first("SELECT min(started_at) FROM txns")?,
            failures_from: first("SELECT min(failed_at) FROM txn_failures")?,
            feedback_from: first("SELECT min(created_at) FROM feedback")?,
            secrets_from: first("SELECT min(created_at) FROM secret_events")?,
        })
    }
}

/// Columns added after a table shipped. `CREATE TABLE IF NOT EXISTS` never
/// alters an existing table, so kai's and kubs0's live journals need the
/// explicit ALTER on upgrade.
fn migrate(conn: &Connection) -> anyhow::Result<()> {
    let has_redacted = conn
        .prepare("SELECT 1 FROM pragma_table_info('blobs') WHERE name = 'redacted'")?
        .exists([])?;
    if !has_redacted {
        conn.execute_batch("ALTER TABLE blobs ADD COLUMN redacted INTEGER NOT NULL DEFAULT 0")?;
        tracing::info!("journal migrated: blobs.redacted added (sprint 008)");
    }
    Ok(())
}

impl TxnRecorder for Journal {
    fn begin(
        &self,
        author: &str,
        intent: Option<&str>,
        root: &ResolvedRoot,
        files: &[FileTxnRecord],
    ) -> Result<i64> {
        let git_head = files
            .first()
            .map(|f| root.path.join(&f.path))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .and_then(|dir| git_head_for(&dir));
        let intent = intent.map(redact_note);
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "INSERT INTO txns (author, intent, root, git_head, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![author, intent, root.name, git_head, now()],
        )
        .map_err(db_err)?;
        let txn_id = tx.last_insert_rowid();
        for f in files {
            tx.execute(
                "INSERT INTO txn_files
                   (txn_id, path, old_version, new_version, lines_added, lines_removed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    txn_id,
                    f.path,
                    f.old_version,
                    f.new_version,
                    f.lines_added,
                    f.lines_removed
                ],
            )
            .map_err(db_err)?;
            // Blob content arrives already redacted for classified files,
            // and is absent (withheld) for content kaed cannot redact —
            // the audit row above stays either way (D-11).
            if let (Some(old_version), Some(blob)) = (&f.old_version, &f.blob_old) {
                tx.execute(
                    "INSERT OR IGNORE INTO blobs (version, content, created_at, redacted)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![old_version, blob.as_bytes(), now(), f.redacted],
                )
                .map_err(db_err)?;
            }
            if let Some(blob) = &f.blob_new {
                tx.execute(
                    "INSERT OR IGNORE INTO blobs (version, content, created_at, redacted)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![f.new_version, blob.as_bytes(), now(), f.redacted],
                )
                .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)?;
        Ok(txn_id)
    }

    fn complete(&self, txn_id: i64) -> Result<()> {
        self.lock()
            .execute(
                "UPDATE txns SET completed_at = ?1 WHERE id = ?2",
                rusqlite::params![now(), txn_id],
            )
            .map_err(db_err)?;
        self.gc_if_due();
        Ok(())
    }

    fn blob(&self, version: &str) -> Option<(String, bool)> {
        self.lock()
            .query_row(
                "SELECT content, redacted FROM blobs WHERE version = ?1",
                [version],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, bool>(1)?)),
            )
            .ok()
            .and_then(|(bytes, redacted)| Some((String::from_utf8(bytes).ok()?, redacted)))
    }

    fn fail(&self, r: &FailedTxnRecord<'_>) {
        // version_conflict is the reason this table exists, so lift its
        // versions into columns — "which base was stale, and what was it
        // actually?" should be a query, not a JSON dig.
        let field = |k: &str| {
            r.error
                .data
                .as_ref()
                .and_then(|d| d.get(k))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        };
        let write = self.lock().execute(
            "INSERT INTO txn_failures
               (author, intent, root, paths, code, message,
                expected_version, actual_version, failed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                r.author,
                r.intent.map(redact_note),
                r.root.name,
                r.paths.join(", "),
                r.error.code.to_string(),
                redact_note(&r.error.message),
                field("expected_version"),
                field("actual_version"),
                now(),
            ],
        );
        // Recording the failure must never mask it.
        if let Err(e) = write {
            tracing::warn!(error = %e, "could not journal a failed transaction");
        }
    }
}

fn now() -> String {
    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string()
}

/// Free text bound for the audit trail — an agent-supplied `intent`, an
/// error message — with secret-shaped tokens replaced.
///
/// Sprint 009 is what makes this necessary: before it, `intent` was
/// write-only and its plaintext went no further than a 0600 SQLite file;
/// `journal` now serves it back, which is exactly the "retained blob
/// becomes a served blob" transition 008's D-11 gate is about. Redacting
/// at the *store* keeps the blast radius where 008 left it, and the read
/// path redacts again so rows already written by pre-009 kaed on the live
/// hosts are covered too.
fn redact_note(s: impl AsRef<str>) -> String {
    crate::secrets::redact_free_text(s.as_ref())
}

/// The `-wal`/`-shm` files SQLite writes alongside the database. They hold
/// the same content, so they get the same mode.
fn wal_sibling(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    std::path::PathBuf::from(s)
}

fn db_err(e: impl std::fmt::Display) -> KaedError {
    KaedError::internal(format!("journal: {e}"))
}

/// HEAD of the git repo enclosing `dir`, if any. Best-effort — kaed works
/// the same outside repos; the head is correlation data, not truth.
fn git_head_for(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let head = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!head.is_empty()).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Limits;
    use crate::errors::ErrorCode;
    use crate::fsops;
    use crate::txn::{self, BaseVersion, EditOp, EditRequest};

    fn record(
        path: &str,
        old: Option<&str>,
        new: &str,
        old_v: Option<&str>,
        new_v: &str,
    ) -> FileTxnRecord {
        let (added, removed) = txn::diffstat(old.unwrap_or(""), new);
        FileTxnRecord {
            path: path.to_owned(),
            old_version: old_v.map(str::to_owned),
            new_version: new_v.to_owned(),
            lines_added: added,
            lines_removed: removed,
            blob_old: old.map(str::to_owned),
            blob_new: Some(new.to_owned()),
            redacted: false,
        }
    }

    /// The blob's content alone, for assertions that don't care about the
    /// redacted flag.
    fn blob_content(j: &Journal, version: &str) -> Option<String> {
        TxnRecorder::blob(j, version).map(|(c, _)| c)
    }

    fn scratch_root() -> (tempfile::TempDir, ResolvedRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::unrestricted("t", dir.path().canonicalize().unwrap());
        (dir, root)
    }

    #[test]
    fn begin_complete_round_trip_with_blobs_and_diffstat() {
        let j = Journal::open_in_memory().unwrap();
        let (_dir, root) = scratch_root();
        let id = j
            .begin(
                "claude",
                Some("test"),
                &root,
                &[record(
                    "f.txt",
                    Some("a\nb\n"),
                    "a\nc\nd\n",
                    Some("oldver1234567890"),
                    "newver1234567890",
                )],
            )
            .unwrap();
        assert!(j.scan_pending().unwrap().iter().any(|p| p.id == id));
        j.complete(id).unwrap();
        assert!(j.scan_pending().unwrap().is_empty());

        assert_eq!(
            blob_content(&j, "oldver1234567890").as_deref(),
            Some("a\nb\n")
        );
        assert_eq!(
            blob_content(&j, "newver1234567890").as_deref(),
            Some("a\nc\nd\n")
        );
        assert_eq!(blob_content(&j, "nope"), None);

        let conn = j.lock();
        let (added, removed): (i64, i64) = conn
            .query_row(
                "SELECT lines_added, lines_removed FROM txn_files WHERE txn_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((added, removed), (2, 1));
    }

    /// D-6, the dated boundary #1049 has to detect. From sprint 007 every
    /// journalled `root` is host-qualified. Rows written before it are not,
    /// and kai's journal already holds five naming a root (`home`) that
    /// sprint 002 removed — so `journal`/`diff`/`revert` must be built to
    /// *label* an unresolvable historical root, never to rewrite the row or
    /// alias the name back into existence.
    #[test]
    fn a_transaction_journals_the_host_qualified_root_name() {
        let j = Journal::open_in_memory().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::unrestricted("kai:src", dir.path().canonicalize().unwrap());
        let id = j
            .begin(
                "claude",
                None,
                &root,
                &[record("f.txt", None, "x\n", None, "v000000000000000")],
            )
            .unwrap();
        j.complete(id).unwrap();

        let stored: String = j
            .lock()
            .query_row("SELECT root FROM txns WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, "kai:src");
    }

    #[test]
    fn pending_scan_reports_torn_txns_with_files() {
        let j = Journal::open_in_memory().unwrap();
        let (_dir, root) = scratch_root();
        let id = j
            .begin(
                "claude",
                None,
                &root,
                &[
                    record("a.txt", None, "x\n", None, "va00000000000000"),
                    record("b.txt", None, "y\n", None, "vb00000000000000"),
                ],
            )
            .unwrap();
        let pending = j.scan_pending().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].author, "claude");
        assert_eq!(pending[0].files, vec!["a.txt", "b.txt"]);
    }

    /// The live test deliberately triggered a version_conflict and an
    /// atomicity rollback, and `sqlite_sequence` for `txns` still read 3 —
    /// neither failure left a trace anywhere (#910).
    /// Sprint 001 shipped `retention_days` with GC deferred, so the knob
    /// promised something it never did (#909). Expiry drops content and
    /// keeps the audit trail.
    #[test]
    fn blob_gc_expires_content_but_never_the_audit_trail() {
        let j = Journal::in_memory_with_retention(7).unwrap();
        let (_dir, root) = scratch_root();
        let id = j
            .begin(
                "claude",
                Some("test"),
                &root,
                &[record(
                    "f.txt",
                    Some("secret\n"),
                    "redacted\n",
                    Some("oldver1234567890"),
                    "newver1234567890",
                )],
            )
            .unwrap();
        j.complete(id).unwrap();
        assert_eq!(
            blob_content(&j, "oldver1234567890").as_deref(),
            Some("secret\n")
        );

        // age both blobs past the window
        j.lock()
            .execute("UPDATE blobs SET created_at = '2020-01-01T00:00:00Z'", [])
            .unwrap();
        assert_eq!(j.gc_blobs().unwrap(), 2);
        assert_eq!(blob_content(&j, "oldver1234567890"), None);

        let conn = j.lock();
        let txns: i64 = conn
            .query_row("SELECT count(*) FROM txns", [], |r| r.get(0))
            .unwrap();
        let files: i64 = conn
            .query_row("SELECT count(*) FROM txn_files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            (txns, files),
            (1, 1),
            "metadata is the audit trail; it stays"
        );
    }

    #[test]
    fn blob_gc_keeps_content_inside_the_window() {
        let j = Journal::in_memory_with_retention(7).unwrap();
        let (_dir, root) = scratch_root();
        let id = j
            .begin(
                "claude",
                None,
                &root,
                &[record("f.txt", None, "fresh\n", None, "v000000000000000")],
            )
            .unwrap();
        j.complete(id).unwrap();
        assert_eq!(j.gc_blobs().unwrap(), 0);
        assert_eq!(
            blob_content(&j, "v000000000000000").as_deref(),
            Some("fresh\n")
        );
    }

    #[test]
    fn the_journal_file_is_not_world_readable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");
        let j = Journal::open(&path, 7).unwrap();
        let (_d, root) = scratch_root();
        // force the WAL siblings into existence, then reopen so they're
        // caught too — the -wal file holds the same content as the DB
        let id = j
            .begin(
                "claude",
                None,
                &root,
                &[record("f.txt", None, "x\n", None, "v100000000000000")],
            )
            .unwrap();
        j.complete(id).unwrap();
        drop(j);
        let _j = Journal::open(&path, 7).unwrap();

        for f in [
            path.clone(),
            wal_sibling(&path, "-wal"),
            wal_sibling(&path, "-shm"),
        ] {
            if f.exists() {
                let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{} is {mode:o}", f.display());
            }
        }
    }

    #[test]
    fn a_version_conflict_is_recorded_with_both_versions() {
        let (dir, root) = scratch_root();
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();

        let stale = EditRequest {
            base: vec![BaseVersion {
                path: "f.txt".into(),
                version: fsops::version_of(b"something else entirely"),
            }],
            ops: vec![EditOp::AnchorReplace {
                path: "f.txt".into(),
                old_text: "first".into(),
                new_text: "second".into(),
                occurrence: None,
            }],
            dry_run: false,
            return_diff: true,
            intent: Some("retry from a stale base".into()),
            drop_keys: Vec::new(),
        };
        let err = txn::apply(&root, &stale, &Limits::default(), "claude", &j).unwrap_err();
        assert_eq!(err.code, ErrorCode::VersionConflict);

        let conn = j.lock();
        let (author, intent, paths, code, expected, actual): (
            String,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT author, intent, paths, code, expected_version, actual_version
                   FROM txn_failures",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(author, "claude");
        assert_eq!(intent.as_deref(), Some("retry from a stale base"));
        assert_eq!(paths, "f.txt");
        assert_eq!(code, "version_conflict");
        assert_eq!(
            actual.as_deref(),
            Some(fsops::version_of(b"first\n").as_str())
        );
        assert!(expected.is_some());

        // no blobs from a failed attempt (#909): nothing new was produced
        let blobs: i64 = conn
            .query_row("SELECT count(*) FROM blobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(blobs, 0);
    }

    #[test]
    fn conflict_rate_per_author_is_one_query() {
        // the metric #910 exists to make possible, and the first thing
        // kmon would scrape off this service
        let (dir, root) = scratch_root();
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let good = fsops::version_of(b"first\n");

        let edit = |version: &str, new: &str| EditRequest {
            base: vec![BaseVersion {
                path: "f.txt".into(),
                version: version.into(),
            }],
            ops: vec![EditOp::AnchorReplace {
                path: "f.txt".into(),
                old_text: "first".into(),
                new_text: new.into(),
                occurrence: None,
            }],
            dry_run: false,
            return_diff: false,
            intent: None,
            drop_keys: Vec::new(),
        };
        // one success, then two attempts still holding the pre-edit version
        txn::apply(&root, &edit(&good, "second"), &Limits::default(), "a", &j).unwrap();
        for author in ["a", "b"] {
            txn::apply(&root, &edit(&good, "third"), &Limits::default(), author, &j).unwrap_err();
        }

        let conn = j.lock();
        let mut stmt = conn
            .prepare(
                "SELECT author, count(*) FROM txn_failures
                  WHERE code = 'version_conflict' GROUP BY author ORDER BY author",
            )
            .unwrap();
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec![("a".into(), 1), ("b".into(), 1)]);
    }

    #[test]
    fn a_dry_run_that_fails_is_not_recorded_as_an_attempt() {
        let (dir, root) = scratch_root();
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let req = EditRequest {
            base: vec![BaseVersion {
                path: "f.txt".into(),
                version: fsops::version_of(b"stale"),
            }],
            ops: vec![EditOp::AnchorReplace {
                path: "f.txt".into(),
                old_text: "first".into(),
                new_text: "second".into(),
                occurrence: None,
            }],
            dry_run: true,
            return_diff: true,
            intent: None,
            drop_keys: Vec::new(),
        };
        assert!(txn::apply(&root, &req, &Limits::default(), "claude", &j).is_err());
        let n: i64 = j
            .lock()
            .query_row("SELECT count(*) FROM txn_failures", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn git_head_is_captured_inside_a_repo_and_none_outside() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(git_head_for(dir.path()), None);

        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let run = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(st.status.success(), "git {args:?}: {st:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(repo.join("f"), "x").unwrap();
        run(&["add", "f"]);
        run(&["commit", "-q", "-m", "c", "--no-gpg-sign"]);
        let head = git_head_for(&repo).unwrap();
        assert_eq!(head.len(), 40);
    }

    // ------------------------------------------------ secrets model (008)

    /// End to end through the real engine and the real journal: editing a
    /// classified dotenv file leaves only redacted renderings in the blob
    /// store. A redacted read with a plaintext journal would be theatre —
    /// #909's retention decision was made without this feature in view and
    /// deliberately does not cover it (D-11).
    #[test]
    fn classified_edits_journal_redacted_blobs_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::with_default_classify("t", dir.path().canonicalize().unwrap());
        let j = Journal::open_in_memory().unwrap();
        const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        let content = format!("KLAMS_TOKEN={VALUE}\n");
        std::fs::write(dir.path().join(".env"), &content).unwrap();
        let v = fsops::version_of(content.as_bytes());

        let out = txn::apply(
            &root,
            &EditRequest {
                base: vec![BaseVersion {
                    path: ".env".into(),
                    version: v.clone(),
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
                intent: None,
                drop_keys: Vec::new(),
            },
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap();

        // both blobs exist, both are redacted renderings, flagged as such
        let new_v = &out.files[0].new_version;
        for version in [&v, new_v] {
            let (blob, redacted) = TxnRecorder::blob(&j, version).expect("blob retained");
            assert!(redacted, "blob for {version} not flagged redacted");
            assert!(!blob.contains(VALUE), "journal leaked plaintext: {blob}");
            assert!(blob.contains("⟨kaed:KLAMS_TOKEN@"), "{blob}");
        }
        // …and the real value is on disk, untouched
        assert!(
            std::fs::read_to_string(dir.path().join(".env"))
                .unwrap()
                .contains(VALUE)
        );

        // the audit numbers survived redaction (diffstat is over raw text)
        let (added, _removed): (i64, i64) = j
            .lock()
            .query_row(
                "SELECT lines_added, lines_removed FROM txn_files",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(added, 1);
    }

    /// kai's and kubs0's live journals predate `blobs.redacted`; opening
    /// one must migrate it, not fail on it.
    #[test]
    fn opening_a_pre_008_database_migrates_the_blobs_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE blobs (
                   version    TEXT PRIMARY KEY,
                   content    BLOB NOT NULL,
                   created_at TEXT NOT NULL
                 );
                 INSERT INTO blobs VALUES ('legacyver1234567', x'73656372657400', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let j = Journal::open(&path, 3650).unwrap();
        // the legacy row reads back as unredacted — exactly the signal
        // history tools must key redact-on-read from
        let (content, redacted) =
            TxnRecorder::blob(&j, "legacyver1234567").expect("legacy blob readable");
        assert!(!redacted);
        assert_eq!(content.as_bytes(), b"secret\0");
    }

    #[test]
    fn full_loop_conflict_delta_from_real_journal() {
        // edit through the engine with a real journal, then retry with the
        // stale version: the conflict delta is an actual diff, because the
        // journal retained the old blob
        let (dir, root) = scratch_root();
        let j = Journal::open_in_memory().unwrap();
        std::fs::write(dir.path().join("f.txt"), "first\n").unwrap();
        let v1 = fsops::version_of(b"first\n");

        let edit = |old: &str, new: &str, ver: &str| EditRequest {
            base: vec![BaseVersion {
                path: "f.txt".into(),
                version: ver.into(),
            }],
            ops: vec![EditOp::AnchorReplace {
                path: "f.txt".into(),
                old_text: old.into(),
                new_text: new.into(),
                occurrence: None,
            }],
            dry_run: false,
            return_diff: true,
            intent: None,
            drop_keys: Vec::new(),
        };

        let out = txn::apply(
            &root,
            &edit("first", "second", &v1),
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap();
        assert_eq!(out.txn_id, Some(1));
        assert!(j.scan_pending().unwrap().is_empty());

        // stale retry: base v1, but the file is now at v2
        let err = txn::apply(
            &root,
            &edit("first", "third", &v1),
            &Limits::default(),
            "claude",
            &j,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::VersionConflict);
        let delta = err.data.unwrap()["delta"].as_str().unwrap().to_string();
        assert!(delta.contains("-first"), "delta was: {delta}");
        assert!(delta.contains("+second"), "delta was: {delta}");
    }
}
