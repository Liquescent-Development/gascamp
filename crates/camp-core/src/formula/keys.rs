//! The camp formula key table (compat spec §4) — the single source of truth
//! for what camp does with every key it can meet in a Gas City formula.
//!
//! Three axes, and §4's three traps are exactly the three ways to collapse
//! them:
//!
//! * **Trap 1 — key off NESTING, never name.** Top-level `mode` and
//!   `single_lane` are DEAD in gc; `steps.<id>.check.check.mode` and
//!   `steps.<id>.drain.item.single_lane` are load-bearing. Same words,
//!   different keys. That is why [`classify`] takes a [`Site`].
//! * **Trap 2 — a key's CLASS is not its VALUE's verdict.** `phase` refuses
//!   on the key, but a step's `metadata` is accepted while a `gc.kind =
//!   "scope"` hiding in its VALUES is refused. [`classify`] cannot say that;
//!   [`refuse`] can.
//! * **Trap 3 — an accepted key that compiles to nothing is a silent lie.**
//!   [`UNIMPLEMENTED`] names every key the table accepts before the pipeline
//!   honours it, so the rung counts stay true. It is DELETED by the phase's
//!   last rung.

use toml::Value;

use crate::formula::ast::Refusal;

/// WHERE a key sits. §4 trap 1: the same word means different things at
/// different nestings, so nothing in this module keys off the name alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Top,
    Step,
    /// `steps.<id>.check`
    Check,
    /// `steps.<id>.check.check` — gc's inner exec table.
    CheckInner,
    /// `steps.<id>.retry`
    Retry,
    /// `steps.<id>.on_complete`
    OnComplete,
    /// `steps.<id>.drain`
    Drain,
    /// `steps.<id>.drain.item`
    DrainItem,
}

/// D2′ — the permissiveness rule is scoped by ORIGIN. An unrecognised key is
/// a warning in a third-party pack camp merely IMPORTS, and a hard error in
/// the operator's own `<root>/formulas/`, where a typo is a bug camp must
/// name rather than silently drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Imported,
    CampLocal,
}

/// What camp does with a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Camp implements it (or will, at a later rung — see [`UNIMPLEMENTED`]).
    Accepted,
    /// Real gc semantics camp does NOT implement ⇒ §4 rule 1: refuse by name,
    /// loudly, rather than approximate.
    Refused,
    /// A real gc key with NO gc semantics behind it (gc's own dead config).
    /// Ignored + warned in BOTH tiers: refusing a formula over a key that
    /// does nothing even in gc would cost real corpus coverage for nothing.
    DeadInGc,
    /// Human annotation. Silent in both tiers.
    Annotation,
    /// Recognised by nobody. Imported ⇒ ignore + warn. CampLocal ⇒ HARD ERROR.
    Unknown,
}

/// One rung of §9's key-set ladder.
pub struct Rung {
    pub id: &'static str,
    pub top: &'static [&'static str],
    pub step: &'static [&'static str],
}

/// §9's ladder, verbatim. Each rung ADDS its keys to the accepted set; the
/// per-rung corpus counts (2 · 31 · 49 · 76 · 95) are pinned by
/// `ci/gc-compat/formula_gate.py` driving the real binary, and independently
/// predicted by `ci/gc-compat/rungs.py`.
pub const RUNGS: &[Rung] = &[
    Rung {
        id: "2a",
        top: &["contract"],
        step: &["description_file", "metadata"],
    },
    Rung {
        id: "2b",
        top: &["vars"],
        step: &["condition"],
    },
    Rung {
        id: "2c",
        top: &["extends"],
        step: &[],
    },
    Rung {
        id: "2d",
        top: &["type", "template"],
        step: &["expand", "expand_vars", "children"],
    },
    Rung {
        id: "2e",
        top: &[],
        step: &["drain"],
    },
];

