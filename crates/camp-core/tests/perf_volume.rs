#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Phase 13 volume + throughput suite (spec §14 / §16). LOCAL-ONLY: the
//! measured assertions in `volume_suite` are #[ignore]d and run only by
//! `make perf` in --release. The non-ignored tests exercise the fixture
//! generator and the pure helpers so CI keeps them correct and compiling.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use camp_core::clock::Clock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

/// Fixed seed: the whole corpus is a deterministic function of this.
const SEED: u64 = 0x00C0_FFEE_CA11_0013;

/// Rigs the corpus spreads across (name == prefix). Multiple rigs give the
/// `beads_status_rig` index real selectivity and let `ls --ready` filter.
const RIGS: &[&str] = &["gc", "app", "core", "web", "data"];

/// Vocabulary the corpus draws titles/descriptions/close-reasons from, so
/// FTS queries built from these words return real ranked hits.
const WORDS: &[&str] = &[
    "ledger",
    "worker",
    "spawn",
    "dispatch",
    "formula",
    "bead",
    "rig",
    "pack",
    "cron",
    "order",
    "search",
    "memory",
    "refold",
    "vacuum",
    "socket",
    "timer",
    "graph",
    "retry",
    "check",
    "close",
    "claim",
    "milestone",
    "session",
    "event",
    "flag",
    "parser",
    "index",
    "cursor",
    "throttle",
    "watch",
    "signal",
    "queue",
    "backup",
    "corpus",
    "latency",
    "volume",
    "seed",
    "fixture",
    "assert",
    "budget",
];

const LABELS: &[&str] = &["perf", "infra", "bug", "chore", "docs", "spike"];

/// A clock whose timestamps advance a fixed step per event, so the fixture
/// spans ~30 heavy days of history (spec §16) instead of collapsing to one
/// instant. `Clock: Send`; the build is single-threaded but the counter is
/// atomic so the boxed clock stays `Send`.
struct AdvancingClock {
    start_secs: i64,
    step_secs: i64,
    n: AtomicI64,
}

impl AdvancingClock {
    fn new() -> Self {
        // 2026-01-01T00:00:00Z == unix 1_767_225_600. 3 s/event over ~1.0M
        // events ≈ 34 days ≥ 30 heavy days.
        Self {
            start_secs: 1_767_225_600,
            step_secs: 3,
            n: AtomicI64::new(0),
        }
    }
}

impl Clock for AdvancingClock {
    fn now_utc(&self) -> String {
        let i = self.n.fetch_add(1, Ordering::Relaxed);
        let secs = self.start_secs + i * self.step_secs;
        jiff::Timestamp::from_second(secs)
            .unwrap()
            .strftime("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
}

fn pick<'a>(rng: &mut fastrand::Rng, xs: &[&'a str]) -> &'a str {
    xs[rng.usize(..xs.len())]
}

fn words(rng: &mut fastrand::Rng, min: usize, max: usize) -> String {
    let n = rng.usize(min..=max);
    (0..n)
        .map(|_| pick(rng, WORDS))
        .collect::<Vec<_>>()
        .join(" ")
}

fn flush(ledger: &mut Ledger, batch: &mut Vec<EventInput>) {
    if !batch.is_empty() {
        ledger.append_batch(std::mem::take(batch)).unwrap();
    }
}

