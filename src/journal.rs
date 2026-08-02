//! The durable, attributed history (R6): SQLite, one DB per host.
//!
//! Sprint 001 records transactions (the `journal`/`diff`/`revert` read
//! tools come later). `begin` lands before any rename and `complete` after
//! the last one, so an interrupted transaction survives as a row with
//! `completed_at IS NULL` — the torn-state signal `scan_pending` reports
//! at startup. Blobs are content-addressed by version and power conflict
//! deltas today, `diff`/`revert` later. Retention GC is deferred.

use crate::config::ResolvedRoot;
use crate::errors::{KaedError, Result};
use crate::txn::{FileTxnRecord, TxnRecorder};
use rusqlite::Connection;
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
CREATE TABLE IF NOT EXISTS blobs (
  version    TEXT PRIMARY KEY,
  content    BLOB NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS feedback (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  author     TEXT NOT NULL,
  category   TEXT NOT NULL,
  summary    TEXT NOT NULL,
  detail     TEXT,
  context    TEXT,
  created_at TEXT NOT NULL
);
";

pub struct Journal {
    conn: Mutex<Connection>,
}

#[derive(Debug)]
pub struct PendingTxn {
    pub id: i64,
    pub author: String,
    pub started_at: String,
    pub files: Vec<String>,
}

impl Journal {
    pub fn open(path: &Path) -> anyhow::Result<Journal> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Journal {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> anyhow::Result<Journal> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Journal {
            conn: Mutex::new(conn),
        })
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
}

impl TxnRecorder for Journal {
    fn begin(
        &self,
        author: &str,
        intent: Option<&str>,
        root: &ResolvedRoot,
        files: &[FileTxnRecord<'_>],
    ) -> Result<i64> {
        let git_head = files
            .first()
            .map(|f| root.path.join(f.path))
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .and_then(|dir| git_head_for(&dir));
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
            let (added, removed) = diffstat(f.old_content.unwrap_or(""), f.new_content);
            tx.execute(
                "INSERT INTO txn_files
                   (txn_id, path, old_version, new_version, lines_added, lines_removed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![txn_id, f.path, f.old_version, f.new_version, added, removed],
            )
            .map_err(db_err)?;
            if let (Some(old_version), Some(old_content)) = (f.old_version, f.old_content) {
                tx.execute(
                    "INSERT OR IGNORE INTO blobs (version, content, created_at)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![old_version, old_content.as_bytes(), now()],
                )
                .map_err(db_err)?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO blobs (version, content, created_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![f.new_version, f.new_content.as_bytes(), now()],
            )
            .map_err(db_err)?;
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
        Ok(())
    }

    fn blob(&self, version: &str) -> Option<String> {
        self.lock()
            .query_row(
                "SELECT content FROM blobs WHERE version = ?1",
                [version],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }
}

fn now() -> String {
    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string()
}

fn db_err(e: impl std::fmt::Display) -> KaedError {
    KaedError::internal(format!("journal: {e}"))
}

fn diffstat(old: &str, new: &str) -> (i64, i64) {
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

    fn record<'a>(
        path: &'a str,
        old: Option<&'a str>,
        new: &'a str,
        old_v: Option<&'a str>,
        new_v: &'a str,
    ) -> FileTxnRecord<'a> {
        FileTxnRecord {
            path,
            old_version: old_v,
            new_version: new_v,
            old_content: old,
            new_content: new,
        }
    }

    fn scratch_root() -> (tempfile::TempDir, ResolvedRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot {
            name: "t".into(),
            path: dir.path().canonicalize().unwrap(),
            description: None,
        };
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

        assert_eq!(j.blob("oldver1234567890").as_deref(), Some("a\nb\n"));
        assert_eq!(j.blob("newver1234567890").as_deref(), Some("a\nc\nd\n"));
        assert_eq!(j.blob("nope"), None);

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