/// §4 trap 3 — keys the table ACCEPTS that the pipeline does not yet honour.
/// Each of Tasks 5–8 removes its own keys as it implements them; the last one
/// DELETES this constant. Without it an accepted-but-unimplemented key would
/// compile to nothing, silently, and every intermediate rung count would be a
/// lie.
pub const UNIMPLEMENTED: &[&str] = &[
    // rung 2b (`vars`, `condition`) — LANDED.
    "extends", // rung 2c
    "type",
    "template",
    "expand",
    "expand_vars",
    "children", // rung 2d
    "drain",    // rung 2e
];

// ---- the tables ------------------------------------------------------------

const BASE_TOP: &[&str] = &["description", "formula", "requires", "steps"];
const BASE_STEP: &[&str] = &[
    "assignee",
    "check",
    "description",
    "id",
    "needs",
    "on_complete",
    "retry",
    "timeout",
    "title",
];

/// Real gc keys with no gc semantics behind them. `mode` and `single_lane`
/// are here ONLY at [`Site::Top`] — §4 trap 1.
const DEAD_TOP: &[&str] = &[
    "version",
    "target_required",
    "internal",
    "mode",
    "single_lane",
    "sling_container_mode",
];

const ANNO_TOP: &[&str] = &["notes", "catalog", "metadata"];
const ANNO_STEP: &[&str] = &["notes", "tags", "priority"];

/// §4 rule 1 — gc semantics camp does not implement. Refused BY NAME.
const REFUSED_TOP: &[&str] = &["advice", "compose", "phase", "pointcuts", "pour"];
const REFUSED_STEP: &[&str] = &["depends_on", "gate", "loop", "tally", "waits_for"];
/// Drain keys camp refuses (0 corpus uses each; §4 rule 1).
const REFUSED_DRAIN: &[&str] = &["continuation_group", "max_units"];

/// Every key accepted at [`Site::Step`] across ALL rungs, plus the base set.
fn accepted_step(key: &str) -> bool {
    BASE_STEP.contains(&key) || RUNGS.iter().any(|r| r.step.contains(&key))
}

/// Every key accepted at [`Site::Top`] across ALL rungs, plus the base set.
fn accepted_top(key: &str) -> bool {
    BASE_TOP.contains(&key) || RUNGS.iter().any(|r| r.top.contains(&key))
}

/// §4 trap 3 AND trap 1, together: is `key` — **at `site`** — a rung key the
/// pipeline does not yet honour?
///
/// **Site-aware, and that is not decoration.** [`UNIMPLEMENTED`] is a list of
/// NAMES, and `vars` is on it (the top-level rung-2b key). But
/// `steps.<id>.on_complete.vars` is a DIFFERENT KEY that merely shares the name
/// — it is implemented, load-bearing, and carries every fan-out formula in the
/// corpus. Matching UNIMPLEMENTED by name alone rejects all of them. Same shape
/// as `mode` (dead at Top, load-bearing at [`Site::CheckInner`]) and
/// `single_lane` (dead at Top, real at [`Site::DrainItem`]).
pub fn is_unimplemented(site: Site, key: &str) -> bool {
    let on_a_rung_at_this_site = RUNGS.iter().any(|r| match site {
        Site::Top => r.top.contains(&key),
        Site::Step => r.step.contains(&key),
        _ => false,
    });
    on_a_rung_at_this_site && UNIMPLEMENTED.contains(&key)
}