/// Build a deterministic corpus at `db_path`: exactly `bead_target` beads
/// across `RIGS`, each with a seeded lifecycle (created → maybe claimed →
/// milestones → updates → maybe closed), then topped up with milestone
/// breadcrumbs until at least `event_floor` events exist. Events are written
/// through `append_batch` (the real append path) in 5000-event WAL txns.
/// Returns (events_appended, beads_created).
fn build_fixture(db_path: &Path, bead_target: usize, event_floor: usize) -> (u64, u64) {
    let clock: Box<dyn Clock> = Box::new(AdvancingClock::new());
    let mut ledger = Ledger::open_with_clock(db_path, clock).unwrap();
    let mut rng = fastrand::Rng::with_seed(SEED);

    let mut counters: HashMap<&str, i64> = HashMap::new();
    let mut per_rig_ids: HashMap<&str, Vec<String>> = HashMap::new();
    let mut all_ids: Vec<String> = Vec::new();
    let mut batch: Vec<EventInput> = Vec::new();
    let mut events: u64 = 0;

    for _ in 0..bead_target {
        let rig = pick(&mut rng, RIGS);
        let n = {
            let c = counters.entry(rig).or_insert(0);
            *c += 1;
            *c
        };
        let id = format!("{rig}-{n}");

        // backward deps within the same rig (exercises readiness NOT EXISTS)
        let mut needs: Vec<String> = Vec::new();
        if let Some(prev) = per_rig_ids.get(rig)
            && !prev.is_empty()
            && rng.f32() < 0.35
        {
            let k = rng.usize(1..=2usize.min(prev.len()));
            for _ in 0..k {
                let dep = prev[rng.usize(..prev.len())].clone();
                if !needs.contains(&dep) {
                    needs.push(dep);
                }
            }
        }

        let mut data = serde_json::json!({
            "title": words(&mut rng, 3, 6),
            "description": words(&mut rng, 6, 12),
        });
        if !needs.is_empty() {
            data["needs"] = serde_json::json!(needs);
        }
        let nlabels = rng.usize(0..=2);
        if nlabels > 0 {
            let labels: Vec<&str> = (0..nlabels).map(|_| pick(&mut rng, LABELS)).collect();
            data["labels"] = serde_json::json!(labels);
        }
        batch.push(EventInput {
            kind: EventType::BeadCreated,
            rig: Some(rig.to_owned()),
            actor: "seed".into(),
            bead: Some(id.clone()),
            data,
        });
        events += 1;

        let claimed = rng.f32() < 0.9;
        if claimed {
            batch.push(EventInput {
                kind: EventType::BeadClaimed,
                rig: Some(rig.to_owned()),
                actor: "seed".into(),
                bead: Some(id.clone()),
                data: serde_json::json!({ "session": format!("s-{id}") }),
            });
            events += 1;
        }

        for _ in 0..rng.usize(4..=8) {
            batch.push(EventInput {
                kind: EventType::WorkerMilestone,
                rig: Some(rig.to_owned()),
                actor: format!("s-{id}"),
                bead: Some(id.clone()),
                data: serde_json::json!({ "text": words(&mut rng, 3, 8) }),
            });
            events += 1;
        }

        for _ in 0..rng.usize(0..=2) {
            batch.push(EventInput {
                kind: EventType::BeadUpdated,
                rig: Some(rig.to_owned()),
                actor: "seed".into(),
                bead: Some(id.clone()),
                data: serde_json::json!({ "description": words(&mut rng, 6, 12) }),
            });
            events += 1;
        }

        if claimed && rng.f32() < 0.78 {
            let roll = rng.f32();
            let outcome = if roll < 0.8 {
                "pass"
            } else if roll < 0.95 {
                "fail"
            } else {
                "skipped"
            };
            batch.push(EventInput {
                kind: EventType::BeadClosed,
                rig: Some(rig.to_owned()),
                actor: format!("s-{id}"),
                bead: Some(id.clone()),
                data: serde_json::json!({ "outcome": outcome, "reason": words(&mut rng, 4, 10) }),
            });
            events += 1;
        }

        per_rig_ids.entry(rig).or_default().push(id.clone());
        all_ids.push(id);

        if batch.len() >= 5000 {
            flush(&mut ledger, &mut batch);
        }
    }
    flush(&mut ledger, &mut batch);

    // Top up to the event floor with milestone breadcrumbs on existing beads.
    let mut i = 0usize;
    while events < event_floor as u64 {
        let id = all_ids[i % all_ids.len()].clone();
        let rig = id.split_once('-').unwrap().0.to_owned();
        batch.push(EventInput {
            kind: EventType::WorkerMilestone,
            rig: Some(rig),
            actor: "seed".into(),
            bead: Some(id),
            data: serde_json::json!({ "text": words(&mut rng, 3, 8) }),
        });
        events += 1;
        i += 1;
        if batch.len() >= 5000 {
            flush(&mut ledger, &mut batch);
        }
    }
    flush(&mut ledger, &mut batch);

    (events, bead_target as u64)
}

