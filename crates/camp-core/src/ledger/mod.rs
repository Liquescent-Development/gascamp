//! The Gas Camp ledger: one WAL-mode SQLite file holding the append-only
//! event log and the state tables folded from it (spec §7).

mod schema;

use std::path::Path;

use rusqlite::Connection;

use crate::clock::{Clock, SystemClock};
use crate::error::CoreError;

pub struct Ledger {
    #[allow(dead_code)] // read from lib code starting Task 1.3 (append)
    conn: Connection,
    #[allow(dead_code)] // used from Task 1.3 onward (append stamps event timestamps)
    clock: Box<dyn Clock>,
}

impl Ledger {
    pub fn open(db_path: &Path) -> Result<Self, CoreError> {
        Self::open_with_clock(db_path, Box::new(SystemClock))
    }

    pub fn open_with_clock(db_path: &Path, clock: Box<dyn Clock>) -> Result<Self, CoreError> {
        let conn = schema::open_db(db_path)?;
        Ok(Self { conn, clock })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    pub(crate) fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
        )
        .unwrap();
        (dir, ledger)
    }

    #[test]
    fn open_applies_pragmas_and_creates_schema_v1() {
        let (_dir, ledger) = temp_ledger();
        let conn = &ledger.conn;

        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sync, 1, "synchronous must be NORMAL (decided 2026-07-05)");
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);

        for table in [
            "meta", "events", "beads", "deps", "sessions", "cursors", "search",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "missing table {table}");
        }

        let version: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(version, "1");
    }

    #[test]
    fn fts5_is_available_and_searchable() {
        let (_dir, ledger) = temp_ledger();
        ledger
            .conn
            .execute(
                "INSERT INTO search (bead_id, kind, content) VALUES ('gc-1', 'body', 'refactor the auth layer')",
                [],
            )
            .unwrap();
        let hit: String = ledger
            .conn
            .query_row(
                "SELECT bead_id FROM search WHERE search MATCH 'auth'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hit, "gc-1");
    }

    #[test]
    fn reopen_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.db");
        drop(Ledger::open(&path).unwrap());
        // second open must not re-run migration or error
        drop(Ledger::open(&path).unwrap());
    }

    #[test]
    fn unsupported_schema_version_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("camp.db");
        {
            let ledger = Ledger::open(&path).unwrap();
            ledger
                .conn
                .execute(
                    "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
                    [],
                )
                .unwrap();
        }
        match Ledger::open(&path) {
            Err(CoreError::UnsupportedSchema { found, supported }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, 1);
            }
            Err(other) => panic!("expected UnsupportedSchema, got {other:?}"),
            Ok(_) => panic!("open must fail on schema_version 999"),
        }
    }
}
