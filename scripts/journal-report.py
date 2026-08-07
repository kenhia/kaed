#!/usr/bin/env python3
"""Summarise a kaed journal for the agent-usage report.

Run on each host that runs kaed, e.g.:

    ssh kai python3 - < scripts/journal-report.py
    ssh kubs0 python3 - < scripts/journal-report.py

Produces the aggregates behind docs/agent-usage-report-*.md. Read-only:
it opens the journal with mode=ro and never writes, checkpoints or
vacuums, so it is safe to run against a live kaed.

TRAP, and it will bite you: kaed runs SQLite in WAL mode, and the journal
is checkpointed rarely. On kai on 2026-08-06 journal.db was 4 KB while
journal.db-wal was 1.7 MB -- every row lived in the WAL. Opening with
`immutable=1` ignores the WAL and reports an EMPTY DATABASE rather than
an error, so anyone reaching for it concludes kaed has never been used.
Use mode=ro (as below), which reads through the WAL correctly.

Also note there is no sqlite3 CLI on kai or kubs0 -- python3 is the
query path, which is why this is a script and not a .sql file.
"""

import collections
import os
import sqlite3
import sys

DEFAULT_DB = "~/.local/share/kaed/journal.db"


def connect(path):
    full = os.path.expanduser(path)
    if not os.path.exists(full):
        print("no journal at %s" % full)
        sys.exit(1)
    conn = sqlite3.connect("file:%s?mode=ro" % full, uri=True)
    conn.row_factory = sqlite3.Row
    return conn


def counts(conn):
    print("== counts ==")
    for table in ("txns", "txn_files", "txn_failures", "blobs", "feedback"):
        try:
            n = conn.execute("select count(*) from %s" % table).fetchone()[0]
        except sqlite3.Error as exc:
            print("  %-14s ERR %s" % (table, exc))
            continue
        print("  %-14s %d" % (table, n))
    span = conn.execute("select min(started_at), max(started_at) from txns").fetchone()
    print("  span           %s .. %s" % (span[0], span[1]))


def shape(conn):
    print("\n== shape ==")
    roots = collections.Counter(r[0] for r in conn.execute("select root from txns"))
    print("  txns by root:  %s" % dict(roots.most_common()))
    authors = collections.Counter(r[0] for r in conn.execute("select author from txns"))
    print("  txns by author:%s" % dict(authors.most_common()))

    ext = collections.Counter()
    area = collections.Counter()
    creates = edits = added = removed = 0
    rows = conn.execute(
        "select f.*, t.root from txn_files f join txns t on t.id = f.txn_id"
    )
    for row in rows:
        ext[os.path.splitext(row["path"])[1] or "(none)"] += 1
        parts = row["path"].split("/")
        area["%s:%s" % (row["root"], "/".join(parts[:2]))] += 1
        if row["old_version"] is None:
            creates += 1
        else:
            edits += 1
        added += row["lines_added"]
        removed += row["lines_removed"]

    total = creates + edits
    docs = sum(n for e, n in ext.items() if e in (".md", ".html"))
    print("  file touches:  %d (%d create, %d edit)" % (total, creates, edits))
    print("  lines:         +%d -%d" % (added, removed))
    if total:
        print("  doc share:     %d/%d (%.0f%% .md/.html)" % (docs, total, 100.0 * docs / total))
    print("  by extension:  %s" % dict(ext.most_common()))
    print("  by area:")
    for name, n in area.most_common():
        print("     %-40s %d" % (name, n))


def failures(conn):
    print("\n== failures ==")
    by_code = collections.Counter(r[0] for r in conn.execute("select code from txn_failures"))
    print("  by code:       %s" % dict(by_code.most_common()))
    print("  (classify each by hand -- deploy/contract verification runs are")
    print("   indistinguishable from real friction except via `intent`.)")
    for row in conn.execute("select * from txn_failures order by id"):
        print("\n  #%d %s  %s  root=%s" % (row["id"], row["failed_at"], row["code"], row["root"]))
        print("     paths:   %s" % row["paths"])
        print("     intent:  %s" % (row["intent"] or "(none)"))
        print("     message: %s" % row["message"])


def intents(conn):
    print("\n== intents ==")
    missing = conn.execute(
        "select count(*) from txns where intent is null or trim(intent) = ''"
    ).fetchone()[0]
    lengths = [len(r[0] or "") for r in conn.execute("select intent from txns")]
    if lengths:
        lengths.sort()
        median = lengths[len(lengths) // 2]
        print("  missing:       %d of %d" % (missing, len(lengths)))
        print("  median length: %d chars (min %d, max %d)" % (median, lengths[0], lengths[-1]))


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_DB
    conn = connect(path)
    counts(conn)
    shape(conn)
    failures(conn)
    intents(conn)
    print("\nNOTE: reads are not journaled -- only write transactions and")
    print("failed write attempts. Read shapes, denied reads and read-side")
    print("retries are invisible here by construction.")


if __name__ == "__main__":
    main()
