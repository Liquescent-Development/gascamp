//! `drain` — gc's scatter/gather construct (compat §9 rung 2e), restricted to
//! what camp implements.
//!
//! **F2: gc's compiled Recipe has NO `Drain` struct.** A drain lives entirely in
//! the step's METADATA (`gc.drain_*`), written by `ApplyDrainControlMetadata`
//! (`compile.go:584-614`). Camp parses `drain` into a typed struct AND emits the
//! same metadata, because that is where gc keeps it and the differential gate
//! diffs the map.
//!
//! **F5/F6: `single_lane` and `on_item_failure` are DEAD CONFIG AT RUNTIME — in
//! gc.** `single_lane` has ZERO production readers (`types.go:371`: *"reserved
//! for future shared drains"*; its only readers are the compiler that writes it
//! and the validator). `on_item_failure` is read ONLY by `advanceSharedDrain`
//! (`drain.go:467`), so for `context = "separate"` gc is ALWAYS effectively
//! `continue`. Camp parses, validates and round-trips both with **no runtime
//! behavior behind them** — matching gc exactly. §9's claim that camp "honours
//! `single_lane` mechanically" is a source-read mistake and is amended.

use serde::{Deserialize, Serialize};

/// gc's metadata keys, VERBATIM (`beadmeta/keys.go`; invariant 7).
pub const KIND: &str = "gc.kind";
pub const KIND_DRAIN: &str = "drain";
pub const CONTEXT: &str = "gc.drain_context";
pub const FORMULA: &str = "gc.drain_formula";
pub const MEMBER_ACCESS: &str = "gc.drain_member_access";
pub const ON_ITEM_FAILURE: &str = "gc.drain_on_item_failure";
pub const ITEM_SINGLE_LANE: &str = "gc.drain_item_single_lane";

/// gc's `defaultDrainMaxUnits` (`drain.go:24`). The KEY `drain.max_units` is
/// REFUSED by name (0 corpus uses, §4 rule 1) — but gc applies this as a RUNTIME
/// cap and hard-closes a drain whose member set exceeds it (`drain.go:244-255`,
/// reason `limit_exceeded`). Camp honours the cap.
///
/// Refusing the authored key while honouring the runtime cap is the only
/// combination that neither invents semantics nor scatters 200 workers where gc
/// fails.
pub const DEFAULT_MAX_UNITS: usize = 100;

/// gc's `DrainSpec` (`types.go:341`), restricted to what camp implements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Drain {
    /// Always `Separate` — `Shared` is REFUSED (§9), by a value-aware refusal in
    /// `keys::refuse`, so it never reaches this struct.
    pub context: DrainContext,
    /// The item formula. Rejected at VALIDATION if it contains `{{` — gc's own
    /// rule (`graphv2_validation.go:417-419`: *"templated item formula names are
    /// not supported in v0"*), NOT a substitution exemption.
    pub formula: String,
    /// Default `Read` (gc `compile.go:590-598`).
    pub member_access: MemberAccess,
    /// Default `Continue` for a separate drain. PARSED, NOT ACTED ON (F6).
    pub on_item_failure: OnItemFailure,
    /// `single_lane`. PARSED, NOT ACTED ON (F5).
    pub item: DrainItem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrainContext {
    Separate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberAccess {
    Read,
    Exclusive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnItemFailure {
    Continue,
    SkipRemaining,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DrainItem {
    #[serde(default)]
    pub single_lane: bool,
}

impl DrainContext {
    pub fn as_str(self) -> &'static str {
        match self {
            DrainContext::Separate => "separate",
        }
    }
}

impl MemberAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            MemberAccess::Read => "read",
            MemberAccess::Exclusive => "exclusive",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(MemberAccess::Read),
            "exclusive" => Some(MemberAccess::Exclusive),
            _ => None,
        }
    }
}

impl OnItemFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            OnItemFailure::Continue => "continue",
            OnItemFailure::SkipRemaining => "skip_remaining",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "continue" => Some(OnItemFailure::Continue),
            "skip_remaining" => Some(OnItemFailure::SkipRemaining),
            _ => None,
        }
    }
}

impl Drain {
    /// gc's `ApplyDrainControlMetadata` (`compile.go:584-614`), reproduced
    /// exactly. This is what the differential gate (assertion B) diffs against
    /// gc's own emitted `gc.drain_*` map for all 20 of its drain steps.
    ///
    /// Camp never emits `gc.drain_max_units` or `gc.drain_continuation_group` —
    /// both keys are REFUSED (0 corpus uses), so a formula carrying one never
    /// reaches here. `gc.drain_item_single_lane` is written ONLY when true, as gc
    /// writes it.
    pub fn metadata(&self) -> std::collections::BTreeMap<String, String> {
        let mut md = std::collections::BTreeMap::new();
        md.insert(KIND.to_owned(), KIND_DRAIN.to_owned());
        md.insert(CONTEXT.to_owned(), self.context.as_str().to_owned());
        md.insert(FORMULA.to_owned(), self.formula.clone());
        md.insert(
            MEMBER_ACCESS.to_owned(),
            self.member_access.as_str().to_owned(),
        );
        md.insert(
            ON_ITEM_FAILURE.to_owned(),
            self.on_item_failure.as_str().to_owned(),
        );
        if self.item.single_lane {
            md.insert(ITEM_SINGLE_LANE.to_owned(), "true".to_owned());
        }
        md
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn drain() -> Drain {
        Drain {
            context: DrainContext::Separate,
            formula: "bmad-story-development".into(),
            member_access: MemberAccess::Exclusive,
            on_item_failure: OnItemFailure::Continue,
            item: DrainItem::default(),
        }
    }

    #[test]
    fn a_drain_compiles_to_gcs_gc_drain_metadata_exactly() {
        // F2/F3 — the exact 5-key map, straight out of `bmad-build`'s real Recipe.
        let md = drain().metadata();
        assert_eq!(md.len(), 5, "{md:?}");
        assert_eq!(md["gc.kind"], "drain");
        assert_eq!(md["gc.drain_context"], "separate");
        assert_eq!(md["gc.drain_formula"], "bmad-story-development");
        assert_eq!(md["gc.drain_member_access"], "exclusive");
        // DEFAULTED by the compiler — the author wrote nothing.
        assert_eq!(md["gc.drain_on_item_failure"], "continue");
        // Not written unless true, exactly as gc writes it.
        assert!(!md.contains_key("gc.drain_item_single_lane"));
    }

    #[test]
    fn single_lane_is_written_only_when_true() {
        let mut d = drain();
        d.item.single_lane = true;
        let md = d.metadata();
        assert_eq!(md["gc.drain_item_single_lane"], "true");
        assert_eq!(md.len(), 6);
    }

    #[test]
    fn camp_never_emits_the_keys_it_refuses() {
        // `max_units` and `continuation_group` are REFUSED by name (§4 rule 1),
        // so a formula carrying one never compiles and this map can never hold
        // them. The METADATA key `gc.continuation_group` is a DIFFERENT thing (29
        // authored uses) and rides through on the step's own metadata.
        let md = drain().metadata();
        assert!(!md.contains_key("gc.drain_max_units"));
        assert!(!md.contains_key("gc.drain_continuation_group"));
    }
}
