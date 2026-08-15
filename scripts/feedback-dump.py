#!/usr/bin/env python3
"""Dump the `feedback` rows from a kaed journal, in full.

Run on each host that runs kaed, e.g.:

    ssh kubs0 python3 - < scripts/feedback-dump.py
    ssh kubs0 'python3 - --since-id 4' < scripts/feedback-dump.py

Arguments go INSIDE the quoted remote command, as above. The obvious
`ssh kubs0 python3 - < script -- --since-id 4` does not work: the `--`
reaches argparse, which then reads `--since-id` as the positional db path
and rejects `4` as a stray. It fails loudly, at least.

The companion to journal-report.py, which aggregates txns and failures but
only ever *counts* this table. Feedback is prose written by an agent at the
moment of friction; there is nothing to aggregate, so this prints it whole.
`--since-id N` prints only rows above N -- the triage-feedback skill's
new-only pass, driven by the high-water marks in docs/feedback-triage.md.

Read-only: opens with mode=ro and never writes, checkpoints or vacuums, so
it is safe against a live kaed.

TRAP, inherited from journal-report.py and just as fatal here: kaed runs
SQLite in WAL mode and checkpoints rarely, so most rows live in the -wal
file. Opening with `immutable=1` ignores the WAL and reports an EMPTY TABLE
rather than an error -- you conclude no agent has ever filed feedback. Use
mode=ro, as below, which reads through the WAL correctly.

There is no sqlite3 CLI on the fleet hosts; python3 is the query path,
which is why this is a script and not a .sql file.
"""

import argparse
import os
import sqlite3
import sys

DEFAULT_DB = "~/.local/share/kaed/journal.db"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("db", nargs="?", default=DEFAULT_DB)
    ap.add_argument("--since-id", type=int, default=0,
                    help="only rows with id > N (0 = all)")
    args = ap.parse_args()

    path = os.path.expanduser(args.db)
    if not os.path.exists(path):
        print("no journal at %s" % path)
        return 0

    conn = sqlite3.connect("file:%s?mode=ro" % path, uri=True)
    conn.row_factory = sqlite3.Row
    try:
        total = conn.execute("select count(*) from feedback").fetchone()[0]
        max_id = conn.execute("select coalesce(max(id), 0) from feedback").fetchone()[0]
    except sqlite3.Error as exc:
        # A pre-009 build has no feedback table. Say which, so nobody reads
        # this as "an agent has never filed anything".
        print("no feedback table in %s (%s) -- pre-sprint-009 journal?" % (path, exc))
        return 0

    rows = conn.execute(
        "select * from feedback where id > ? order by id", (args.since_id,)
    ).fetchall()

    print("== feedback: %d rows total, high-water id %d, %d newer than %d =="
          % (total, max_id, len(rows), args.since_id))
    for row in rows:
        print("\n#%d  %s  [%s]  author=%s"
              % (row["id"], row["created_at"], row["category"], row["author"]))
        print("  summary: %s" % row["summary"])
        if row["detail"]:
            print("  detail:  %s" % row["detail"])
        if row["context"]:
            print("  context: %s" % row["context"])
    return 0


if __name__ == "__main__":
    sys.exit(main())