/// What camp does with `key` at `site`. Pure; value-blind — the value-aware
/// layer is [`refuse`].
pub fn classify(site: Site, key: &str) -> Class {
    match site {
        Site::Top => {
            if REFUSED_TOP.contains(&key) {
                Class::Refused
            } else if accepted_top(key) {
                Class::Accepted
            } else if DEAD_TOP.contains(&key) {
                Class::DeadInGc
            } else if ANNO_TOP.contains(&key) {
                Class::Annotation
            } else {
                Class::Unknown
            }
        }
        Site::Step => {
            if REFUSED_STEP.contains(&key) {
                Class::Refused
            } else if accepted_step(key) {
                Class::Accepted
            } else if ANNO_STEP.contains(&key) {
                Class::Annotation
            } else {
                Class::Unknown
            }
        }
        Site::Check => match key {
            "check" | "max_attempts" => Class::Accepted,
            _ => Class::Unknown,
        },
        // §4 trap 1: `mode` is DEAD at Top and LOAD-BEARING here.
        Site::CheckInner => match key {
            "mode" | "path" | "timeout" => Class::Accepted,
            _ => Class::Unknown,
        },
        Site::Retry => match key {
            "max_attempts" | "on_exhausted" => Class::Accepted,
            _ => Class::Unknown,
        },
        Site::OnComplete => match key {
            "bond" | "for_each" | "parallel" | "sequential" | "vars" => Class::Accepted,
            _ => Class::Unknown,
        },
        Site::Drain => {
            if REFUSED_DRAIN.contains(&key) {
                Class::Refused
            } else {
                match key {
                    "context" | "formula" | "item" | "member_access" | "on_item_failure" => {
                        Class::Accepted
                    }
                    _ => Class::Unknown,
                }
            }
        }
        // §4 trap 1 again: `single_lane` is DEAD at Top and a real (if
        // behaviour-free — F5) key here.
        Site::DrainItem => match key {
            "single_lane" => Class::Accepted,
            _ => Class::Unknown,
        },
    }
}

fn refusal(construct: &str, key: &str, reason: String) -> Refusal {
    Refusal {
        construct: construct.to_owned(),
        key: key.to_owned(),
        reason,
        step: None,
    }
}