/// Nearest-rank percentile of an ascending-sorted slice. `p` in (0, 100].
fn percentile(sorted: &[u128], p: f64) -> u128 {
    assert!(!sorted.is_empty());
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[test]
fn percentile_is_nearest_rank() {
    let xs: Vec<u128> = (1..=100).collect();
    assert_eq!(percentile(&xs, 50.0), 50);
    assert_eq!(percentile(&xs, 99.0), 99);
    assert_eq!(percentile(&xs, 100.0), 100);
    assert_eq!(percentile(&[42], 50.0), 42);
}

#[test]
fn fixture_generation_is_deterministic() {
    let d1 = tempfile::tempdir().unwrap();
    let d2 = tempfile::tempdir().unwrap();
    let db1 = d1.path().join("camp.db");
    let db2 = d2.path().join("camp.db");
    let a = build_fixture(&db1, 50, 0);
    let b = build_fixture(&db2, 50, 0);
    assert_eq!(a, b);

    let dump = |db: &Path| -> Vec<(String, Option<String>, String, String)> {
        let conn = rusqlite::Connection::open(db).unwrap();
        let mut stmt = conn
            .prepare("SELECT type, bead, ts, data FROM events ORDER BY seq")
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
    };
    assert_eq!(
        dump(&db1),
        dump(&db2),
        "the seeded corpus must be identical"
    );
}

/// The spec §14 volume + throughput budget as one orchestrated assertion.
/// Builds the 1M-event / 100k-bead fixture ONCE (an expensive multi-minute
/// build in --release) and runs every volume assertion against it in order,
/// printing each measured number for the PR record. LOCAL-ONLY: run via
/// `make perf`.
#[test]
#[ignore = "volume suite: run via `make perf` (release, local-only)"]
fn volume_suite() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("camp.db");

    // 1) Build the fixture through the real append path.
    let t = Instant::now();
    let (events, beads) = build_fixture(&db, 100_000, 1_000_000);
    eprintln!(
        "[volume] built {events} events / {beads} beads in {:?}",
        t.elapsed()
    );
    assert!(
        events >= 1_000_000,
        "fixture must have >=1M events, got {events}"
    );
    assert_eq!(beads, 100_000, "fixture must have ~100k beads, got {beads}");

    let mut ledger = Ledger::open(&db).unwrap();

    // 2) doctor --refold clean at volume.
    let t = Instant::now();
    let report = ledger.refold_check().unwrap();
    eprintln!(
        "[volume] refold replayed {} events, {} drift rows in {:?}",
        report.events_replayed,
        report.drift.len(),
        t.elapsed()
    );
    assert!(
        report.drift.is_empty(),
        "refold drift at volume: {:?}",
        report.drift
    );
    assert!(report.events_replayed >= 1_000_000);

    // 3) Ranked FTS: a 10-query set, each < 50 ms.
    let queries = [
        "ledger",
        "worker spawn",
        "dispatch formula",
        "bead rig",
        "search memory",
        "refold",
        "cron order",
        "retry check",
        "backup corpus",
        "latency budget",
    ];
    for q in queries {
        let t = Instant::now();
        let hits = ledger.search(q, None, 20).unwrap();
        let dt = t.elapsed();
        eprintln!("[volume] FTS {q:?}: {} hits in {dt:?}", hits.len());
        assert!(
            !hits.is_empty(),
            "FTS {q:?} returned no hits — query/corpus mismatch"
        );
        assert!(
            dt < Duration::from_millis(50),
            "FTS {q:?} took {dt:?} (>50ms)"
        );
    }

    // 4) ls --ready indexed read < 10 ms.
    let t = Instant::now();
    let ready = ledger.ready_beads(None).unwrap();
    let dt = t.elapsed();
    eprintln!("[volume] ls --ready: {} rows in {dt:?}", ready.len());
    assert!(!ready.is_empty(), "the corpus must have ready beads");
    assert!(
        dt < Duration::from_millis(10),
        "ls --ready took {dt:?} (>10ms)"
    );

    // 5) camp backup (VACUUM INTO) of the 1M-event db, integrity_check ok.
    let backup = dir.path().join("backup.db");
    let t = Instant::now();
    ledger.backup_into(&backup).unwrap();
    eprintln!(
        "[volume] backup VACUUM INTO + integrity_check in {:?}",
        t.elapsed()
    );
    assert!(backup.exists());
    {
        let conn = rusqlite::Connection::open(&backup).unwrap();
        let ok: String = conn
            .query_row("PRAGMA integrity_check", [], |r| r.get(0))
            .unwrap();
        assert_eq!(ok, "ok", "backup failed integrity_check");
        let n: i64 = conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert!(
            n >= 1_000_000,
            "backup must carry the whole ledger, got {n}"
        );
    }

    // 6) Ledger write p50 AND p99 < 1 ms over 10k appends into the 1M db.
    let mut samples: Vec<u128> = Vec::with_capacity(10_000);
    for i in 1..=10_000i64 {
        let input = EventInput {
            kind: EventType::BeadCreated,
            rig: Some("perf".into()),
            actor: "bench".into(),
            bead: Some(format!("perf-{i}")),
            data: serde_json::json!({ "title": "perf write path sample" }),
        };
        let t = Instant::now();
        ledger.append(input).unwrap();
        samples.push(t.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let p50 = Duration::from_nanos(percentile(&samples, 50.0) as u64);
    let p99 = Duration::from_nanos(percentile(&samples, 99.0) as u64);
    eprintln!("[volume] ledger write over 10k: p50={p50:?} p99={p99:?}");
    assert!(p50 < Duration::from_millis(1), "write p50 {p50:?} (>1ms)");
    assert!(p99 < Duration::from_millis(1), "write p99 {p99:?} (>1ms)");
}
