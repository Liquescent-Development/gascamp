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
               close_reason, work_outcome, work_commit, work_branch, final_disposition, \
               dispatch_failure, labels, run_id, step_id, created_ts, updated_ts, closed_ts",
        key: "id",
    },
    // AFTER `beads`: `replace_state_from_shadow` deletes with `.iter().rev()`,
    // so a child table listed after its parent is deleted FIRST and the FK
    // holds (`foreign_keys = ON`). Without this entry `--refold` never diffs a
    // reservation and `--repair` hard-fails on the constraint.
    TableSpec {
        name: "bead_meta",
        cols: "bead_id, key, value",
        key: "bead_id || '/' || key",
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
    // cp-3 (§5.3): the permission plane. No FK to sessions, so placement is
    // free; every column is listed so the EXCEPT-both-ways diff observes a
    // divergent decision/decided_by/status.
    TableSpec {
        name: "permissions",
        cols: "request_id, session, tool_name, status, decision, decided_by, \
               requested_ts, decided_ts",
        key: "request_id",
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeSet;

    use super::STATE_TABLES;
    use crate::ledger::Ledger;

    /// EVERY column of a state table must be listed in its [`TableSpec`].
    ///
    /// `cols` drives BOTH halves of the refold, so an unlisted column is not a
    /// cosmetic omission — it is silent data loss that reports success:
    ///
    /// - the drift diff (`SELECT {cols} … EXCEPT SELECT {cols}`) never compares
    ///   it, so `--refold` cannot SEE it drift; and
    /// - `replace_state_from_shadow`'s `INSERT INTO main.{t} ({cols}) SELECT
    ///   {cols}` never copies it, so `--repair` NULLs it for EVERY row — and
    ///   then its own verifying check calls the result clean.
    ///
    /// #122 shipped exactly that way: `beads.final_disposition` was added to
    /// the DDL and not here, so one `camp doctor --repair` would have wiped
    /// every disposition in the camp and certified itself healthy. "0 drift
    /// rows" over an undiffed column is a vacuous pass.
    ///
    /// This pins the two hand-maintained lists — the DDL and `STATE_TABLES` —
    /// to each other, so the NEXT column added to a state table fails HERE
    /// instead of quietly rotting the operator's integrity surface.
    #[test]
    fn every_state_table_column_is_diffed_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open(&dir.path().join("camp.db")).unwrap();
        for spec in STATE_TABLES {
            let mut stmt = ledger
                .conn
                .prepare("SELECT name FROM pragma_table_info(?1)")
                .unwrap();
            let actual: BTreeSet<String> = stmt
                .query_map([spec.name], |r| r.get::<_, String>(0))
                .unwrap()
                .map(|c| c.unwrap())
                .collect();
            assert!(
                !actual.is_empty(),
                "{}: pragma_table_info returned nothing — is the table named right?",
                spec.name
            );
            let listed: BTreeSet<String> =
                spec.cols.split(',').map(|c| c.trim().to_owned()).collect();
            assert_eq!(
                actual, listed,
                "state table `{}` has columns the refold neither diffs nor repairs — \
                 `--repair` would NULL them for every row and report clean",
                spec.name
            );
        }
    }
}
