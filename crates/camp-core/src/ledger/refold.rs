//! The refold property (spec §7.4, §13.5): the state tables are a fold of
//! the event log. `refold_check` replays the whole log through the same
//! `fold::apply` into a shadow database and diffs it against live state;
//! `refold_repair` replaces live state with the shadow's content.

use std::path::PathBuf;

use rusqlite::{Connection, TransactionBehavior};

use crate::error::CoreError;
use crate::ledger::{Ledger, fold, row_to_event, schema};

#[derive(Debug)]
pub struct DriftEntry {
    pub table: String,
    pub detail: String,
}

#[derive(Debug)]
pub struct RefoldReport {
    pub events_replayed: u64,
    pub drift: Vec<DriftEntry>,
}

struct TableSpec {
    name: &'static str,
    cols: &'static str,
    key: &'static str,
}

const STATE_TABLES: &[TableSpec] = &[
    TableSpec {
        name: "beads",
        cols: "id, rig, type, title, description, status, assignee, claimed_by, outcome, \
               close_reason, work_outcome, work_commit, work_branch, dispatch_failure, \
               labels, run_id, step_id, created_ts, updated_ts, closed_ts",
        key: "id",
    },
    TableSpec {
        name: "deps",
        cols: "bead_id, needs_id",
        key: "bead_id || ' needs ' || needs_id",
    },
    TableSpec {
        name: "sessions",
        cols: "name, agent, rig, claude_session_id, transcript_path, pid, status, bead, \
               spawned_ts, ended_ts",
        key: "name",
    },
    TableSpec {
        name: "search",
        cols: "bead_id, kind, content",
        key: "bead_id || '/' || kind",
    },
    TableSpec {
        name: "counters",
        cols: "prefix, high",
        key: "prefix",
    },
];

impl Ledger {
    /// Rebuild state from the event log in a shadow db and report drift.
    pub fn refold_check(&mut self) -> Result<RefoldReport, CoreError> {
        let shadow_path = self.shadow_path()?;
        let events_replayed = self.build_shadow(&shadow_path)?;
        self.attach_shadow(&shadow_path)?;
        let diff_result = diff_all(&self.conn);
        self.detach_and_remove(&shadow_path)?;
        Ok(RefoldReport {
            events_replayed,
            drift: diff_result?,
        })
    }

    /// Rebuild state from the event log and replace the live state tables
    /// with the result, then verify. One transaction covers the replacement.
    pub fn refold_repair(&mut self) -> Result<RefoldReport, CoreError> {
        let shadow_path = self.shadow_path()?;
        self.build_shadow(&shadow_path)?;
        self.attach_shadow(&shadow_path)?;
        let replace_result = self.replace_state_from_shadow();
        self.detach_and_remove(&shadow_path)?;
        replace_result?;
        self.refold_check()
    }

    fn shadow_path(&self) -> Result<PathBuf, CoreError> {
        let db_path = self
            .conn
            .path()
            .ok_or_else(|| CoreError::Corrupt("ledger connection has no file path".to_owned()))?;
        Ok(PathBuf::from(format!("{db_path}.refold")))
    }

    /// Create the shadow db (state tables only) and replay every event
    /// through the same fold that produced live state.
    fn build_shadow(&self, shadow_path: &PathBuf) -> Result<u64, CoreError> {
        if shadow_path.exists() {
            std::fs::remove_file(shadow_path).map_err(|e| {
                CoreError::Corrupt(format!(
                    "stale refold shadow {shadow_path:?} could not be removed: {e}"
                ))
            })?;
        }
        let shadow = Connection::open(shadow_path)?;
        shadow.pragma_update(None, "foreign_keys", "ON")?;
        shadow.execute_batch(schema::STATE_DDL)?;

        let mut stmt = self
            .conn
            .prepare("SELECT seq, ts, type, rig, actor, bead, data FROM events ORDER BY seq")?;
        let rows = stmt.query_map([], row_to_event)?;
        let mut replayed = 0u64;
        for row in rows {
            let event = row?;
            fold::apply(&shadow, &event)?;
            replayed += 1;
        }
        Ok(replayed)
    }

    fn attach_shadow(&self, shadow_path: &PathBuf) -> Result<(), CoreError> {
        let path_str = shadow_path.to_str().ok_or_else(|| {
            CoreError::Corrupt(format!("refold shadow path is not UTF-8: {shadow_path:?}"))
        })?;
        self.conn
            .execute("ATTACH DATABASE ?1 AS refold", [path_str])?;
        Ok(())
    }

    fn detach_and_remove(&self, shadow_path: &PathBuf) -> Result<(), CoreError> {
        self.conn.execute("DETACH DATABASE refold", [])?;
        std::fs::remove_file(shadow_path).map_err(|e| {
            CoreError::Corrupt(format!(
                "refold shadow {shadow_path:?} could not be removed: {e}"
            ))
        })?;
        Ok(())
    }

    fn replace_state_from_shadow(&mut self) -> Result<(), CoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Delete child tables before parents (deps references beads), insert
        // parents before children.
        for spec in STATE_TABLES.iter().rev() {
            tx.execute(&format!("DELETE FROM main.{}", spec.name), [])?;
        }
        for spec in STATE_TABLES {
            tx.execute(
                &format!(
                    "INSERT INTO main.{table} ({cols}) SELECT {cols} FROM refold.{table}",
                    table = spec.name,
                    cols = spec.cols
                ),
                [],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn diff_all(conn: &Connection) -> Result<Vec<DriftEntry>, CoreError> {
    let mut drift = Vec::new();
    for spec in STATE_TABLES {
        collect_keys(
            conn,
            spec,
            "refold",
            "main",
            "fold expects this row; live state is missing or has altered it",
            &mut drift,
        )?;
        collect_keys(
            conn,
            spec,
            "main",
            "refold",
            "live state holds this row but the fold does not produce it",
            &mut drift,
        )?;
    }
    Ok(drift)
}

fn collect_keys(
    conn: &Connection,
    spec: &TableSpec,
    have: &str,
    lack: &str,
    what: &str,
    drift: &mut Vec<DriftEntry>,
) -> Result<(), CoreError> {
    let sql = format!(
        "SELECT {key} FROM (SELECT {cols} FROM {have}.{table} EXCEPT SELECT {cols} FROM {lack}.{table})",
        key = spec.key,
        cols = spec.cols,
        table = spec.name
    );
    let mut stmt = conn.prepare(&sql)?;
    let keys = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for key in keys {
        drift.push(DriftEntry {
            table: spec.name.to_owned(),
            detail: format!("{}: {what}", key?),
        });
    }
    Ok(())
}
