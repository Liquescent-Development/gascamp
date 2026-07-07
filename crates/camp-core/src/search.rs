//! Ranked full-text search over the ledger's FTS5 `search` table (spec
//! §7.4): titles, descriptions, close notes, and memory. Search rows are
//! written exclusively by the fold (Phase 1); this module only reads.

use rusqlite::{Connection, params};

use crate::error::CoreError;

/// One ranked search result. `kind` is the matched row's provenance:
/// `"body"` (title + description) or `"close"` (close note). `rank` is the
/// raw SQLite `bm25(search)` value — more negative is better; results come
/// back best-first.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub bead_id: String,
    pub kind: String,
    pub snippet: String,
    pub rank: f64,
}

/// Ranked FTS5 search over everything, all time (spec §7.4). `query` is
/// FTS5 query syntax verbatim: bare terms are AND-ed, `"quoted strings"`
/// are exact phrases, `term*` is a prefix. A query FTS5 cannot parse
/// surfaces as [`CoreError::InvalidSearchQuery`] — a clean domain error,
/// never a panic. `type_filter` narrows hits to beads of one type
/// (`Some("memory")` is `camp recall`); `limit` caps the result set.
pub fn search(
    conn: &Connection,
    query: &str,
    type_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>, CoreError> {
    // rusqlite's `ToSql for usize` sits behind the off-by-default
    // `fallible_uint` feature; convert explicitly and fail fast instead.
    let limit = i64::try_from(limit).map_err(|_| CoreError::InvalidSearchQuery {
        query: query.to_owned(),
        reason: format!("limit {limit} exceeds the SQLite integer range"),
    })?;
    let mut stmt = conn.prepare(
        "SELECT search.bead_id, search.kind,
                snippet(search, 2, '', '', '…', 12),
                bm25(search)
         FROM search
         JOIN beads ON beads.id = search.bead_id
         WHERE search MATCH ?1
           AND (?2 IS NULL OR beads.type = ?2)
         ORDER BY bm25(search), search.bead_id, search.kind
         LIMIT ?3",
    )?;
    let rows = stmt
        .query_map(params![query, type_filter, limit], |r| {
            Ok(SearchHit {
                bead_id: r.get(0)?,
                kind: r.get(1)?,
                snippet: r.get(2)?,
                rank: r.get(3)?,
            })
        })
        .map_err(|e| translate_fts_error(query, e))?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(|e| translate_fts_error(query, e))?);
    }
    Ok(hits)
}