/// The VALUE-AWARE refusal layer. [`classify`] alone cannot express
/// `phase = "vapor"` (refused on the key, but the reason must name the value)
/// nor a `gc.kind = "scope"` hiding inside an otherwise-accepted step
/// `metadata` map (§4 trap 2).
///
/// `at` is the full construct location (e.g. `steps.review.drain.context`).
/// The returned [`Refusal`] carries `step: None`; the caller stamps the step
/// id, because BD2 makes refusals STEP-SCOPED — a refusal on a step that
/// condition-pruning drops must die with it.
pub fn refuse(site: Site, key: &str, value: &Value, at: &str) -> Option<Refusal> {
    // ---- value-aware rules, before the key-only ones.
    match (site, key) {
        // A scope-check formula's scope-ness lives entirely in step-metadata
        // VALUES. Camp inspects the AUTHORED metadata (gc's COMPILER also
        // emits `gc.kind: scope` on generated ralph bodies — camp generates
        // none, so there is nothing to confuse it with).
        (Site::Step, "metadata") => {
            let map = value.as_table()?;
            if map.get("gc.kind").and_then(Value::as_str) == Some("scope") {
                return Some(refusal(
                    at,
                    "gc.kind",
                    "`gc.kind = \"scope\"` marks a Gas City scope-check step; camp does not \
                     implement scope checks and refuses rather than approximate them \
                     (compat §4 rule 1)"
                        .to_owned(),
                ));
            }
            let scoped = map.keys().find(|k| k.starts_with("gc.scope_"))?;
            return Some(refusal(
                at,
                scoped,
                format!(
                    "`{scoped}` is Gas City scope-check metadata; camp does not implement \
                     scope checks and refuses rather than approximate them (compat §4 rule 1)"
                ),
            ));
        }
        // §9: shared drains are REFUSED, loudly. 12 of the corpus's 13 sit
        // behind a `{{drain_policy}} == same-session` condition and are PRUNED
        // before this refusal is ever collected (BD2); the 13th is
        // unconditional and is one of the 5 camp cannot load.
        (Site::Drain, "context") if value.as_str() == Some("shared") => {
            return Some(refusal(
                at,
                "context",
                "`context = \"shared\"` is a same-session drain: the items run inside the \
                 drain owner's own session. camp has no same-session execution and refuses \
                 rather than silently running them as separate sessions (compat §9)"
                    .to_owned(),
            ));
        }
        _ => {}
    }

    // ---- key-only §4 rule-1 refusals.
    if classify(site, key) != Class::Refused {
        return None;
    }
    let reason = match key {
        // Not a pointer to the city: gc formula-v2 hard-removed [steps.tally].
        "tally" => "`tally` was removed from Gas City formula v2; neither camp nor current \
                    gc accepts it (compat §4 rule 1)"
            .to_owned(),
        "phase" => format!(
            "`phase` ({}) is a Gas City molecule-phase key; camp has no phase machinery and \
             refuses rather than ignore it (compat §4 rule 1)",
            value
                .as_str()
                .map_or_else(|| value.to_string(), |s| format!("= {s:?}"))
        ),
        _ => format!(
            "`{key}` is a Gas City construct camp does not implement; camp refuses it by name \
             rather than approximate its semantics (compat §4 rule 1)"
        ),
    };
    Some(refusal(at, key, reason))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn val(s: &str) -> Value {
        Value::String(s.to_owned())
    }

    #[test]
    fn classify_matches_section_4s_table() {
        // Accepted — the base sets.
        for k in ["formula", "description", "steps", "requires"] {
            assert_eq!(classify(Site::Top, k), Class::Accepted, "top {k}");
        }
        for k in ["id", "title", "needs", "check", "retry", "on_complete"] {
            assert_eq!(classify(Site::Step, k), Class::Accepted, "step {k}");
        }
        // Accepted — every rung key, at its own site.
        for r in RUNGS {
            for k in r.top {
                assert_eq!(classify(Site::Top, k), Class::Accepted, "rung top {k}");
            }
            for k in r.step {
                assert_eq!(classify(Site::Step, k), Class::Accepted, "rung step {k}");
            }
        }
        // Refused — §4 rule 1.
        for k in ["advice", "compose", "phase", "pointcuts", "pour"] {
            assert_eq!(classify(Site::Top, k), Class::Refused, "top {k}");
        }
        for k in ["depends_on", "gate", "loop", "tally", "waits_for"] {
            assert_eq!(classify(Site::Step, k), Class::Refused, "step {k}");
        }
        // Dead in gc — ignored + warned in BOTH tiers.
        for k in [
            "version",
            "target_required",
            "internal",
            "mode",
            "single_lane",
            "sling_container_mode",
        ] {
            assert_eq!(classify(Site::Top, k), Class::DeadInGc, "dead {k}");
        }
        // Annotations — silent in both tiers.
        for k in ["notes", "catalog", "metadata"] {
            assert_eq!(classify(Site::Top, k), Class::Annotation, "anno top {k}");
        }
        for k in ["notes", "tags", "priority"] {
            assert_eq!(classify(Site::Step, k), Class::Annotation, "anno step {k}");
        }
        // Unknown — recognised by nobody.
        assert_eq!(classify(Site::Top, "bogus"), Class::Unknown);
        assert_eq!(classify(Site::Step, "dependson"), Class::Unknown);
    }

    #[test]
    fn trap_1_the_same_word_is_a_different_key_at_a_different_nesting() {
        // §4 trap 1, the whole reason `classify` takes a Site: top-level
        // `mode`/`single_lane` are gc's DEAD config; the same words nested are
        // load-bearing.
        assert_eq!(classify(Site::Top, "mode"), Class::DeadInGc);
        assert_eq!(classify(Site::CheckInner, "mode"), Class::Accepted);
        assert_eq!(classify(Site::Top, "single_lane"), Class::DeadInGc);
        assert_eq!(classify(Site::DrainItem, "single_lane"), Class::Accepted);
        // And `type`: a real top-level key (rung 2d), recognised by nobody on
        // a step.
        assert_eq!(classify(Site::Top, "type"), Class::Accepted);
        assert_eq!(classify(Site::Step, "type"), Class::Unknown);
        // `metadata`: a step key camp accepts (rung 2a); a formula-level
        // annotation.
        assert_eq!(classify(Site::Step, "metadata"), Class::Accepted);
        assert_eq!(classify(Site::Top, "metadata"), Class::Annotation);
    }

    #[test]
    fn the_rung_table_is_section_9s_table_verbatim() {
        // A LITERAL transcription of §9's ladder — never derived from the
        // constant under test, which would make this true by construction and
        // unable to fail.
        let expected: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
            ("2a", vec!["contract"], vec!["description_file", "metadata"]),
            ("2b", vec!["vars"], vec!["condition"]),
            ("2c", vec!["extends"], vec![]),
            (
                "2d",
                vec!["type", "template"],
                vec!["expand", "expand_vars", "children"],
            ),
            ("2e", vec![], vec!["drain"]),
        ];
        assert_eq!(RUNGS.len(), expected.len(), "rung count");
        for (rung, (id, top, step)) in RUNGS.iter().zip(&expected) {
            assert_eq!(rung.id, *id);
            assert_eq!(rung.top, top.as_slice(), "rung {id} top");
            assert_eq!(rung.step, step.as_slice(), "rung {id} step");
        }
    }

    #[test]
    fn phase_is_refused_by_key_and_the_reason_names_the_value() {
        let r = refuse(Site::Top, "phase", &val("vapor"), "phase").expect("phase must refuse");
        assert_eq!(r.key, "phase");
        assert_eq!(r.construct, "phase");
        assert!(r.reason.contains("vapor"), "{}", r.reason);
        // Formula-scoped: nothing can prune it away.
        assert_eq!(r.step, None);
    }

    #[test]
    fn a_scope_check_hiding_in_step_metadata_values_is_refused() {
        // §4 trap 2: the KEY (`metadata`) is accepted; the VALUE refuses.
        //
        // NOTE the QUOTED key. The corpus authors these as
        // `metadata = { "gc.kind" = "scope" }` — a FLAT map whose key is
        // literally `gc.kind`. Spelled bare, TOML would read `gc.kind` as a
        // nested table `{gc: {kind: ...}}` and the rule would never fire.
        let md: Value = toml::from_str(r#""gc.kind" = "scope""#).unwrap();
        let r = refuse(Site::Step, "metadata", &md, "steps.a.metadata").expect("must refuse");
        assert_eq!(r.key, "gc.kind");
        assert_eq!(r.construct, "steps.a.metadata");

        // Any `gc.scope_*` key, reported BY ITS OWN NAME.
        let md: Value = toml::from_str(r#""gc.scope_budget" = "3""#).unwrap();
        let r = refuse(Site::Step, "metadata", &md, "steps.a.metadata").expect("must refuse");
        assert_eq!(r.key, "gc.scope_budget");
    }

    #[test]
    fn unimplemented_is_site_aware_because_on_complete_vars_is_not_top_level_vars() {
        // §4 trap 1, and it really bit: matching UNIMPLEMENTED by NAME fired on
        // `steps.<id>.on_complete.vars` — an implemented, load-bearing key —
        // because top-level `vars` (rung 2b) happened to share its spelling. That
        // rejected every fan-out formula in the corpus.
        //
        // Rung 2b has since landed, so `vars` is no longer on the list and this
        // pair can no longer collide. The guard STAYS as a regression fence: a
        // future rung key that shares a nested key's name must not resurrect the
        // bug, and `on_complete.vars` must never be "unimplemented" at any point.
        assert!(
            !is_unimplemented(Site::OnComplete, "vars"),
            "on_complete.vars is a DIFFERENT key that merely shares a name"
        );
        // The rule itself: a rung key is unimplemented only AT ITS OWN SITE.
        assert!(is_unimplemented(Site::Step, "drain"));
        assert!(!is_unimplemented(Site::Top, "drain"), "drain is a STEP key");
        assert!(is_unimplemented(Site::Top, "extends"));
        assert!(
            !is_unimplemented(Site::Step, "extends"),
            "extends is a TOP key"
        );
        // Landed rungs are not unimplemented.
        assert!(!is_unimplemented(Site::Top, "vars"));
        assert!(!is_unimplemented(Site::Step, "condition"));
        assert!(!is_unimplemented(Site::Top, "contract"));
        // Nothing on the base sets may ever be unimplemented.
        assert!(!is_unimplemented(Site::Step, "check"));
        assert!(!is_unimplemented(Site::Top, "steps"));
    }

    #[test]
    fn a_cleanup_kind_and_a_run_target_are_not_refused() {
        // Only `scope` is refused — `cleanup` (1 corpus use) is not, and the
        // routing/annotation metadata rides through untouched.
        for md in [
            r#"gc.kind = "cleanup""#,
            r#""gc.run_target" = "superpowers.implementer""#,
            r#""gc.continuation_group" = "impl""#,
            r#""gc.build.artifact_schema" = "x""#,
            r#""gc.on_fail" = "stop""#,
        ] {
            let v: Value = toml::from_str(md).unwrap();
            assert!(
                refuse(Site::Step, "metadata", &v, "steps.a.metadata").is_none(),
                "{md} must ride through"
            );
        }
    }

    #[test]
    fn a_shared_drain_is_refused_and_a_separate_one_is_not() {
        let r = refuse(
            Site::Drain,
            "context",
            &val("shared"),
            "steps.impl.drain.context",
        )
        .expect("shared must refuse");
        assert_eq!(r.key, "context");
        assert!(
            refuse(
                Site::Drain,
                "context",
                &val("separate"),
                "steps.impl.drain.context"
            )
            .is_none(),
            "separate is camp's implemented shape"
        );
    }

    #[test]
    fn continuation_group_and_max_units_are_refused_by_name_on_a_drain() {
        // The drain KEYS (0 corpus uses). Distinct from the METADATA key
        // `gc.continuation_group` (29 uses), which is accepted — see
        // `a_cleanup_kind_and_a_run_target_are_not_refused`.
        for k in ["continuation_group", "max_units"] {
            let r = refuse(Site::Drain, k, &val("x"), &format!("steps.impl.drain.{k}"))
                .unwrap_or_else(|| panic!("{k} must refuse"));
            assert_eq!(r.key, k);
        }
    }

    #[test]
    fn a_step_scoped_refusal_carries_its_step_id() {
        // `refuse` returns step: None; the WALK stamps the id, because BD2
        // makes a step's refusals die with the step when it is pruned.
        let mut r = refuse(Site::Step, "gate", &val("x"), "steps.a.gate").expect("gate refuses");
        assert_eq!(r.step, None, "refuse() is location-blind");
        r.step = Some("a".to_owned());
        assert_eq!(r.step.as_deref(), Some("a"));
    }

    #[test]
    fn unimplemented_names_only_keys_the_table_accepts() {
        // §4 trap 3: an UNIMPLEMENTED key must be one the table ACCEPTS —
        // otherwise it is refused/unknown and the const is lying about why the
        // formula failed.
        for k in UNIMPLEMENTED {
            let accepted = classify(Site::Top, k) == Class::Accepted
                || classify(Site::Step, k) == Class::Accepted;
            assert!(accepted, "UNIMPLEMENTED key {k:?} is not an accepted key");
        }
        // And every one of them is a rung key — nothing from the base set may
        // ever be unimplemented.
        for k in UNIMPLEMENTED {
            assert!(
                RUNGS
                    .iter()
                    .any(|r| r.top.contains(k) || r.step.contains(k)),
                "UNIMPLEMENTED key {k:?} is not on any rung"
            );
        }
    }
}
