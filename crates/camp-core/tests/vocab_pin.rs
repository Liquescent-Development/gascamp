//! The spec §15.2 vocabulary mirror, enforced: event names and outcome
//! metadata match Gas City verbatim where the concept exists; camp-specific
//! names are additive, never redefinitions. The gc side is pinned in
//! fixtures/gc-vocab.json (re-verified against gascity source by the Phase 6
//! CI job).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use camp_core::event::EventType;
use camp_core::vocab;

#[derive(serde::Deserialize)]
struct GcVocab {
    events: Vec<String>,
    outcome: Vec<String>,
    work_outcome: Vec<String>,
    final_disposition: Vec<String>,
    on_exhausted: Vec<String>,
}

fn gc() -> GcVocab {
    serde_json::from_str(include_str!("fixtures/gc-vocab.json")).unwrap()
}

#[test]
fn every_event_type_is_declared_mirrored_or_camp_specific_never_both() {
    let mirrored: BTreeSet<&str> = vocab::GC_MIRRORED_EVENTS.iter().copied().collect();
    let camp_specific: BTreeSet<&str> = vocab::CAMP_SPECIFIC_EVENTS.iter().copied().collect();

    assert!(
        mirrored.is_disjoint(&camp_specific),
        "a name cannot be both mirrored and camp-specific"
    );

    let declared: BTreeSet<&str> = mirrored.union(&camp_specific).copied().collect();
    let actual: BTreeSet<&str> = EventType::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(
        declared, actual,
        "vocab.rs must partition exactly the EventType registry"
    );
}

#[test]
fn mirrored_names_exist_in_gc_verbatim() {
    let gc_events: BTreeSet<String> = gc().events.into_iter().collect();
    for name in vocab::GC_MIRRORED_EVENTS {
        assert!(
            gc_events.contains(*name),
            "{name} is declared gc-mirrored but gc has no such event"
        );
    }
}

#[test]
fn camp_specific_names_do_not_collide_with_gc() {
    let gc_events: BTreeSet<String> = gc().events.into_iter().collect();
    for name in vocab::CAMP_SPECIFIC_EVENTS {
        assert!(
            !gc_events.contains(*name),
            "{name} is declared camp-specific but exists in gc — it must be mirrored or renamed \
             (additive, never redefinitions)"
        );
    }
}

#[test]
fn outcome_vocabulary_is_a_strict_subset_of_gc() {
    let gc = gc();
    let gc_outcomes: BTreeSet<String> = gc.outcome.into_iter().collect();
    for value in vocab::CAMP_OUTCOMES {
        assert!(
            gc_outcomes.contains(*value),
            "camp outcome {value:?} is not gc vocabulary"
        );
    }

    let gc_dispositions: BTreeSet<String> = gc.final_disposition.into_iter().collect();
    let gc_on_exhausted: BTreeSet<String> = gc.on_exhausted.into_iter().collect();
    for value in vocab::CAMP_FINAL_DISPOSITIONS {
        assert!(
            gc_dispositions.contains(*value),
            "camp final_disposition {value:?} is not gc vocabulary"
        );
        assert!(
            gc_on_exhausted.contains(*value),
            "camp final_disposition {value:?} is not a legal gc on_exhausted value"
        );
    }

    // Phase 9: the run-level disposition vocabulary (run.finalized) is a
    // strict subset of gc's final_disposition list.
    for value in vocab::CAMP_RUN_DISPOSITIONS {
        assert!(
            gc_dispositions.contains(*value),
            "camp run disposition {value:?} is not gc vocabulary"
        );
    }
}

/// Q3 (REVISED, SETTLED): camp adopts Gas City's WorkOutcome axis VERBATIM
/// — the full set, exact spelling and order, mirrored (not a subset, not a
/// superset). Values verified against gascity internal/beadmeta/values.go
/// at the pinned ref (gc.work_outcome, ADR-0009).
#[test]
fn work_outcome_axis_mirrors_gc_verbatim() {
    let gc = gc();
    let gc_work: Vec<&str> = gc.work_outcome.iter().map(String::as_str).collect();
    assert_eq!(vocab::CAMP_WORK_OUTCOMES, gc_work.as_slice());
}

/// Obligation (iv): adopting the WorkOutcome axis changes NOTHING on the
/// control axis — the exact v1 sets, pinned.
#[test]
fn control_outcome_axis_is_unchanged() {
    assert_eq!(vocab::CAMP_OUTCOMES, ["pass", "fail", "skipped"]);
    assert_eq!(vocab::CAMP_FINAL_DISPOSITIONS, ["hard_fail", "soft_fail"]);
    assert_eq!(
        vocab::CAMP_RUN_DISPOSITIONS,
        ["pass", "hard_fail", "soft_fail"]
    );
}