/// The FTS5 query text is a bound parameter, parsed when the statement
/// first steps. A parse failure there is a plain SQLITE_ERROR carrying
/// fts5's message (`fts5: syntax error near "("`, `no such column: x`, …);
/// our own SQL is fixed and known-good, so that combination can only mean
/// a bad user query. Every other error propagates unchanged — nothing is
/// silenced.
fn translate_fts_error(query: &str, e: rusqlite::Error) -> CoreError {
    match e {
        rusqlite::Error::SqliteFailure(ffi, Some(msg))
            if ffi.extended_code == rusqlite::ffi::SQLITE_ERROR =>
        {
            CoreError::InvalidSearchQuery {
                query: query.to_owned(),
                reason: msg,
            }
        }
        other => CoreError::Sqlite(other),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::clock::FixedClock;
    use crate::error::CoreError;
    use crate::event::{EventInput, EventType};
    use crate::ledger::Ledger;

    fn temp_ledger() -> (tempfile::TempDir, Ledger) {
        let dir = tempfile::tempdir().unwrap();
        let ledger = Ledger::open_with_clock(
            &dir.path().join("camp.db"),
            Box::new(FixedClock::new("2026-07-06T12:00:00Z")),
        )
        .unwrap();
        (dir, ledger)
    }

    fn append(ledger: &mut Ledger, kind: EventType, bead: &str, data: serde_json::Value) {
        ledger
            .append(EventInput {
                kind,
                rig: Some("gc".into()),
                actor: "test".into(),
                bead: Some(bead.into()),
                data,
            })
            .unwrap();
    }

    /// Ranking sanity (master-plan test obligation): the same terms adjacent
    /// in a short doc must outrank the same terms scattered through a long
    /// one. The expected winner is gc-2 on purpose: it sorts AFTER gc-1 in
    /// the deterministic bead_id tiebreak, so this test cannot pass by
    /// tiebreak accident — only by bm25 rank.
    #[test]
    fn exact_phrase_outranks_scattered_terms() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({
                "title": "ops notes",
                "description": "the api endpoint returns a payload and somewhere \
                 deep in the config a key rotation schedule hides among many \
                 unrelated words about deployment logging metrics and dashboards"
            }),
        );
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({"title": "rotate the api key"}),
        );

        // Both beads contain "api" and "key"; the short adjacent use wins.
        let hits = ledger.search("api key", None, 10).unwrap();
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].bead_id, "gc-2");
        assert_eq!(hits[1].bead_id, "gc-1");
        assert!(
            hits[0].rank < hits[1].rank,
            "bm25 is more-negative-is-better: {hits:?}"
        );
        assert!(
            hits[0].snippet.contains("api key"),
            "snippet: {:?}",
            hits[0].snippet
        );

        // Quoted, it is an FTS5 phrase query: only the adjacent use matches.
        let hits = ledger.search("\"api key\"", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].bead_id, "gc-2");
    }

    #[test]
    fn type_filter_narrows_to_memory_beads() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "fix the deploy pipeline"}),
        );
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-2",
            serde_json::json!({"title": "deploy runs need the staging token", "type": "memory"}),
        );

        let all = ledger.search("deploy", None, 10).unwrap();
        assert_eq!(all.len(), 2, "{all:?}");

        let memories = ledger.search("deploy", Some("memory"), 10).unwrap();
        assert_eq!(memories.len(), 1, "{memories:?}");
        assert_eq!(memories[0].bead_id, "gc-2");
        assert_eq!(memories[0].kind, "body");
    }

    #[test]
    fn close_note_content_is_searchable() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "chase the flaky test"}),
        );
        append(
            &mut ledger,
            EventType::BeadClosed,
            "gc-1",
            serde_json::json!({"outcome": "pass", "reason": "root cause was a stale dispatcher cache"}),
        );

        let hits = ledger.search("dispatcher", None, 10).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].bead_id, "gc-1");
        assert_eq!(hits[0].kind, "close");
        assert!(
            hits[0].snippet.contains("dispatcher"),
            "snippet: {:?}",
            hits[0].snippet
        );
    }

    #[test]
    fn limit_caps_the_result_set() {
        let (_dir, mut ledger) = temp_ledger();
        for i in 1..=3 {
            append(
                &mut ledger,
                EventType::BeadCreated,
                &format!("gc-{i}"),
                serde_json::json!({"title": format!("widget number {i}")}),
            );
        }
        assert_eq!(ledger.search("widget", None, 10).unwrap().len(), 3);
        assert_eq!(ledger.search("widget", None, 2).unwrap().len(), 2);
    }

    #[test]
    fn malformed_fts_queries_are_clean_domain_errors() {
        let (_dir, mut ledger) = temp_ledger();
        append(
            &mut ledger,
            EventType::BeadCreated,
            "gc-1",
            serde_json::json!({"title": "anything"}),
        );
        // Syntax error, dangling operator, unknown column filter, empty query:
        // every one must be InvalidSearchQuery, never a panic or raw Sqlite error.
        for bad in ["(", "AND", "nosuchcolumn:foo", ""] {
            match ledger.search(bad, None, 10) {
                Err(CoreError::InvalidSearchQuery { query, reason }) => {
                    assert_eq!(query, bad);
                    assert!(!reason.is_empty());
                }
                other => panic!("query {bad:?}: expected InvalidSearchQuery, got {other:?}"),
            }
        }
    }

    #[test]
    fn no_hits_is_ok_and_empty() {
        let (_dir, ledger) = temp_ledger();
        assert_eq!(ledger.search("zeppelin", None, 10).unwrap(), vec![]);
    }
}
