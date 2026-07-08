//! Schema v1 for camp.db (spec §7.1/§7.4). One WAL-mode SQLite file: the
//! append-only `events` table (history + bus) plus the state tables that are
//! a fold of it. All tables are STRICT; opening a db with a different schema
//! version is a hard error — no auto-upgrade in v1.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::CoreError;

pub const SCHEMA_VERSION: i64 = 1;

/// State tables only — everything `refold` rebuilds from the event log.
/// (`cursors` is consumer bookkeeping, `meta`/`events` are not fold-derived.)
pub(crate) const STATE_DDL: &str = r#"
CREATE TABLE beads (
  id           TEXT PRIMARY KEY,
  rig          TEXT NOT NULL,
  type         TEXT NOT NULL DEFAULT 'task',
  title        TEXT NOT NULL,
  description  TEXT NOT NULL DEFAULT '',
  status       TEXT NOT NULL CHECK (status IN ('open','in_progress','closed')),
  assignee     TEXT,
  claimed_by   TEXT,
  outcome      TEXT CHECK (outcome IN ('pass','fail')),
  close_reason TEXT,
  labels       TEXT NOT NULL DEFAULT '[]',
  run_id       TEXT,
  step_id      TEXT,
  created_ts   TEXT NOT NULL,
  updated_ts   TEXT NOT NULL,
  closed_ts    TEXT
) STRICT;
CREATE INDEX beads_status_rig ON beads(status, rig);

CREATE TABLE deps (
  bead_id  TEXT NOT NULL REFERENCES beads(id),
  needs_id TEXT NOT NULL,
  PRIMARY KEY (bead_id, needs_id)
) STRICT;
CREATE INDEX deps_needs ON deps(needs_id);

CREATE TABLE sessions (
  name              TEXT PRIMARY KEY,
  agent             TEXT NOT NULL,
  rig               TEXT,
  claude_session_id TEXT,
  transcript_path   TEXT,
  pid               INTEGER,
  status            TEXT NOT NULL CHECK (status IN ('live','stopped','crashed')),
  bead              TEXT,
  spawned_ts        TEXT NOT NULL,
  ended_ts          TEXT
) STRICT;

CREATE VIRTUAL TABLE search USING fts5(
  bead_id UNINDEXED, kind UNINDEXED, content
);

CREATE TABLE counters (
  prefix TEXT PRIMARY KEY,
  high   INTEGER NOT NULL
) STRICT;
"#;

const FULL_DDL_PREFIX: &str = r#"
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
) STRICT;
INSERT INTO meta (key, value) VALUES ('schema_version', '1');

CREATE TABLE events (
  seq   INTEGER PRIMARY KEY AUTOINCREMENT,
  ts    TEXT NOT NULL,
  type  TEXT NOT NULL,
  rig   TEXT,
  actor TEXT NOT NULL,
  bead  TEXT,
  data  TEXT NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX events_bead ON events(bead) WHERE bead IS NOT NULL;
CREATE INDEX events_type ON events(type);

CREATE TABLE cursors (
  name TEXT PRIMARY KEY,
  seq  INTEGER NOT NULL
) STRICT;
"#;

pub(crate) fn open_db(path: &Path) -> Result<Connection, CoreError> {
    let conn = Connection::open(path)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if mode != "wal" {
        return Err(CoreError::Corrupt(format!(
            "could not enable WAL mode (got {mode:?})"
        )));
    }
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    init_schema(&conn)?;
    Ok(conn)
}

/// Open an existing ledger read-only (`SQLITE_OPEN_READ_ONLY`) — the
/// `camp export` path (spec §15.3). No schema creation, no journal-mode
/// pragma (WAL is a database-file property already set at creation); a
/// missing or schema-less database is a hard error, never repaired.
pub(crate) fn open_db_read_only(path: &Path) -> Result<Connection, CoreError> {
    use rusqlite::OpenFlags;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    if !has_meta(&conn)? {
        return Err(CoreError::Corrupt(format!(
            "{} has no meta table — not an initialized camp ledger",
            path.display()
        )));
    }
    verify_schema_version(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<(), CoreError> {
    if !has_meta(conn)? {
        conn.execute_batch(&format!("BEGIN;{FULL_DDL_PREFIX}{STATE_DDL}COMMIT;"))?;
        return Ok(());
    }
    verify_schema_version(conn)
}

fn has_meta(conn: &Connection) -> Result<bool, CoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')",
        [],
        |r| r.get(0),
    )?)
}

fn verify_schema_version(conn: &Connection) -> Result<(), CoreError> {
    let raw: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'schema_version'",
        [],
        |r| r.get(0),
    )?;
    let found: i64 = raw
        .parse()
        .map_err(|_| CoreError::Corrupt(format!("schema_version is not an integer: {raw:?}")))?;
    if found != SCHEMA_VERSION {
        return Err(CoreError::UnsupportedSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}
