# Phase 5 — Formula Subset Compiler + Cook: Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Parse and validate exactly the camp formula subset (spec §8.2) with Gas City's syntax and semantics, reject every city-only construct with an error naming it and pointing to the city, and cook validated formulas into dispatchable run graphs in one ledger transaction.

**Architecture:** A two-layer compiler in `camp-core/src/formula/`: `parse.rs` walks raw TOML against explicit acceptance/rejection key tables (collecting ALL violations — serde `deny_unknown_fields` alone stops at the first, so the walk is manual), `validate.rs` runs semantic checks (ids, cycles, combination rules, the explicit-declaration rule, semver), and `cook.rs` materializes `runs/<run-id>/` plus root/step beads plus a camp-specific `run.cooked` event through Phase 1's `Ledger::append_batch` (one transaction). `camp doctor --formula` exposes the validator; a fixture corpus is the acceptance/rejection table in file form and doubles as Phase 6's gc-compat CI input.

**Tech Stack:** Rust (edition 2024), toml 1.1, serde/serde_json, semver 1.x (new dep), fastrand 2.x (new dep), rusqlite via existing Ledger, assert_cmd/predicates/tempfile for tests.

## Global Constraints

- Never commit to main; branch `phase-5-formula-subset`; no co-author lines (AGENTS.md).
- No panics in library code: clippy `unwrap_used`/`expect_used`/`panic` are workspace-denied; `#![forbid(unsafe_code)]`.
- Fail fast; no fallbacks; no silenced errors (AGENTS.md invariant 5).
- **Repo invariant 6:** every valid camp formula is a valid Gas City formula-v2 file. Camp may be stricter than gc, never looser.
- **Repo invariant 7:** event names — `run.cooked` is camp-specific and must not exist in gc's registry (it does not; verified against `tests/fixtures/gc-vocab.json`).
- One-transaction event+state property (Phase 1): cook uses ONE `append_batch`; the vocab-pin partition tests and the refold property test must stay green.
- Shared files (`crates/camp/src/main.rs`, `camp-core/src/event.rs`, `src/vocab.rs`, `src/ledger/fold.rs`, both `Cargo.toml`s, `Cargo.lock`) get minimal, additive edits only — sibling phases 4 and 7 touch them too.
- Gates before push: `cargo fmt --all --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace`.
- TDD strictly: failing test first, watch it fail, implement, watch it pass, commit.

## Ground truth from the real gc compiler (recon of /Users/kiener/code/gascity, HEAD d737e4f72ca6752a3f0bbed21c5a560ed526e5fe, branch feat/docker-tutorial-env, clean tree; authoritative doc `docs/reference/specs/formula-spec-v2.md` — verified to match code)

These facts shape fixtures and validation; Phase 6 CI re-verifies against the pinned gc ref:

1. **gc's decoder is non-strict**: unknown TOML keys are *silently ignored* (spec-v2 §1.2/§1.3). Hard-error exceptions: unknown `[requires]` axes, `[steps.tally]` ("steps.tally was removed from the SDK"), non-`exec` check modes, reserved `gc.*` metadata. Consequence: camp rejecting more keys can never make a camp-valid file gc-invalid.
2. **gc does NOT enforce `formula` = file stem** (lookup is by name, header is namespacing only). Camp enforces stem equality — strictly tighter, so invariant 6 holds.
3. **`Validate()` runs inside Resolve under `CompileWithoutRuntimeVarValidation`** — dup ids, unknown `needs`, cycles, combination rules all execute in Phase 6's CI shim. Valid fixtures must clear them all.
4. **Durations are Go `time.ParseDuration`** and must be > 0; no days unit. Camp accepts a strict subset (see Task 2) — every camp-accepted duration parses in Go.
5. **Explicit-declaration rule** (gc compile.go:51, spec-v2 §5): graph-only constructs — `check`, `retry`, `drain`, `on_complete` (and reserved `gc.*` metadata) — require `[requires] formula_compiler = ">=2.0.0"`. gc's error: `requires: formulas that use graph-only constructs must declare [requires] formula_compiler = ">=2.0.0" or the deprecated contract = "graph.v2" explicitly`. `needs` is NOT graph-only — a plain multi-step dag needs no declaration.
6. **Step `timeout` requires `check`** (spec-v2 §1.3: "Max duration for this step's `check` script; requires `check`"). Camp must enforce this or a camp-valid file could flunk gc.
7. **`[requires]`**: `formula_compiler` is the only axis; value must be a semver comparator (gc: `formula.compiler_requirement_invalid: formula_compiler must be a semver comparator, for example ">=2.0.0"`); v2 host capability is 2.x (1.0.0 only when `formula_v2` is disabled).
8. **Check** (spec-v2 §3.1): `max_attempts` ≥ 1 (total attempts incl. first); inner `[steps.check.check]` with `mode` (only `"exec"`), `path`, `timeout` (takes precedence over step `timeout`). gc combination rule: check ∦ {loop, on_complete, gate, expand, assignee, retry}; camp's mirrored subset: **check ∦ {retry, assignee}** (the rest are rejected constructs anyway).
9. **Retry** (spec-v2 §3.2): `max_attempts` ≥ 1; `on_exhausted` = `hard_fail` (default) | `soft_fail`. gc: retry ∦ {check, loop, on_complete, gate, expand, children}; camp's mirrored subset: **retry ∦ {check, on_complete}**.
10. **On-complete** (spec-v2 §3.4): `for_each` must start with `output.`; `for_each` and `bond` must be set together; `vars` binds `{item}`/`{item.field}`/`{index}`; `parallel`/`sequential` are mutually exclusive keys (parallel is the default mode).
11. **Every construct camp rejects is legal gc v2** (spec-v2 §1.2/§1.3 tables): top-level `extends`, `vars`, `type`, `phase`, `pour`, `contract`, `catalog`, `template`, `compose`, `advice`, `pointcuts`; step-level `description_file`, `notes`, `type`, `priority`, `tags`, `metadata`, `depends_on`, `condition`, `children`, `expand`, `expand_vars`, `loop`, `waits_for`, `gate`, `drain`, `tally`. Rejecting them yields a genuine strict subset.

## Contract deviations (flagged for operator approval — all additive)

1. **`Formula.source: String`** added to the pinned struct. Cook must pin a *verbatim* copy of the authored file into `runs/<run-id>/` (spec §8.2; §15.3 exports pinned formulas to a city, so byte-fidelity matters). `parse_and_validate(path)` captures the file text; re-serializing the AST would lose the authored form.
2. **`cook(..., rig: &RigConfig, ...)`** instead of `rig: &str`. Cook needs the rig *name* (event `rig` field) AND the rig *prefix* (bead id allocation, Phase 3 semantics). A bare `&str` cannot carry both; `camp_core::config::RigConfig` already exists and carries exactly this.
3. **`Ledger::now_utc()`** — one-line additive accessor so `run_id` timestamps come from the same `Clock` as events (deterministic under `FixedClock` in tests).
4. **Extra city-pointer rows**: beyond the master plan's rejection list, the remaining gc-legal keys (`contract`, `catalog`, `template`, `compose`, `advice`, `pointcuts`, `expand_vars`) also get named city-pointer errors instead of generic unknown-key errors. Strictly better diagnostics; same outcome (rejection).
5. **`timeout` requires `check`** validation (gc ground-truth fact 6). Spec §8.2 already describes step `timeout` as "general bound on the check script", so no spec change needed — the plan just makes the rule explicit.

## File Structure

```
crates/camp-core/
  Cargo.toml                    # + semver = "1.0.27", fastrand = "2.3.0"
  src/lib.rs                    # + pub mod formula;
  src/formula/mod.rs            # NEW: parse_and_validate(), re-exports, FormulaError/Violation
  src/formula/ast.rs            # NEW: Formula, Step, Check, CheckMode, Retry, Disposition, OnComplete, Requires
  src/formula/parse.rs          # NEW: raw TOML walk, acceptance/rejection key tables, duration parser
  src/formula/validate.rs       # NEW: semantic checks (ids, cycles, combos, declaration rule, semver)
  src/formula/cook.rs           # NEW: cook() -> CookedRun; run_id; pinned copy; manifest.json
  src/event.rs                  # + EventType::RunCooked ("run.cooked")           [shared: additive]
  src/vocab.rs                  # + "run.cooked" in CAMP_SPECIFIC_EVENTS          [shared: additive]
  src/ledger/fold.rs            # + run_id/step_id on BeadCreated; RunCooked arm  [shared: additive]
  src/ledger/mod.rs             # + Ledger::now_utc()                             [additive]
  tests/formula_corpus.rs       # NEW: table-driven acceptance/rejection over the fixture corpus
  tests/cook.rs                 # NEW: cook happy path, readiness, atomicity, refold, file-independence
  tests/fixtures/formulas/valid/*.toml    # NEW corpus (5 files)
  tests/fixtures/formulas/invalid/*.toml  # NEW corpus (one per rejection row + semantic rows)
crates/camp/
  src/main.rs                   # Doctor: --refold OR --formula <path>            [shared: minimal]
  src/cmd/doctor.rs             # + run_formula(path)
  tests/cli_doctor_formula.rs   # NEW: exit 0/1, all violations printed
docs/superpowers/plans/2026-07-06-phase-5-formula-subset.md  # this plan
```

Commit after every task. Run `cargo test -p camp-core` (or `-p camp`) after every test/implement step pair.

---

### Task 1: AST types and FormulaError

**Files:**
- Modify: `crates/camp-core/Cargo.toml` (add `semver`, `fastrand`)
- Modify: `crates/camp-core/src/lib.rs` (add `pub mod formula;`)
- Create: `crates/camp-core/src/formula/mod.rs`
- Create: `crates/camp-core/src/formula/ast.rs`
- Test: inline `#[cfg(test)]` in `ast.rs`

**Interfaces:**
- Consumes: nothing (leaf types).
- Produces: `Formula`, `Step`, `Check`, `CheckMode`, `Retry`, `Disposition`, `OnComplete`, `Requires`, `FormulaError`, `Violation` — used by every later task. All construction is by struct literal; no builder.

- [ ] **Step 1: Write the failing test** — in `crates/camp-core/src/formula/ast.rs`:

```rust
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn formula_error_display_lists_every_violation_with_its_construct() {
        let err = FormulaError {
            path: std::path::PathBuf::from("bad.toml"),
            violations: vec![
                Violation { construct: "drain".into(), message: "x".into() },
                Violation { construct: "steps.review.needs".into(), message: "y".into() },
            ],
        };
        let text = err.to_string();
        assert!(text.contains("bad.toml"), "{text}");
        assert!(text.contains("2 violation"), "{text}");
        assert!(text.contains("drain: x"), "{text}");
        assert!(text.contains("steps.review.needs: y"), "{text}");
    }

    #[test]
    fn disposition_and_check_mode_spell_gc_vocabulary() {
        assert_eq!(Disposition::HardFail.as_str(), "hard_fail");
        assert_eq!(Disposition::SoftFail.as_str(), "soft_fail");
        assert_eq!(CheckMode::Exec.as_str(), "exec");
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p camp-core formula` — expected: compile error (module does not exist).

- [ ] **Step 3: Implement**

`crates/camp-core/Cargo.toml` — add to `[dependencies]` (alphabetical, matching file style):

```toml
fastrand = "2.3.0"
semver = "1.0.27"
```

`crates/camp-core/src/lib.rs` — add `pub mod formula;` to the module list (alphabetical: after `pub mod event;`).

`crates/camp-core/src/formula/mod.rs`:

```rust
//! The camp formula subset compiler (spec §8.2). Every valid camp formula
//! is a valid Gas City formula-v2 file (repo invariant 6): camp adopts
//! constructs with gc's exact syntax and semantics or not at all, and camp
//! is strictly *tighter* — it rejects every city-only construct by name and
//! accepts no unknown keys, where gc silently ignores them.

pub mod ast;
mod cook;   // Task 7 (add the line in Task 7)
mod parse;  // Task 3 (add the line in Task 3)
mod validate; // Task 4 (add the line in Task 4)

pub use ast::{
    Check, CheckMode, Disposition, Formula, FormulaError, OnComplete, Requires, Step, Violation,
};
```

(Only declare `pub mod ast;` and the re-exports now; add `parse`/`validate`/`cook` lines in their tasks.)

`crates/camp-core/src/formula/ast.rs`:

```rust
//! The camp formula AST — the master-plan Phase 5 pinned interfaces, plus
//! `Formula::source` (the verbatim authored bytes, pinned into the run dir
//! by cook; re-serializing would lose the authored form).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq)]
pub struct Formula {
    pub name: String,
    pub description: Option<String>,
    pub requires: Option<Requires>,
    pub steps: Vec<Step>,
    /// Verbatim bytes of the authored file (contract deviation 1).
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Requires {
    /// A semver comparator, e.g. ">=2.0.0" (gc: the only [requires] axis).
    pub formula_compiler: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub needs: Vec<String>,
    pub assignee: Option<String>,
    /// General bound on the step's check script (gc: requires `check`).
    pub timeout: Option<Duration>,
    pub check: Option<Check>,
    pub retry: Option<Retry>,
    pub on_complete: Option<OnComplete>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckMode {
    Exec,
}

impl CheckMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckMode::Exec => "exec",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub max_attempts: u32,
    pub mode: CheckMode,
    pub path: PathBuf,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    HardFail,
    SoftFail,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Disposition::HardFail => "hard_fail",
            Disposition::SoftFail => "soft_fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Retry {
    pub max_attempts: u32,
    /// Default hard_fail (gc formula-spec-v2 §3.2).
    pub on_exhausted: Disposition,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OnComplete {
    /// Path into structured step output; must start with "output.".
    pub for_each: String,
    /// Formula instantiated per item; set together with `for_each`.
    pub bond: String,
    pub vars: BTreeMap<String, String>,
    /// true = parallel (gc default); `sequential = true` sets false.
    pub parallel: bool,
}

/// One rule violation. `construct` names what the message is about (a
/// rejected key like "drain", or a location like "steps.review.needs") so
/// tests and users can see exactly which construct failed.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    pub construct: String,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.construct, self.message)
    }
}

/// The complete verdict on one formula file: ALL violations, never just the
/// first (master-plan Phase 5 contract).
#[derive(Debug)]
pub struct FormulaError {
    pub path: PathBuf,
    pub violations: Vec<Violation>,
}

impl std::fmt::Display for FormulaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{}: {} violation(s):",
            self.path.display(),
            self.violations.len()
        )?;
        for v in &self.violations {
            writeln!(f, "  {v}")?;
        }
        Ok(())
    }
}

impl std::error::Error for FormulaError {}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p camp-core formula` — expected: 2 passed. Also `cargo clippy -p camp-core --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/Cargo.toml Cargo.lock crates/camp-core/src/lib.rs crates/camp-core/src/formula/
git commit -m "feat: formula AST and FormulaError with all-violations reporting"
```

### Task 2: Go-compatible duration parser

**Files:**
- Modify: `crates/camp-core/src/formula/parse.rs` (create the file with just this function; the walk arrives in Task 3)
- Test: inline in `parse.rs`

**Interfaces:**
- Produces: `pub(crate) fn parse_duration(s: &str) -> Result<std::time::Duration, String>` — used by Task 3 for `timeout` and `check.timeout` fields.
- Rule: strict subset of Go `time.ParseDuration` (ground-truth fact 4): one or more `<positive integer><unit>` segments, units `ms`, `s`, `m`, `h`; total must be > 0. No sign, no decimals, no days, no bare numbers. Everything camp accepts parses identically in Go; camp rejects Go-legal exotica (`1.5h`, `-3s`, `100ns`) it does not need.

- [ ] **Step 1: Write the failing test** — create `crates/camp-core/src/formula/parse.rs`:

```rust
//! Raw TOML walk for the camp formula subset (Task 3) and the duration
//! grammar shared by `timeout` fields.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn duration_grammar_is_a_strict_go_subset() {
        for (input, secs) in [
            ("5m", 300),
            ("2m", 120),
            ("300s", 300),
            ("1h30m", 5400),
            ("1h", 3600),
            ("1500ms", 1), // 1.5s truncates only in this assertion; see below
        ] {
            let d = parse_duration(input).unwrap();
            assert_eq!(d.as_secs(), secs, "{input}");
        }
        assert_eq!(parse_duration("1500ms").unwrap(), Duration::from_millis(1500));
        for bad in ["", "5", "m", "-3s", "1.5h", "5d", "1h 30m", "0s", "0m", "s5", "5S"] {
            assert!(parse_duration(bad).is_err(), "{bad:?} must be rejected");
        }
    }
}
```

- [ ] **Step 2: Run and watch it fail**

Add `mod parse;` to `formula/mod.rs`. Run: `cargo test -p camp-core duration` — expected: compile error (`parse_duration` not found).

- [ ] **Step 3: Implement** — above the tests module in `parse.rs`:

```rust
use std::time::Duration;

/// Parse a duration in camp's strict subset of Go `time.ParseDuration`
/// (repo invariant 6: everything camp accepts must parse in gc): one or
/// more `<positive integer><unit>` segments with units `ms`|`s`|`m`|`h`,
/// summing to > 0. E.g. "5m", "300s", "1h30m".
pub(crate) fn parse_duration(s: &str) -> Result<Duration, String> {
    const UNITS: &[(&str, u64)] = &[("ms", 1), ("s", 1000), ("m", 60_000), ("h", 3_600_000)];
    let mut rest = s;
    let mut total_ms: u64 = 0;
    if rest.is_empty() {
        return Err("empty duration".to_owned());
    }
    while !rest.is_empty() {
        let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if digits_end == 0 {
            return Err(format!(
                "invalid duration {s:?}: expected digits, found {rest:?}"
            ));
        }
        let (digits, tail) = rest.split_at(digits_end);
        let value: u64 = digits
            .parse()
            .map_err(|e| format!("invalid duration {s:?}: {e}"))?;
        // Longest-match the unit ("ms" before "m").
        let Some((unit, factor)) = UNITS
            .iter()
            .filter(|(u, _)| tail.starts_with(u))
            .max_by_key(|(u, _)| u.len())
        else {
            return Err(format!(
                "invalid duration {s:?}: expected a unit (ms|s|m|h) after {digits:?}"
            ));
        };
        total_ms = total_ms
            .checked_add(value.checked_mul(*factor).ok_or_else(|| {
                format!("invalid duration {s:?}: overflow")
            })?)
            .ok_or_else(|| format!("invalid duration {s:?}: overflow"))?;
        rest = &tail[unit.len()..];
    }
    if total_ms == 0 {
        return Err(format!("invalid duration {s:?}: must be greater than zero"));
    }
    Ok(Duration::from_millis(total_ms))
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p camp-core duration` — expected: 1 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/formula/
git commit -m "feat: strict Go-subset duration grammar for formula timeouts"
```

### Task 3: The raw TOML walk — acceptance and rejection tables

**Files:**
- Modify: `crates/camp-core/src/formula/parse.rs`
- Test: inline in `parse.rs`

**Interfaces:**
- Consumes: `parse_duration` (Task 2), AST types (Task 1).
- Produces: `pub(crate) fn walk(text: &str) -> (RawFormula, Vec<Violation>)` and `pub(crate) struct RawFormula` with these exact fields (Task 4 consumes them):

```rust
pub(crate) struct RawFormula {
    pub name: Option<String>,
    pub description: Option<String>,
    pub formula_compiler: Option<String>, // [requires] formula_compiler, unparsed
    pub steps: Vec<RawStep>,
}
pub(crate) struct RawStep {
    pub index: usize, // position in [[steps]], for error locations before ids exist
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub needs: Vec<String>,
    pub assignee: Option<String>,
    pub timeout: Option<std::time::Duration>,
    pub check: Option<crate::formula::ast::Check>,
    pub retry: Option<RawRetry>,
    pub on_complete: Option<crate::formula::ast::OnComplete>,
}
pub(crate) struct RawRetry {
    pub max_attempts: u32,
    pub on_exhausted: Option<String>, // validated + defaulted in Task 4
}
```

Design: serde `deny_unknown_fields` stops at the first unknown key, but the contract requires ALL violations. So the walk deserializes to `toml::Table` and checks every key by hand against three tables:

1. **Accepted keys** — top: `formula`, `description`, `requires`, `steps`; requires: `formula_compiler`; step: `id`, `title`, `description`, `needs`, `assignee`, `timeout`, `check`, `retry`, `on_complete`; check: `max_attempts`, `check`; inner check: `mode`, `path`, `timeout`; retry: `max_attempts`, `on_exhausted`; on_complete: `for_each`, `bond`, `vars`, `parallel`, `sequential`.
2. **City-only keys** (each is legal gc v2 — ground-truth fact 11 — and gets an error naming the construct and pointing to the city): top-level `extends`, `vars`, `type`, `phase`, `pour`, `contract`, `catalog`, `template`, `compose`, `advice`, `pointcuts`; step-level `drain`, `gate`, `loop`, `expand`, `expand_vars`, `children`, `waits_for`, `condition`, `tally`, `metadata`, `depends_on`, `type`, `priority`, `tags`, `description_file`, `notes`.
3. **Anything else** — rejected as an unknown key (camp accepts no unknown keys, unlike gc's silent ignore — plan decision 5).

Message formats (tests assert the construct name appears):
- City-only: `` `{key}` is a Gas City-only construct; camp does not accept it — run this formula in a Gas City (spec §8.2)``
- Unknown: `` unknown key `{key}`: camp formulas accept no unknown keys (spec §8.2)``
- Wrong type: `` `{key}` must be a {expected}``

`Violation.construct` is the bare key name for city-only/unknown keys (e.g. `"drain"`, `"dependson"`), and the dotted location for typed/value errors (e.g. `"steps[2].timeout"`, `"steps.review.check.max_attempts"` once ids are known — use `steps[{index}]` when `id` is missing/not yet parsed, else `steps.{id}`).

- [ ] **Step 1: Write the failing tests** — append to the `tests` module in `parse.rs`:

```rust
    fn violations(text: &str) -> Vec<crate::formula::ast::Violation> {
        walk(text).1
    }

    fn constructs(text: &str) -> Vec<String> {
        violations(text).into_iter().map(|v| v.construct).collect()
    }

    const MINIMAL: &str = "formula = \"minimal\"\n\n[[steps]]\nid = \"only\"\ntitle = \"Do the thing\"\n";

    #[test]
    fn minimal_formula_walks_clean() {
        let (raw, v) = walk(MINIMAL);
        assert!(v.is_empty(), "{v:?}");
        assert_eq!(raw.name.as_deref(), Some("minimal"));
        assert_eq!(raw.steps.len(), 1);
        assert_eq!(raw.steps[0].id.as_deref(), Some("only"));
        assert_eq!(raw.steps[0].title.as_deref(), Some("Do the thing"));
    }

    #[test]
    fn every_city_only_key_is_rejected_by_name_with_a_city_pointer() {
        for key in ["extends", "vars", "type", "phase", "pour", "contract",
                    "catalog", "template", "compose", "advice", "pointcuts"] {
            let text = format!("{key} = 1\nformula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n");
            let v = violations(&text);
            assert!(
                v.iter().any(|v| v.construct == key && v.message.contains("Gas City")),
                "{key}: {v:?}"
            );
        }
        for key in ["drain", "gate", "loop", "expand", "expand_vars", "children",
                    "waits_for", "condition", "tally", "metadata", "depends_on",
                    "type", "priority", "tags", "description_file", "notes"] {
            let text = format!(
                "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n{key} = 1\n"
            );
            let v = violations(&text);
            assert!(
                v.iter().any(|v| v.construct == key && v.message.contains("Gas City")),
                "step {key}: {v:?}"
            );
        }
    }

    #[test]
    fn unknown_keys_are_rejected_everywhere_gc_would_silently_ignore_them() {
        // gc silently drops a `dependson` typo (formula-spec-v2 §1.3 note);
        // camp names it.
        let text = "formula = \"x\"\nbogus = 1\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ndependson = [\"b\"]\n";
        let c = constructs(text);
        assert!(c.contains(&"bogus".to_owned()), "{c:?}");
        assert!(c.contains(&"dependson".to_owned()), "{c:?}");
    }

    #[test]
    fn walk_collects_all_violations_not_just_the_first() {
        let text = "formula = \"x\"\nvars = {}\npour = true\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntags = [\"x\"]\n";
        let c = constructs(text);
        assert_eq!(c, vec!["pour", "vars", "tags"], "sorted top keys then steps: {c:?}");
    }

    #[test]
    fn check_retry_and_on_complete_tables_parse_with_gc_shapes() {
        let text = r#"
formula = "shapes"
[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"
timeout = "5m"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "scripts/verify.sh"
timeout = "2m"

[[steps]]
id = "b"
title = "t"

[steps.retry]
max_attempts = 2
on_exhausted = "soft_fail"

[[steps]]
id = "c"
title = "t"

[steps.on_complete]
for_each = "output.items"
bond = "minimal"
sequential = true

[steps.on_complete.vars]
name = "{item.name}"
"#;
        let (raw, v) = walk(text);
        assert!(v.is_empty(), "{v:?}");
        let check = raw.steps[0].check.as_ref().unwrap();
        assert_eq!(check.max_attempts, 3);
        assert_eq!(check.path, std::path::PathBuf::from("scripts/verify.sh"));
        assert_eq!(check.timeout, Some(std::time::Duration::from_secs(120)));
        assert_eq!(raw.steps[0].timeout, Some(std::time::Duration::from_secs(300)));
        let retry = raw.steps[1].retry.as_ref().unwrap();
        assert_eq!((retry.max_attempts, retry.on_exhausted.as_deref()), (2, Some("soft_fail")));
        let oc = raw.steps[2].on_complete.as_ref().unwrap();
        assert_eq!(oc.for_each, "output.items");
        assert_eq!(oc.bond, "minimal");
        assert!(!oc.parallel, "sequential = true must flip the default");
        assert_eq!(oc.vars.get("name").map(String::as_str), Some("{item.name}"));
    }

    #[test]
    fn bad_types_and_bad_values_are_violations_with_locations() {
        let text = "formula = 3\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntimeout = \"eleven\"\ncheck = { max_attempts = 1, check = { mode = \"inference\", path = \"x\" } }\n";
        let v = violations(text);
        assert!(v.iter().any(|v| v.construct == "formula" && v.message.contains("string")), "{v:?}");
        assert!(v.iter().any(|v| v.construct == "steps.a.timeout"), "{v:?}");
        assert!(
            v.iter().any(|v| v.construct == "steps.a.check.check.mode"
                && v.message.contains("exec")),
            "{v:?}"
        );
    }

    #[test]
    fn toml_syntax_error_is_a_single_violation() {
        let (_, v) = walk("formula = \"x\"\n[[steps\n");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].construct, "toml");
    }

    #[test]
    fn parallel_and_sequential_both_present_is_a_violation() {
        let text = "formula = \"x\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\non_complete = { for_each = \"output.i\", bond = \"b\", parallel = true, sequential = true }\n";
        let v = violations(text);
        assert!(
            v.iter().any(|v| v.construct == "steps.a.on_complete"
                && v.message.contains("mutually exclusive")),
            "{v:?}"
        );
    }
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p camp-core parse` — expected: compile error (`walk`, `RawFormula` not found).

- [ ] **Step 3: Implement the walk** — in `parse.rs`. Complete implementation:

```rust
use std::path::PathBuf;

use toml::Value;

use crate::formula::ast::{Check, CheckMode, OnComplete, Violation};

pub(crate) struct RawFormula {
    pub name: Option<String>,
    pub description: Option<String>,
    pub formula_compiler: Option<String>,
    pub steps: Vec<RawStep>,
}

pub(crate) struct RawStep {
    pub index: usize,
    pub id: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub needs: Vec<String>,
    pub assignee: Option<String>,
    pub timeout: Option<std::time::Duration>,
    pub check: Option<Check>,
    pub retry: Option<RawRetry>,
    pub on_complete: Option<OnComplete>,
}

pub(crate) struct RawRetry {
    pub max_attempts: u32,
    pub on_exhausted: Option<String>,
}

/// Keys that exist in Gas City formula v2 but are outside camp's subset
/// (spec §8.2 "City-only in v1"; gc formula-spec-v2 §1.2/§1.3).
const CITY_ONLY_TOP: &[&str] = &[
    "advice", "catalog", "compose", "contract", "extends", "phase",
    "pointcuts", "pour", "template", "type", "vars",
];
const CITY_ONLY_STEP: &[&str] = &[
    "children", "condition", "depends_on", "description_file", "drain",
    "expand", "expand_vars", "gate", "loop", "metadata", "notes",
    "priority", "tags", "tally", "type", "waits_for",
];

const ACCEPTED_TOP: &[&str] = &["description", "formula", "requires", "steps"];
const ACCEPTED_STEP: &[&str] = &[
    "assignee", "check", "description", "id", "needs", "on_complete",
    "retry", "timeout", "title",
];

fn city_only(key: &str) -> Violation {
    Violation {
        construct: key.to_owned(),
        message: format!(
            "`{key}` is a Gas City-only construct; camp does not accept it — \
             run this formula in a Gas City (spec §8.2)"
        ),
    }
}

fn unknown(key: &str) -> Violation {
    Violation {
        construct: key.to_owned(),
        message: format!("unknown key `{key}`: camp formulas accept no unknown keys (spec §8.2)"),
    }
}

fn wrong_type(construct: &str, expected: &str) -> Violation {
    Violation {
        construct: construct.to_owned(),
        message: format!("`{construct}` must be {expected}"),
    }
}

/// Sorted keys of a table — deterministic violation order for tests/users.
fn sorted_keys(table: &toml::Table) -> Vec<&str> {
    let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys
}

fn get_string(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Option<String> {
    match table.get(key) {
        None => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            out.push(wrong_type(construct, "a string"));
            None
        }
    }
}

fn get_string_array(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Vec<String> {
    match table.get(key) {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut result = Vec::new();
            for item in items {
                match item {
                    Value::String(s) => result.push(s.clone()),
                    _ => {
                        out.push(wrong_type(construct, "an array of strings"));
                        return Vec::new();
                    }
                }
            }
            result
        }
        Some(_) => {
            out.push(wrong_type(construct, "an array of strings"));
            Vec::new()
        }
    }
}

fn get_duration(
    table: &toml::Table,
    key: &str,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Option<std::time::Duration> {
    let text = get_string(table, key, construct, out)?;
    match parse_duration(&text) {
        Ok(d) => Some(d),
        Err(message) => {
            out.push(Violation { construct: construct.to_owned(), message });
            None
        }
    }
}

fn get_max_attempts(table: &toml::Table, construct: &str, out: &mut Vec<Violation>) -> u32 {
    match table.get("max_attempts") {
        Some(Value::Integer(n)) if *n >= 1 => u32::try_from(*n).unwrap_or_else(|_| {
            out.push(wrong_type(construct, "an integer >= 1"));
            1
        }),
        Some(_) => {
            out.push(wrong_type(construct, "an integer >= 1"));
            1
        }
        None => {
            out.push(Violation {
                construct: construct.to_owned(),
                message: "`max_attempts` is required and must be >= 1".to_owned(),
            });
            1
        }
    }
}

/// Walk raw TOML text against camp's acceptance/rejection tables, collecting
/// every violation. Returns whatever structure could be extracted so Task
/// 4's semantic checks can still run and report *their* violations too.
pub(crate) fn walk(text: &str) -> (RawFormula, Vec<Violation>) {
    let mut out = Vec::new();
    let empty = RawFormula {
        name: None,
        description: None,
        formula_compiler: None,
        steps: Vec::new(),
    };
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            out.push(Violation { construct: "toml".to_owned(), message: e.to_string() });
            return (empty, out);
        }
    };

    for key in sorted_keys(&table) {
        if ACCEPTED_TOP.contains(&key) {
            continue;
        } else if CITY_ONLY_TOP.contains(&key) {
            out.push(city_only(key));
        } else {
            out.push(unknown(key));
        }
    }

    let name = get_string(&table, "formula", "formula", &mut out);
    let description = get_string(&table, "description", "description", &mut out);
    let formula_compiler = walk_requires(&table, &mut out);
    let steps = walk_steps(&table, &mut out);

    (RawFormula { name, description, formula_compiler, steps }, out)
}

fn walk_requires(table: &toml::Table, out: &mut Vec<Violation>) -> Option<String> {
    let requires = match table.get("requires") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type("requires", "a table"));
            return None;
        }
    };
    for key in sorted_keys(requires) {
        if key != "formula_compiler" {
            // Mirrors gc's one hard-key exception: unknown [requires] axes
            // fail even in gc (formula.requirement_unknown).
            out.push(Violation {
                construct: format!("requires.{key}"),
                message: format!(
                    "unknown formula requirement `{key}`; supported requirements: formula_compiler"
                ),
            });
        }
    }
    get_string(requires, "formula_compiler", "requires.formula_compiler", out)
}

fn walk_steps(table: &toml::Table, out: &mut Vec<Violation>) -> Vec<RawStep> {
    let raw_steps = match table.get("steps") {
        None => return Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => {
            out.push(wrong_type("steps", "an array of tables"));
            return Vec::new();
        }
    };
    let mut steps = Vec::new();
    for (index, item) in raw_steps.iter().enumerate() {
        let Value::Table(step) = item else {
            out.push(wrong_type(&format!("steps[{index}]"), "a table"));
            continue;
        };
        steps.push(walk_step(index, step, out));
    }
    steps
}

fn walk_step(index: usize, step: &toml::Table, out: &mut Vec<Violation>) -> RawStep {
    let id = match step.get("id") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => {
            out.push(wrong_type(&format!("steps[{index}].id"), "a string"));
            None
        }
        None => None,
    };
    // Location prefix: the id when we have one, else the index.
    let at = |field: &str| match &id {
        Some(id) => format!("steps.{id}.{field}"),
        None => format!("steps[{index}].{field}"),
    };

    for key in sorted_keys(step) {
        if ACCEPTED_STEP.contains(&key) {
            continue;
        } else if CITY_ONLY_STEP.contains(&key) {
            out.push(city_only(key));
        } else {
            out.push(unknown(key));
        }
    }

    let title = get_string(step, "title", &at("title"), out);
    let description = get_string(step, "description", &at("description"), out);
    let needs = get_string_array(step, "needs", &at("needs"), out);
    let assignee = get_string(step, "assignee", &at("assignee"), out);
    let timeout = get_duration(step, "timeout", &at("timeout"), out);
    let check = walk_check(step, &at("check"), out);
    let retry = walk_retry(step, &at("retry"), out);
    let on_complete = walk_on_complete(step, &at("on_complete"), out);

    RawStep {
        index, id, title, description, needs, assignee, timeout, check, retry, on_complete,
    }
}

fn walk_check(step: &toml::Table, construct: &str, out: &mut Vec<Violation>) -> Option<Check> {
    let check = match step.get("check") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    for key in sorted_keys(check) {
        if !["check", "max_attempts"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let max_attempts = get_max_attempts(check, &format!("{construct}.max_attempts"), out);
    let inner = match check.get("check") {
        Some(Value::Table(t)) => t,
        Some(_) | None => {
            out.push(Violation {
                construct: format!("{construct}.check"),
                message: "check requires an inner [steps.check.check] table with \
                          mode = \"exec\" and a path"
                    .to_owned(),
            });
            return None;
        }
    };
    for key in sorted_keys(inner) {
        if !["mode", "path", "timeout"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let mode_construct = format!("{construct}.check.mode");
    let mode = match get_string(inner, "mode", &mode_construct, out) {
        Some(m) if m == "exec" => CheckMode::Exec,
        Some(m) => {
            out.push(Violation {
                construct: mode_construct,
                message: format!("check mode {m:?} is not supported; only \"exec\" is (spec §8.2)"),
            });
            return None;
        }
        None => {
            out.push(Violation {
                construct: mode_construct,
                message: "check mode is required; only \"exec\" is supported".to_owned(),
            });
            return None;
        }
    };
    let path_construct = format!("{construct}.check.path");
    let path = match get_string(inner, "path", &path_construct, out) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        Some(_) | None => {
            out.push(Violation {
                construct: path_construct,
                message: "check path is required and must be non-empty".to_owned(),
            });
            return None;
        }
    };
    let timeout = get_duration(inner, "timeout", &format!("{construct}.check.timeout"), out);
    Some(Check { max_attempts, mode, path, timeout })
}

fn walk_retry(step: &toml::Table, construct: &str, out: &mut Vec<Violation>) -> Option<RawRetry> {
    let retry = match step.get("retry") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    for key in sorted_keys(retry) {
        if !["max_attempts", "on_exhausted"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let max_attempts = get_max_attempts(retry, &format!("{construct}.max_attempts"), out);
    let on_exhausted = get_string(retry, "on_exhausted", &format!("{construct}.on_exhausted"), out);
    Some(RawRetry { max_attempts, on_exhausted })
}

fn walk_on_complete(
    step: &toml::Table,
    construct: &str,
    out: &mut Vec<Violation>,
) -> Option<OnComplete> {
    let oc = match step.get("on_complete") {
        None => return None,
        Some(Value::Table(t)) => t,
        Some(_) => {
            out.push(wrong_type(construct, "a table"));
            return None;
        }
    };
    for key in sorted_keys(oc) {
        if !["bond", "for_each", "parallel", "sequential", "vars"].contains(&key) {
            out.push(unknown(key));
        }
    }
    let for_each = get_string(oc, "for_each", &format!("{construct}.for_each"), out);
    let bond = get_string(oc, "bond", &format!("{construct}.bond"), out);
    let parallel_key = match oc.get("parallel") {
        None => None,
        Some(Value::Boolean(b)) => Some(*b),
        Some(_) => {
            out.push(wrong_type(&format!("{construct}.parallel"), "a boolean"));
            None
        }
    };
    let sequential_key = match oc.get("sequential") {
        None => None,
        Some(Value::Boolean(b)) => Some(*b),
        Some(_) => {
            out.push(wrong_type(&format!("{construct}.sequential"), "a boolean"));
            None
        }
    };
    if parallel_key.is_some() && sequential_key.is_some() {
        out.push(Violation {
            construct: construct.to_owned(),
            message: "`parallel` and `sequential` are mutually exclusive (gc formula-spec-v2 §3.4)"
                .to_owned(),
        });
    }
    let parallel = match (parallel_key, sequential_key) {
        (Some(p), None) => p,
        (None, Some(s)) => !s,
        _ => true, // gc default: parallel
    };
    let mut vars = std::collections::BTreeMap::new();
    match oc.get("vars") {
        None => {}
        Some(Value::Table(t)) => {
            for (k, v) in t {
                match v {
                    Value::String(s) => {
                        vars.insert(k.clone(), s.clone());
                    }
                    _ => out.push(wrong_type(
                        &format!("{construct}.vars.{k}"),
                        "a string",
                    )),
                }
            }
        }
        Some(_) => out.push(wrong_type(&format!("{construct}.vars"), "a table of strings")),
    }
    // for_each and bond must be set together (gc formula-spec-v2 §3.4);
    // reported here because it is a shape rule, not a semantic one.
    match (&for_each, &bond) {
        (Some(f), Some(b)) => Some(OnComplete {
            for_each: f.clone(),
            bond: b.clone(),
            vars,
            parallel,
        }),
        (None, None) => {
            out.push(Violation {
                construct: construct.to_owned(),
                message: "`for_each` and `bond` are required and must be set together \
                          (gc formula-spec-v2 §3.4)"
                    .to_owned(),
            });
            None
        }
        _ => {
            out.push(Violation {
                construct: construct.to_owned(),
                message: "`for_each` and `bond` must be set together (gc formula-spec-v2 §3.4)"
                    .to_owned(),
            });
            None
        }
    }
}
```

Note the deliberate salvage behavior: on a wrong-typed or missing required field the walk records the violation and keeps going with `None`/defaults so later semantic checks still report everything else. Nothing is silenced — every salvage records its violation first.

- [ ] **Step 4: Run and watch them pass**

Run: `cargo test -p camp-core parse` — expected: all Task 2 + Task 3 tests pass. Then `cargo clippy -p camp-core --all-targets -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/formula/
git commit -m "feat: formula TOML walk with acceptance and city-pointer rejection tables"
```

### Task 4: Semantic validation and `parse_and_validate`

**Files:**
- Create: `crates/camp-core/src/formula/validate.rs`
- Modify: `crates/camp-core/src/formula/mod.rs` (add `mod validate;` and `parse_and_validate`)
- Test: inline in `validate.rs`

**Interfaces:**
- Consumes: `walk`/`RawFormula`/`RawStep`/`RawRetry` (Task 3), AST (Task 1).
- Produces:
  - `pub fn parse_and_validate(path: &Path) -> Result<Formula, FormulaError>` (in `mod.rs` — the contract entry point; reads the file, runs `walk`, runs `validate::check`, returns the `Formula` with `source` set, or ALL violations).
  - `pub(crate) fn check(raw: &RawFormula, stem: Option<&str>, out: &mut Vec<Violation>) -> ()` plus `pub(crate) fn assemble(raw: RawFormula, source: String) -> Formula` in `validate.rs`.
  - `pub const FORMULA_COMPILER_CAPABILITY: &str = "2.0.0";` in `validate.rs`, re-exported from `mod.rs` — camp's formula-compiler capability, mirroring gc's v2 host capability (gc formula-spec-v2 §5: capability is 1.0.0 only when v2 is disabled).

Semantic rules (each records a `Violation`; all of gc's, some stricter — strictness direction is always camp-tighter, invariant 6):

| # | Rule | gc parity |
|---|---|---|
| S1 | `formula` header required, non-empty | gc requires it |
| S2 | `formula` must equal the file stem (when a stem is known) | camp-stricter (fact 2) |
| S3 | at least one step | camp-stricter |
| S4 | step `id` required, non-empty, unique across the formula | gc |
| S5 | step `title` required, non-empty | gc |
| S6 | every `needs` entry names a known step id; no self-need; no duplicate entry | gc (unknown id) + camp-stricter (dup entry, which Phase 3's fold rejects anyway) |
| S7 | the `needs` graph is acyclic (report each cycle by its path) | gc |
| S8 | `timeout` requires `check` (fact 6) | gc |
| S9 | `check` ∦ `retry`; `check` ∦ `assignee`; `retry` ∦ `on_complete` | gc (fact 8/9) |
| S10 | `retry.on_exhausted` ∈ {`hard_fail`, `soft_fail`}; absent → `hard_fail` | gc |
| S11 | graph-only constructs (`check`, `retry`, `on_complete` — and `timeout`, which requires `check` anyway) require `[requires] formula_compiler` (fact 5) | gc |
| S12 | `requires.formula_compiler` must parse as a semver comparator and be satisfied by `FORMULA_COMPILER_CAPABILITY` | gc (fact 7) |
| S13 | `on_complete.for_each` must start with `"output."` | gc (fact 10) |

- [ ] **Step 1: Write the failing tests** — `crates/camp-core/src/formula/validate.rs` starts as:

```rust
//! Semantic validation for the camp formula subset (rules S1–S13 in the
//! Phase 5 plan). Pure functions over the raw walk output; every rule
//! records a Violation — the caller reports all of them at once.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use crate::formula::ast::Violation;
    use crate::formula::parse::walk;

    fn violations_for(text: &str, stem: &str) -> Vec<Violation> {
        let (raw, mut v) = walk(text);
        super::check(&raw, Some(stem), &mut v);
        v
    }

    fn has(v: &[Violation], construct: &str, needle: &str) -> bool {
        v.iter().any(|v| v.construct == construct && v.message.contains(needle))
    }

    const HEADER: &str = "formula = \"f\"\n";

    #[test]
    fn name_must_match_the_file_stem() {
        let v = violations_for("formula = \"other\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n", "f");
        assert!(has(&v, "formula", "file stem"), "{v:?}");
        assert!(violations_for(&format!("{HEADER}[[steps]]\nid = \"a\"\ntitle = \"t\"\n"), "f").is_empty());
    }

    #[test]
    fn missing_name_missing_steps_missing_title_all_reported_together() {
        let v = violations_for("[[steps]]\nid = \"a\"\n", "f");
        assert!(has(&v, "formula", "required"), "{v:?}");
        assert!(has(&v, "steps.a.title", "required"), "{v:?}");
        let v = violations_for("formula = \"f\"\n", "f");
        assert!(has(&v, "steps", "at least one step"), "{v:?}");
    }

    #[test]
    fn duplicate_ids_unknown_needs_self_needs_and_dup_needs_are_reported() {
        let text = format!(
            "{HEADER}\
             [[steps]]\nid = \"a\"\ntitle = \"t\"\nneeds = [\"a\", \"ghost\"]\n\
             [[steps]]\nid = \"a\"\ntitle = \"t\"\n\
             [[steps]]\nid = \"b\"\ntitle = \"t\"\nneeds = [\"a\", \"a\"]\n"
        );
        let v = violations_for(&text, "f");
        assert!(has(&v, "steps.a.id", "duplicate"), "{v:?}");
        assert!(has(&v, "steps.a.needs", "ghost"), "{v:?}");
        assert!(has(&v, "steps.a.needs", "itself"), "{v:?}");
        assert!(has(&v, "steps.b.needs", "duplicate"), "{v:?}");
    }

    #[test]
    fn cycles_are_reported_with_their_path() {
        let text = format!(
            "{HEADER}\
             [[steps]]\nid = \"a\"\ntitle = \"t\"\nneeds = [\"c\"]\n\
             [[steps]]\nid = \"b\"\ntitle = \"t\"\nneeds = [\"a\"]\n\
             [[steps]]\nid = \"c\"\ntitle = \"t\"\nneeds = [\"b\"]\n"
        );
        let v = violations_for(&text, "f");
        assert!(has(&v, "steps", "cycle"), "{v:?}");
        assert!(v.iter().any(|v| v.message.contains("a") && v.message.contains("b") && v.message.contains("c")), "{v:?}");
    }

    #[test]
    fn combination_rules_mirror_gc() {
        let check = "[steps.check]\nmax_attempts = 1\n[steps.check.check]\nmode = \"exec\"\npath = \"v.sh\"\n";
        let requires = "[requires]\nformula_compiler = \">=2.0.0\"\n";
        // check + retry
        let v = violations_for(
            &format!("{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n{check}[steps.retry]\nmax_attempts = 2\n"),
            "f",
        );
        assert!(has(&v, "steps.a.check", "retry"), "{v:?}");
        // check + assignee
        let v = violations_for(
            &format!("{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\nassignee = \"dev\"\n{check}"),
            "f",
        );
        assert!(has(&v, "steps.a.check", "assignee"), "{v:?}");
        // retry + on_complete
        let v = violations_for(
            &format!(
                "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.retry]\nmax_attempts = 2\n\
                 [steps.on_complete]\nfor_each = \"output.i\"\nbond = \"b\"\n"
            ),
            "f",
        );
        assert!(has(&v, "steps.a.retry", "on_complete"), "{v:?}");
    }

    #[test]
    fn timeout_requires_check() {
        let v = violations_for(
            &format!("{HEADER}[requires]\nformula_compiler = \">=2.0.0\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntimeout = \"5m\"\n"),
            "f",
        );
        assert!(has(&v, "steps.a.timeout", "requires `check`"), "{v:?}");
    }

    #[test]
    fn graph_only_constructs_require_the_explicit_declaration() {
        let check = "[steps.check]\nmax_attempts = 1\n[steps.check.check]\nmode = \"exec\"\npath = \"v.sh\"\n";
        let v = violations_for(&format!("{HEADER}[[steps]]\nid = \"a\"\ntitle = \"t\"\n{check}"), "f");
        assert!(
            has(&v, "requires", "graph-only constructs must declare [requires] formula_compiler"),
            "{v:?}"
        );
        // with the declaration the same formula is clean
        let v = violations_for(
            &format!("{HEADER}[requires]\nformula_compiler = \">=2.0.0\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n{check}"),
            "f",
        );
        assert!(v.is_empty(), "{v:?}");
    }

    #[test]
    fn semver_comparator_is_validated_and_checked_against_capability() {
        let v = violations_for(
            &format!("{HEADER}[requires]\nformula_compiler = \"not-a-version\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n"),
            "f",
        );
        assert!(has(&v, "requires.formula_compiler", "semver comparator"), "{v:?}");
        let v = violations_for(
            &format!("{HEADER}[requires]\nformula_compiler = \">=3.0.0\"\n[[steps]]\nid = \"a\"\ntitle = \"t\"\n"),
            "f",
        );
        assert!(has(&v, "requires.formula_compiler", "capability"), "{v:?}");
    }

    #[test]
    fn retry_defaults_and_on_complete_rules() {
        let requires = "[requires]\nformula_compiler = \">=2.0.0\"\n";
        // default on_exhausted = hard_fail
        let (raw, mut v) = walk(&format!(
            "{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.retry]\nmax_attempts = 2\n"
        ));
        super::check(&raw, Some("f"), &mut v);
        assert!(v.is_empty(), "{v:?}");
        let formula = super::assemble(raw, String::new());
        assert_eq!(
            formula.steps[0].retry.as_ref().unwrap().on_exhausted,
            crate::formula::ast::Disposition::HardFail
        );
        // bad on_exhausted value
        let v = violations_for(
            &format!("{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.retry]\nmax_attempts = 2\non_exhausted = \"explode\"\n"),
            "f",
        );
        assert!(has(&v, "steps.a.retry.on_exhausted", "hard_fail"), "{v:?}");
        // for_each must start with output.
        let v = violations_for(
            &format!("{HEADER}{requires}[[steps]]\nid = \"a\"\ntitle = \"t\"\n[steps.on_complete]\nfor_each = \"items\"\nbond = \"b\"\n"),
            "f",
        );
        assert!(has(&v, "steps.a.on_complete.for_each", "output."), "{v:?}");
    }
}
```

- [ ] **Step 2: Run and watch them fail**

Add `mod validate;` to `formula/mod.rs`. Run: `cargo test -p camp-core validate` — expected: compile error (`check`/`assemble` not found).

- [ ] **Step 3: Implement** — above the tests in `validate.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use crate::formula::ast::{Disposition, Formula, Retry, Step, Violation};
use crate::formula::parse::{RawFormula, RawStep};

/// Camp's formula-compiler capability. Mirrors gc's v2 host capability
/// (gc formula-spec-v2 §5); `[requires] formula_compiler` comparators are
/// checked against this version.
pub const FORMULA_COMPILER_CAPABILITY: &str = "2.0.0";

fn violation(out: &mut Vec<Violation>, construct: impl Into<String>, message: impl Into<String>) {
    out.push(Violation { construct: construct.into(), message: message.into() });
}

/// Location prefix for a step: its id, else its index.
fn step_loc(step: &RawStep) -> String {
    match &step.id {
        Some(id) => format!("steps.{id}"),
        None => format!("steps[{}]", step.index),
    }
}

/// Run rules S1–S13. Appends to `out`; the caller already holds the walk's
/// shape violations.
pub(crate) fn check(raw: &RawFormula, stem: Option<&str>, out: &mut Vec<Violation>) {
    // S1/S2 — header name.
    match raw.name.as_deref() {
        None | Some("") => violation(out, "formula", "the `formula` name is required"),
        Some(name) => {
            if let Some(stem) = stem
                && name != stem
            {
                violation(
                    out,
                    "formula",
                    format!(
                        "formula name {name:?} must equal the file stem {stem:?} \
                         (camp enforces gc's name-is-the-lookup-key convention)"
                    ),
                );
            }
        }
    }

    // S3 — at least one step.
    if raw.steps.is_empty() {
        violation(out, "steps", "a camp formula must declare at least one step");
    }

    // S4 — ids: required, non-empty, unique.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for step in &raw.steps {
        match step.id.as_deref() {
            None | Some("") => {
                violation(out, format!("{}.id", step_loc(step)), "step `id` is required")
            }
            Some(id) => {
                if !seen.insert(id) {
                    violation(
                        out,
                        format!("steps.{id}.id"),
                        format!("duplicate step id {id:?}"),
                    );
                }
            }
        }
    }
    let known: BTreeSet<&str> = raw.steps.iter().filter_map(|s| s.id.as_deref()).collect();

    for step in &raw.steps {
        let loc = step_loc(step);

        // S5 — title.
        if step.title.as_deref().is_none_or(str::is_empty) {
            violation(out, format!("{loc}.title"), "step `title` is required");
        }

        // S6 — needs reference known, non-self, non-duplicate ids.
        let mut seen_needs: BTreeSet<&str> = BTreeSet::new();
        for need in &step.needs {
            if Some(need.as_str()) == step.id.as_deref() {
                violation(out, format!("{loc}.needs"), format!("step {need:?} needs itself"));
            } else if !known.contains(need.as_str()) {
                violation(
                    out,
                    format!("{loc}.needs"),
                    format!("needs unknown step id {need:?}"),
                );
            }
            if !seen_needs.insert(need) {
                violation(out, format!("{loc}.needs"), format!("duplicate needs entry {need:?}"));
            }
        }

        // S8 — timeout requires check (gc formula-spec-v2 §1.3).
        if step.timeout.is_some() && step.check.is_none() {
            violation(
                out,
                format!("{loc}.timeout"),
                "step `timeout` bounds the check script and requires `check` \
                 (gc formula-spec-v2 §1.3)",
            );
        }

        // S9 — combination rules (gc formula-spec-v2 §3.1/§3.2).
        if step.check.is_some() && step.retry.is_some() {
            violation(
                out,
                format!("{loc}.check"),
                "`check` must not be combined with `retry` (gc formula-spec-v2 §3.1)",
            );
        }
        if step.check.is_some() && step.assignee.is_some() {
            violation(
                out,
                format!("{loc}.check"),
                "`check` must not be combined with `assignee` (gc formula-spec-v2 §3.1)",
            );
        }
        if step.retry.is_some() && step.on_complete.is_some() {
            violation(
                out,
                format!("{loc}.retry"),
                "`retry` must not be combined with `on_complete` (gc formula-spec-v2 §3.2)",
            );
        }

        // S10 — retry.on_exhausted vocabulary.
        if let Some(retry) = &step.retry
            && let Some(value) = retry.on_exhausted.as_deref()
            && !crate::vocab::CAMP_FINAL_DISPOSITIONS.contains(&value)
        {
            violation(
                out,
                format!("{loc}.retry.on_exhausted"),
                format!("on_exhausted {value:?} is not legal; use \"hard_fail\" or \"soft_fail\""),
            );
        }

        // S13 — for_each path shape.
        if let Some(oc) = &step.on_complete
            && !oc.for_each.starts_with("output.")
        {
            violation(
                out,
                format!("{loc}.on_complete.for_each"),
                format!("for_each {:?} must start with \"output.\"", oc.for_each),
            );
        }
    }

    // S7 — acyclic needs graph (DFS over known ids; unknown ids were S6).
    check_cycles(raw, out);

    // S11 — the explicit-declaration rule (gc compile.go:51 concept).
    let uses_graph_only = raw
        .steps
        .iter()
        .any(|s| s.check.is_some() || s.retry.is_some() || s.on_complete.is_some());
    if uses_graph_only && raw.formula_compiler.is_none() {
        violation(
            out,
            "requires",
            "formulas that use graph-only constructs must declare \
             [requires] formula_compiler = \">=2.0.0\" (gc formula-spec-v2 §5)",
        );
    }

    // S12 — the comparator itself.
    if let Some(req) = raw.formula_compiler.as_deref() {
        match semver::VersionReq::parse(req) {
            Err(e) => violation(
                out,
                "requires.formula_compiler",
                format!("formula_compiler must be a semver comparator, for example \">=2.0.0\": {e}"),
            ),
            Ok(parsed) => {
                // Infallible: FORMULA_COMPILER_CAPABILITY is a const literal,
                // and a broken constant must fail loudly, not silently pass.
                match semver::Version::parse(FORMULA_COMPILER_CAPABILITY) {
                    Err(e) => violation(
                        out,
                        "requires.formula_compiler",
                        format!("internal: capability constant unparseable: {e}"),
                    ),
                    Ok(capability) => {
                        if !parsed.matches(&capability) {
                            violation(
                                out,
                                "requires.formula_compiler",
                                format!(
                                    "formula requires formula_compiler {req:?}, but camp's \
                                     capability is {FORMULA_COMPILER_CAPABILITY}"
                                ),
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Iterative DFS cycle detection; reports one violation per cycle found,
/// with the cycle's path in the message.
fn check_cycles(raw: &RawFormula, out: &mut Vec<Violation>) {
    let edges: BTreeMap<&str, Vec<&str>> = raw
        .steps
        .iter()
        .filter_map(|s| s.id.as_deref().map(|id| (id, s.needs.iter().map(String::as_str).collect())))
        .collect();
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Unvisited,
        InStack,
        Done,
    }
    let mut state: BTreeMap<&str, State> =
        edges.keys().map(|&k| (k, State::Unvisited)).collect();
    let mut reported: BTreeSet<String> = BTreeSet::new();

    fn dfs<'a>(
        node: &'a str,
        edges: &BTreeMap<&'a str, Vec<&'a str>>,
        state: &mut BTreeMap<&'a str, State>,
        stack: &mut Vec<&'a str>,
        out: &mut Vec<Violation>,
        reported: &mut BTreeSet<String>,
    ) {
        state.insert(node, State::InStack);
        stack.push(node);
        for &next in edges.get(node).map(Vec::as_slice).unwrap_or(&[]) {
            match state.get(next) {
                Some(State::InStack) => {
                    let start = stack.iter().position(|&n| n == next).unwrap_or(0);
                    let mut cycle: Vec<&str> = stack[start..].to_vec();
                    cycle.push(next);
                    // Canonical form so the same cycle is reported once.
                    let mut canonical = cycle.clone();
                    canonical.pop();
                    canonical.sort_unstable();
                    if reported.insert(canonical.join(",")) {
                        out.push(Violation {
                            construct: "steps".to_owned(),
                            message: format!(
                                "dependency cycle: {}",
                                cycle.join(" -> ")
                            ),
                        });
                    }
                }
                Some(State::Unvisited) => {
                    dfs(next, edges, state, stack, out, reported);
                }
                _ => {}
            }
        }
        stack.pop();
        state.insert(node, State::Done);
    }

    let nodes: Vec<&str> = edges.keys().copied().collect();
    for node in nodes {
        if state.get(node) == Some(&State::Unvisited) {
            let mut stack = Vec::new();
            dfs(node, &edges, &mut state, &mut stack, out, &mut reported);
        }
    }
}

/// Convert a violation-free RawFormula into the public Formula. Only call
/// after `check` reported no violations (parse_and_validate enforces this).
pub(crate) fn assemble(raw: RawFormula, source: String) -> Formula {
    Formula {
        name: raw.name.unwrap_or_default(),
        description: raw.description,
        requires: raw
            .formula_compiler
            .map(|formula_compiler| crate::formula::ast::Requires { formula_compiler }),
        steps: raw
            .steps
            .into_iter()
            .map(|s| Step {
                id: s.id.unwrap_or_default(),
                title: s.title.unwrap_or_default(),
                description: s.description,
                needs: s.needs,
                assignee: s.assignee,
                timeout: s.timeout,
                check: s.check,
                retry: s.retry.map(|r| Retry {
                    max_attempts: r.max_attempts,
                    on_exhausted: match r.on_exhausted.as_deref() {
                        Some("soft_fail") => Disposition::SoftFail,
                        _ => Disposition::HardFail, // gc default
                    },
                }),
                on_complete: s.on_complete,
            })
            .collect(),
        source,
    }
}
```

And in `formula/mod.rs`, the contract entry point:

```rust
use std::path::Path;

pub use validate::FORMULA_COMPILER_CAPABILITY;

/// Parse and validate one formula file against the camp subset (spec §8.2).
/// On failure the error lists ALL violations, not just the first. The file
/// stem is the enforced formula name.
pub fn parse_and_validate(path: &Path) -> Result<Formula, FormulaError> {
    let source = std::fs::read_to_string(path).map_err(|e| FormulaError {
        path: path.to_path_buf(),
        violations: vec![Violation {
            construct: "file".to_owned(),
            message: format!("cannot read: {e}"),
        }],
    })?;
    let stem = path.file_stem().and_then(|s| s.to_str());
    let (raw, mut violations) = parse::walk(&source);
    validate::check(&raw, stem, &mut violations);
    if violations.is_empty() {
        Ok(validate::assemble(raw, source))
    } else {
        Err(FormulaError { path: path.to_path_buf(), violations })
    }
}
```

- [ ] **Step 4: Run and watch them pass**

Run: `cargo test -p camp-core formula` — expected: all pass. `cargo clippy -p camp-core --all-targets -- -D warnings` (note: the tests-only `unwrap_or(0)` in `dfs` is in non-test code — it is a positional fallback that cannot trigger because `next` is guaranteed on the stack when state is `InStack`; if clippy or review flags it, replace with an explicit `match` that pushes a `Violation` naming the internal invariant. Never `unwrap`).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/formula/
git commit -m "feat: semantic validation with all-violations reporting and parse_and_validate"
```

### Task 5: The fixture corpus and the table-driven corpus test

**Files:**
- Create: `crates/camp-core/tests/fixtures/formulas/valid/*.toml` (5 files)
- Create: `crates/camp-core/tests/fixtures/formulas/invalid/*.toml` (25 files)
- Test: `crates/camp-core/tests/formula_corpus.rs`

**Interfaces:**
- Consumes: `camp_core::formula::parse_and_validate` (Task 4).
- Produces: the corpus directory Phase 6's gc-compat CI job validates (`valid/` must compile under the real gc compiler at `ci/gc-compat/GASCITY_REF`). File stems ARE formula names — never rename one without renaming the header.

**valid/ rules recap for whoever edits fixtures later:** stem = `formula` value; graph-only constructs (`check`/`retry`/`on_complete`, and `timeout` which requires `check`) demand `[requires] formula_compiler = ">=2.0.0"`; a plain `needs` dag does not. gc will silently ignore nothing here because camp validated first; gc will run dup-id/unknown-needs/cycle/combination checks (ground-truth fact 3).

- [ ] **Step 1: Create the corpus** — run this script from the repo root (heredocs keep the plan executable; every file's exact content is below):

```bash
mkdir -p crates/camp-core/tests/fixtures/formulas/{valid,invalid}
cd crates/camp-core/tests/fixtures/formulas

# ============ valid/ ============

# Spec §8.2 example, verbatim (do not edit — tests compare against the spec).
cat > valid/guarded-change.toml <<'EOF'
formula = "guarded-change"
description = "Implement with script verification and bounded retries"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "implement"
title = "Implement the change"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "scripts/verify.sh"
timeout = "5m"

[[steps]]
id = "review"
title = "Review the final diff"
needs = ["implement"]
EOF

cat > valid/minimal.toml <<'EOF'
formula = "minimal"

[[steps]]
id = "only"
title = "Do the one thing"
EOF

cat > valid/retry-fetch.toml <<'EOF'
formula = "retry-fetch"
description = "Bounded transient retries with a soft-fail disposition"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "fetch"
title = "Fetch the dataset"

[steps.retry]
max_attempts = 3
on_exhausted = "soft_fail"
EOF

# A diamond dag with assignees and NO [requires]: needs alone is not
# graph-only (gc formula-spec-v2 §5), and camp must mirror that.
cat > valid/diamond.toml <<'EOF'
formula = "diamond"
description = "Fan out and join"

[[steps]]
id = "design"
title = "Design the interface"
assignee = "architect"

[[steps]]
id = "implement"
title = "Implement the interface"
needs = ["design"]
assignee = "dev"

[[steps]]
id = "document"
title = "Document the interface"
needs = ["design"]
assignee = "writer"

[[steps]]
id = "release"
title = "Release it"
needs = ["implement", "document"]
assignee = "dev"
EOF

# The first on_complete fixture anywhere (gc ships none — Phase 6 proves it
# against the real compiler). bond = "minimal" so the bonded formula exists
# in this same directory.
cat > valid/fan-out.toml <<'EOF'
formula = "fan-out"
description = "Runtime fan-out over structured step output"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "enumerate"
title = "Enumerate the work items"

[steps.on_complete]
for_each = "output.items"
bond = "minimal"
sequential = true

[steps.on_complete.vars]
name = "{item.name}"
position = "{index}"
EOF

# ============ invalid/ — one file per rejection-table row ============
# (rejection is by key name; header/step scaffolding keeps each file
# focused on exactly one construct)

for key in extends vars phase pour contract catalog template compose advice pointcuts; do
cat > "invalid/${key//_/-}.toml" <<EOF
formula = "${key//_/-}"
${key} = true

[[steps]]
id = "a"
title = "t"
EOF
done
# top-level `type` needs its own body (name clash with the step-level file)
cat > invalid/type-top-level.toml <<'EOF'
formula = "type-top-level"
type = "expansion"

[[steps]]
id = "a"
title = "t"
EOF

for key in drain gate loop expand expand_vars children waits_for condition tally metadata depends_on priority tags description_file notes; do
cat > "invalid/${key//_/-}.toml" <<EOF
formula = "${key//_/-}"

[[steps]]
id = "a"
title = "t"
${key} = true
EOF
done
cat > invalid/type-step-level.toml <<'EOF'
formula = "type-step-level"

[[steps]]
id = "a"
title = "t"
type = "bug"
EOF

cat > invalid/unknown-key.toml <<'EOF'
formula = "unknown-key"
bogus = 1

[[steps]]
id = "a"
title = "t"
dependson = ["b"]
EOF

# ============ invalid/ — semantic rows ============

cat > invalid/dup-step-id.toml <<'EOF'
formula = "dup-step-id"

[[steps]]
id = "a"
title = "t"

[[steps]]
id = "a"
title = "t"
EOF

cat > invalid/unknown-needs-id.toml <<'EOF'
formula = "unknown-needs-id"

[[steps]]
id = "a"
title = "t"
needs = ["ghost"]
EOF

cat > invalid/cycle.toml <<'EOF'
formula = "cycle"

[[steps]]
id = "a"
title = "t"
needs = ["c"]

[[steps]]
id = "b"
title = "t"
needs = ["a"]

[[steps]]
id = "c"
title = "t"
needs = ["b"]
EOF

cat > invalid/bad-semver.toml <<'EOF'
formula = "bad-semver"

[requires]
formula_compiler = "not-a-version"

[[steps]]
id = "a"
title = "t"
EOF

cat > invalid/check-without-requires.toml <<'EOF'
formula = "check-without-requires"

[[steps]]
id = "a"
title = "t"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "scripts/verify.sh"
EOF

cat > invalid/check-with-retry.toml <<'EOF'
formula = "check-with-retry"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "v.sh"

[steps.retry]
max_attempts = 2
EOF

cat > invalid/check-with-assignee.toml <<'EOF'
formula = "check-with-assignee"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"
assignee = "dev"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "exec"
path = "v.sh"
EOF

cat > invalid/retry-with-on-complete.toml <<'EOF'
formula = "retry-with-on-complete"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.retry]
max_attempts = 2

[steps.on_complete]
for_each = "output.items"
bond = "minimal"
EOF

cat > invalid/for-each-not-output.toml <<'EOF'
formula = "for-each-not-output"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.on_complete]
for_each = "items"
bond = "minimal"
EOF

cat > invalid/on-complete-missing-bond.toml <<'EOF'
formula = "on-complete-missing-bond"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.on_complete]
for_each = "output.items"
EOF

cat > invalid/parallel-and-sequential.toml <<'EOF'
formula = "parallel-and-sequential"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.on_complete]
for_each = "output.items"
bond = "minimal"
parallel = true
sequential = true
EOF

cat > invalid/timeout-without-check.toml <<'EOF'
formula = "timeout-without-check"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"
timeout = "5m"
EOF

cat > invalid/check-mode-not-exec.toml <<'EOF'
formula = "check-mode-not-exec"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.check]
max_attempts = 3

[steps.check.check]
mode = "inference"
path = "v.sh"
EOF

cat > invalid/check-zero-attempts.toml <<'EOF'
formula = "check-zero-attempts"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.check]
max_attempts = 0

[steps.check.check]
mode = "exec"
path = "v.sh"
EOF

cat > invalid/retry-zero-attempts.toml <<'EOF'
formula = "retry-zero-attempts"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.retry]
max_attempts = 0
EOF

cat > invalid/bad-on-exhausted.toml <<'EOF'
formula = "bad-on-exhausted"

[requires]
formula_compiler = ">=2.0.0"

[[steps]]
id = "a"
title = "t"

[steps.retry]
max_attempts = 2
on_exhausted = "explode"
EOF

cat > invalid/name-stem-mismatch.toml <<'EOF'
formula = "some-other-name"

[[steps]]
id = "a"
title = "t"
EOF

cat > invalid/missing-title.toml <<'EOF'
formula = "missing-title"

[[steps]]
id = "a"
EOF

cat > invalid/unsatisfied-requirement.toml <<'EOF'
formula = "unsatisfied-requirement"

[requires]
formula_compiler = ">=3.0.0"

[[steps]]
id = "a"
title = "t"
EOF

# Several violations at once — the all-violations contract, in file form.
cat > invalid/multi-violation.toml <<'EOF'
formula = "wrong-name"
pour = true

[[steps]]
id = "a"
title = "t"
tags = ["x"]
needs = ["ghost"]
timeout = "5m"
EOF
```

- [ ] **Step 2: Write the failing test** — `crates/camp-core/tests/formula_corpus.rs`:

```rust
//! Table-driven acceptance/rejection over the fixture corpus (master-plan
//! Phase 5). Every valid fixture must parse clean; every invalid fixture
//! must fail with a violation naming the expected construct; and the table
//! must cover exactly the files on disk so a fixture can never silently
//! drop out of coverage. Phase 6 revalidates valid/ with the real gc
//! compiler.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::PathBuf;

use camp_core::formula::parse_and_validate;

fn corpus(kind: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formulas")
        .join(kind)
}

fn toml_files(kind: &str) -> BTreeSet<String> {
    std::fs::read_dir(corpus(kind))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_owned())
        .collect()
}

#[test]
fn every_valid_fixture_is_accepted() {
    let files = toml_files("valid");
    assert_eq!(
        files,
        ["diamond", "fan-out", "guarded-change", "minimal", "retry-fetch"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "valid corpus drifted from the plan"
    );
    for stem in files {
        let path = corpus("valid").join(format!("{stem}.toml"));
        match parse_and_validate(&path) {
            Ok(f) => assert_eq!(f.name, stem),
            Err(e) => panic!("{stem} must be valid:\n{e}"),
        }
    }
}

/// filename stem -> the construct a violation must name.
const REJECTIONS: &[(&str, &str)] = &[
    // city-only, top level
    ("extends", "extends"),
    ("vars", "vars"),
    ("type-top-level", "type"),
    ("phase", "phase"),
    ("pour", "pour"),
    ("contract", "contract"),
    ("catalog", "catalog"),
    ("template", "template"),
    ("compose", "compose"),
    ("advice", "advice"),
    ("pointcuts", "pointcuts"),
    // city-only, step level
    ("drain", "drain"),
    ("gate", "gate"),
    ("loop", "loop"),
    ("expand", "expand"),
    ("expand-vars", "expand_vars"),
    ("children", "children"),
    ("waits-for", "waits_for"),
    ("condition", "condition"),
    ("tally", "tally"),
    ("metadata", "metadata"),
    ("depends-on", "depends_on"),
    ("type-step-level", "type"),
    ("priority", "priority"),
    ("tags", "tags"),
    ("description-file", "description_file"),
    ("notes", "notes"),
    // stricter-than-gc
    ("unknown-key", "dependson"),
    // semantic
    ("dup-step-id", "steps.a.id"),
    ("unknown-needs-id", "steps.a.needs"),
    ("cycle", "steps"),
    ("bad-semver", "requires.formula_compiler"),
    ("unsatisfied-requirement", "requires.formula_compiler"),
    ("check-without-requires", "requires"),
    ("check-with-retry", "steps.a.check"),
    ("check-with-assignee", "steps.a.check"),
    ("retry-with-on-complete", "steps.a.retry"),
    ("for-each-not-output", "steps.a.on_complete.for_each"),
    ("on-complete-missing-bond", "steps.a.on_complete"),
    ("parallel-and-sequential", "steps.a.on_complete"),
    ("timeout-without-check", "steps.a.timeout"),
    ("check-mode-not-exec", "steps.a.check.check.mode"),
    ("check-zero-attempts", "steps.a.check.max_attempts"),
    ("retry-zero-attempts", "steps.a.retry.max_attempts"),
    ("bad-on-exhausted", "steps.a.retry.on_exhausted"),
    ("name-stem-mismatch", "formula"),
    ("missing-title", "steps.a.title"),
    ("multi-violation", "pour"),
];

#[test]
fn every_invalid_fixture_is_rejected_naming_the_construct() {
    let on_disk = toml_files("invalid");
    let in_table: BTreeSet<String> =
        REJECTIONS.iter().map(|(f, _)| (*f).to_owned()).collect();
    assert_eq!(on_disk, in_table, "invalid corpus and rejection table must match");

    for (stem, construct) in REJECTIONS {
        let path = corpus("invalid").join(format!("{stem}.toml"));
        let err = parse_and_validate(&path)
            .expect_err(&format!("{stem} must be rejected"));
        assert!(
            err.violations.iter().any(|v| v.construct == *construct),
            "{stem}: no violation names {construct:?} — got:\n{err}"
        );
    }
}

#[test]
fn multi_violation_fixture_reports_every_problem_at_once() {
    let err = parse_and_validate(&corpus("invalid").join("multi-violation.toml"))
        .expect_err("multi-violation must be rejected");
    for construct in ["pour", "tags", "formula", "steps.a.needs", "steps.a.timeout"] {
        assert!(
            err.violations.iter().any(|v| v.construct == construct),
            "missing {construct:?} in:\n{err}"
        );
    }
    assert!(err.violations.len() >= 5, "{err}");
}

#[test]
fn guarded_change_fixture_is_the_spec_example_verbatim() {
    let text = std::fs::read_to_string(corpus("valid").join("guarded-change.toml")).unwrap();
    // Anchor lines from spec §8.2 — if the spec changes, this fixture must
    // change with it (spec and code never silently diverge).
    for anchor in [
        "formula = \"guarded-change\"",
        "formula_compiler = \">=2.0.0\"",
        "max_attempts = 3",
        "path = \"scripts/verify.sh\"",
        "timeout = \"5m\"",
        "needs = [\"implement\"]",
    ] {
        assert!(text.contains(anchor), "spec anchor missing: {anchor}");
    }
}
```

- [ ] **Step 3: Run — corpus test red or green reveals validator gaps**

Run: `cargo test -p camp-core --test formula_corpus` — expected on first run: failures are REAL findings (either a fixture is wrong or Tasks 3–4 have a bug). Fix the code, not the assertion, unless the fixture itself contradicts the gc ground truth quoted in this plan. Iterate to green.

- [ ] **Step 4: Commit**

```bash
git add crates/camp-core/tests/
git commit -m "test: formula fixture corpus with table-driven acceptance/rejection"
```

### Task 6: Ledger vocabulary — `run.cooked` and run-aware `bead.created`

Shared-file task (event.rs, vocab.rs, fold.rs): keep every edit additive and minimal — siblings phase-4 (merged) and phase-7 (in flight) touch neighboring lines.

**Files:**
- Modify: `crates/camp-core/src/event.rs` (one enum variant + registry entries)
- Modify: `crates/camp-core/src/vocab.rs` (one list entry)
- Modify: `crates/camp-core/src/ledger/fold.rs` (two optional payload fields + one match arm + one payload struct)
- Test: extend inline tests in `fold.rs`-adjacent `ledger/mod.rs` tests? No — put new tests in `crates/camp-core/tests/cook.rs` (created here, extended in Task 7) to keep the shared files' test modules untouched.

**Interfaces:**
- Consumes: existing `EventType`, fold plumbing, `vocab_pin.rs` tests (they enforce the partition automatically — `run.cooked` is absent from `gc-vocab.json`'s events, verified).
- Produces: `EventType::RunCooked` (`"run.cooked"`, camp-specific, log-only with validated payload); `bead.created` payloads accept optional `run_id` and `step_id` strings folded into the existing `beads.run_id`/`beads.step_id` columns (schema v1 already has them; refold's `STATE_TABLES` already lists them — no schema or refold change).

- [ ] **Step 1: Write the failing tests** — create `crates/camp-core/tests/cook.rs`:

```rust
//! Cook-side ledger behavior: the run.cooked event, run-aware bead.created,
//! and (Task 7) the cook() transaction itself.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use camp_core::clock::FixedClock;
use camp_core::event::{EventInput, EventType};
use camp_core::ledger::Ledger;

fn temp_ledger() -> (tempfile::TempDir, Ledger) {
    let dir = tempfile::tempdir().unwrap();
    let ledger = Ledger::open_with_clock(
        &dir.path().join("camp.db"),
        Box::new(FixedClock::new("2026-07-05T21:14:03Z")),
    )
    .unwrap();
    (dir, ledger)
}

#[test]
fn run_cooked_round_trips_and_is_log_only() {
    let (_dir, mut ledger) = temp_ledger();
    assert_eq!(EventType::parse("run.cooked").unwrap(), EventType::RunCooked);
    ledger
        .append(EventInput {
            kind: EventType::RunCooked,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: None,
            data: serde_json::json!({
                "run_id": "20260705T211403Z-a1b2c3",
                "formula": "minimal",
                "root": "gc-1",
                "steps": {"only": "gc-2"}
            }),
        })
        .unwrap();
    // log-only: no bead rows appear
    let beads = ledger.list_beads(&Default::default()).unwrap();
    assert!(beads.is_empty());
    let events = ledger.events_range(1, None).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn run_cooked_payload_is_validated_and_rejects_unknown_fields() {
    let (_dir, mut ledger) = temp_ledger();
    for bad in [
        serde_json::json!({"formula": "m", "root": "gc-1", "steps": {}}), // missing run_id
        serde_json::json!({"run_id": "", "formula": "m", "root": "gc-1", "steps": {}}), // empty
        serde_json::json!({"run_id": "r", "formula": "m", "root": "gc-1", "steps": {}, "extra": 1}),
    ] {
        assert!(
            ledger
                .append(EventInput {
                    kind: EventType::RunCooked,
                    rig: Some("gc".into()),
                    actor: "cli".into(),
                    bead: None,
                    data: bad.clone(),
                })
                .is_err(),
            "must reject {bad}"
        );
    }
    assert!(ledger.events_range(1, None).unwrap().is_empty());
}

#[test]
fn bead_created_accepts_run_and_step_ids_and_refolds_exactly() {
    let (_dir, mut ledger) = temp_ledger();
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gc".into()),
            actor: "cli".into(),
            bead: Some("gc-1".into()),
            data: serde_json::json!({
                "title": "implement",
                "run_id": "20260705T211403Z-a1b2c3",
                "step_id": "implement"
            }),
        })
        .unwrap();
    let report = ledger.refold_check().unwrap();
    assert!(report.drift.is_empty(), "{:?}", report.drift);
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p camp-core --test cook` — expected: compile error (`RunCooked` not found).

- [ ] **Step 3: Implement (additive edits only)**

`event.rs` — add to the enum, `ALL`, and `as_str` (append after `RigAdded` in each):

```rust
    RunCooked,
```
```rust
        EventType::RunCooked,
```
```rust
            EventType::RunCooked => "run.cooked",
```

`vocab.rs` — append to `CAMP_SPECIFIC_EVENTS`:

```rust
    "run.cooked",
```

`fold.rs` — extend `BeadCreated` (two new optional fields at the end of the struct):

```rust
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    step_id: Option<String>,
```

extend `bead_created`'s INSERT to carry them:

```rust
    conn.execute(
        "INSERT INTO beads (id, rig, type, title, description, status, assignee, labels,
                            run_id, step_id, created_ts, updated_ts)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8, ?9, ?10, ?10)",
        params![
            id,
            rig,
            p.bead_type,
            p.title,
            p.description,
            p.assignee,
            serde_json::to_string(&p.labels)?,
            p.run_id,
            p.step_id,
            event.ts,
        ],
    )?;
```

add the `RunCooked` arm to `apply` (next to `RigAdded` — validated, log-only):

```rust
        EventType::RunCooked => run_cooked(event),
```

and the payload struct + fold function (mirroring `rig_added`'s pattern):

```rust
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunCooked {
    run_id: String,
    formula: String,
    root: String,
    steps: std::collections::BTreeMap<String, String>,
}

/// `run.cooked` is log-only: the run's durable truth is its beads (created
/// in the same transaction) and the pinned run dir. The fold validates the
/// audit payload so a malformed cook event fails fast.
fn run_cooked(event: &Event) -> Result<(), CoreError> {
    let p: RunCooked = payload(event)?;
    for (field, value) in [("run_id", &p.run_id), ("formula", &p.formula), ("root", &p.root)] {
        if value.is_empty() {
            return Err(CoreError::InvalidEventData {
                event_type: event.kind.as_str().to_owned(),
                reason: format!("empty {field}"),
            });
        }
    }
    Ok(())
}
```

(`steps` may be empty in the struct type but never is in practice — cook refuses formulas with zero steps via S3; the fold does not re-legislate validator rules.)

- [ ] **Step 4: Run and watch them pass**

Run: `cargo test -p camp-core` — the three new tests pass AND `vocab_pin` + `refold_prop` + all existing ledger tests stay green (the partition test fails the build if `run.cooked` is missing from either the enum or the vocab list — that is the pin doing its job).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/event.rs crates/camp-core/src/vocab.rs crates/camp-core/src/ledger/fold.rs crates/camp-core/tests/cook.rs
git commit -m "feat: run.cooked event and run/step ids on bead.created"
```

### Task 7: Cook — one transaction, one run directory

**Files:**
- Create: `crates/camp-core/src/formula/cook.rs`
- Modify: `crates/camp-core/src/formula/mod.rs` (add `mod cook;` + re-export `cook`, `CookedRun`)
- Modify: `crates/camp-core/src/ledger/mod.rs` (add `now_utc` accessor — additive, one method)
- Test: extend `crates/camp-core/tests/cook.rs`

**Interfaces:**
- Consumes: `Formula` (with `source`), `Ledger::{append_batch, next_bead_id, now_utc, is_ready, ready_beads, get_bead, refold_check}`, `camp_core::config::RigConfig`, `camp_core::id::parse_bead_id`, `EventType::{BeadCreated, RunCooked}`, `fastrand`.
- Produces (contract, with deviation 2 — `rig: &RigConfig`):

```rust
pub struct CookedRun {
    pub run_id: String,
    pub root_bead: String,
    pub step_beads: std::collections::BTreeMap<String, String>, // step_id -> bead id
}

/// Cook a validated formula into the ledger (spec §8.2): create
/// `<run_dir>/<run-id>/` with the pinned verbatim formula copy and
/// manifest.json, then materialize root + step beads + edges + the
/// run.cooked event in ONE append_batch transaction. From that moment the
/// run is independent of the formula file.
pub fn cook(
    ledger: &mut Ledger,
    formula: &Formula,
    run_dir: &Path,   // the runs/ root; cook creates run_dir/<run-id>/
    rig: &RigConfig,  // name goes on events; prefix allocates bead ids
    actor: &str,
) -> Result<CookedRun, CoreError>
```

Semantics locked here:
- `run_id` = `<utc-compact>-<6-hex>` — the ledger clock's now (`2026-07-05T21:14:03Z` → `20260705T211403Z`) + `-` + 6 lowercase hex chars from `fastrand::u32`. Deterministic time part under `FixedClock`.
- Bead ids: contiguous block from Phase 3's per-rig counter — read `next_bead_id(prefix)` once, parse its number `n` via `id::parse_bead_id`, allocate root = `{prefix}-{n}`, steps = `{prefix}-{n+1}`… in `steps` order. The `bead.created` fold bumps the counter per event inside the txn, so the block commits atomically; a concurrent writer racing the same ids makes the batch fail on the duplicate id and roll back everything (fail fast — same race window `camp create` already has).
- Root bead: `title` = formula name, `description` = formula description (when set), `run_id` set, no `step_id`, `needs` = every step bead id — so the root is never dispatch-ready while steps are open, and the last step's close makes it `newly_ready` (Phase 9's finalization trigger).
- Step beads: `title`/`description` from the step, `assignee` passthrough, `needs` = the step's `needs` mapped through the step→bead map (rig-prefixed by construction), `run_id` + `step_id` set.
- Event order in the batch: root create, step creates (formula order), `run.cooked` last (bead = root id) — the cause chain reads top-down.
- Files BEFORE the txn: `create_dir_all(run_dir)`, then `create_dir(dir)` (pre-existing `<run-id>` dir = hard error), write `<name>.toml` (verbatim `formula.source` bytes) and `manifest.json`. If `append_batch` fails, remove the run dir; if THAT cleanup also fails, surface both errors in one `CoreError::Corrupt` — nothing is silenced. (A hard crash between file-write and commit leaves an inert dir with no `run.cooked` event — visible, explainable, and harmless: nothing references it.)
- `manifest.json` (serde_json, pretty): `{"run_id", "formula", "rig", "actor", "cooked_ts", "root", "steps": {step_id: bead_id}}`.

- [ ] **Step 1: Write the failing tests** — append to `crates/camp-core/tests/cook.rs`:

```rust
use std::path::Path;

use camp_core::config::RigConfig;
use camp_core::formula::{cook, parse_and_validate};

fn fixture(stem: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/formulas/valid")
        .join(format!("{stem}.toml"))
}

fn rig() -> RigConfig {
    RigConfig { name: "gascity".into(), path: "/code/gascity".into(), prefix: "gc".into() }
}

#[test]
fn cook_materializes_a_diamond_run_in_one_transaction() {
    let (dir, mut ledger) = temp_ledger();
    let formula = parse_and_validate(&fixture("diamond")).unwrap();
    let runs = dir.path().join("runs");
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();

    // run_id shape: utc-compact from the FixedClock + 6 hex
    assert!(cooked.run_id.starts_with("20260705T211403Z-"), "{}", cooked.run_id);
    let suffix = cooked.run_id.rsplit('-').next().unwrap();
    assert_eq!(suffix.len(), 6);
    assert!(suffix.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

    // beads: root + 4 steps, contiguous rig-prefixed ids
    assert_eq!(cooked.root_bead, "gc-1");
    assert_eq!(cooked.step_beads.len(), 4);
    assert_eq!(cooked.step_beads["design"], "gc-2");
    assert_eq!(cooked.step_beads["release"], "gc-5");

    // step bead carries run_id/step_id/assignee; needs are bead ids
    let release = ledger.get_bead("gc-5").unwrap().unwrap();
    assert_eq!(release.assignee.as_deref(), Some("dev"));
    assert_eq!(release.rig, "gascity");

    // events: 5 creates + run.cooked, all in one batch
    let events = ledger.events_range(1, None).unwrap();
    assert_eq!(events.len(), 6);
    assert_eq!(events[0].bead.as_deref(), Some("gc-1"));
    assert_eq!(events[5].kind, camp_core::event::EventType::RunCooked);
    assert_eq!(events[5].bead.as_deref(), Some("gc-1"));
    assert_eq!(events[5].data["steps"]["design"], "gc-2");

    // refold property holds over a cooked ledger
    assert!(ledger.refold_check().unwrap().drift.is_empty());
}

#[test]
fn cooked_graphs_satisfy_phase_3_readiness_roots_ready_dependents_not() {
    let (dir, mut ledger) = temp_ledger();
    let formula = parse_and_validate(&fixture("diamond")).unwrap();
    let cooked = cook(&mut ledger, &formula, &dir.path().join("runs"), &rig(), "cli").unwrap();

    let ready: Vec<String> = ledger
        .ready_beads(None)
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect();
    // Only the dag root step (design) is ready. Dependents are blocked, and
    // the run root needs every step, so it is blocked too.
    assert_eq!(ready, vec![cooked.step_beads["design"].clone()]);
    assert!(!ledger.is_ready(&cooked.root_bead).unwrap());
    assert!(!ledger.is_ready(&cooked.step_beads["implement"]).unwrap());
    assert!(!ledger.is_ready(&cooked.step_beads["release"]).unwrap());
}

#[test]
fn cook_pins_the_formula_verbatim_and_writes_the_manifest() {
    let (dir, mut ledger) = temp_ledger();
    let source_path = fixture("guarded-change");
    let formula = parse_and_validate(&source_path).unwrap();
    let runs = dir.path().join("runs");
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();

    let run_dir = runs.join(&cooked.run_id);
    let pinned = std::fs::read_to_string(run_dir.join("guarded-change.toml")).unwrap();
    assert_eq!(pinned, std::fs::read_to_string(&source_path).unwrap(), "verbatim copy");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_dir.join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["run_id"], cooked.run_id.as_str());
    assert_eq!(manifest["formula"], "guarded-change");
    assert_eq!(manifest["rig"], "gascity");
    assert_eq!(manifest["actor"], "cli");
    assert_eq!(manifest["cooked_ts"], "2026-07-05T21:14:03Z");
    assert_eq!(manifest["root"], cooked.root_bead.as_str());
    assert_eq!(manifest["steps"]["implement"], cooked.step_beads["implement"].as_str());
}

#[test]
fn cook_is_file_independent_afterwards() {
    let (dir, mut ledger) = temp_ledger();
    // copy the fixture somewhere deletable, cook it, delete the original
    let scratch = dir.path().join("minimal.toml");
    std::fs::copy(fixture("minimal"), &scratch).unwrap();
    let formula = parse_and_validate(&scratch).unwrap();
    let cooked = cook(&mut ledger, &formula, &dir.path().join("runs"), &rig(), "cli").unwrap();
    std::fs::remove_file(&scratch).unwrap();

    // the run lives on: beads dispatchable, pinned copy present
    assert!(ledger.is_ready(&cooked.step_beads["only"]).unwrap());
    assert!(
        dir.path()
            .join("runs")
            .join(&cooked.run_id)
            .join("minimal.toml")
            .exists()
    );
}

#[test]
fn cook_atomicity_fault_injection_leaves_nothing() {
    // Requires `rusqlite` in camp-core [dev-dependencies] (same version as
    // the main dep, "0.40.1") — integration tests only see dev-deps.
    let (dir, mut ledger) = temp_ledger();
    // Occupy gc-2 through the public API (the counter advances to 2), then
    // FAULT-INJECT by winding the counter back with a raw connection so
    // cook allocates a block that collides with gc-2 mid-batch: root gc-1
    // inserts fine, the first step create hits the gc-2 UNIQUE constraint,
    // and the whole transaction must roll back.
    ledger
        .append(EventInput {
            kind: EventType::BeadCreated,
            rig: Some("gascity".into()),
            actor: "test".into(),
            bead: Some("gc-2".into()),
            data: serde_json::json!({"title": "squatter"}),
        })
        .unwrap();
    let db = dir.path().join("camp.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute("UPDATE counters SET high = 0 WHERE prefix = 'gc'", [])
            .unwrap();
    }
    let events_before = ledger.events_range(1, None).unwrap().len();

    let formula = parse_and_validate(&fixture("diamond")).unwrap();
    let runs = dir.path().join("runs");
    let err = cook(&mut ledger, &formula, &runs, &rig(), "cli");
    assert!(err.is_err(), "colliding id block must fail the whole cook");

    // NOTHING landed: no new events, no beads beyond the squatter, no run dir
    assert_eq!(ledger.events_range(1, None).unwrap().len(), events_before);
    assert_eq!(ledger.list_beads(&Default::default()).unwrap().len(), 1);
    let leftover = std::fs::read_dir(&runs)
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(leftover, 0, "run dir must be removed on rollback");

    // The injected counter tamper is exactly what doctor --refold repairs;
    // after repair the ledger is drift-free and cooking works again.
    ledger.refold_repair().unwrap();
    assert!(ledger.refold_check().unwrap().drift.is_empty());
    let cooked = cook(&mut ledger, &formula, &runs, &rig(), "cli").unwrap();
    assert_eq!(cooked.root_bead, "gc-3");
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p camp-core --test cook` — expected: compile error (`cook` not found).

- [ ] **Step 3: Implement**

`crates/camp-core/Cargo.toml` — add to `[dev-dependencies]` (the fault-injection test opens a raw connection):

```toml
rusqlite = "0.40.1"
```

`crates/camp-core/src/ledger/mod.rs` — one additive method on `impl Ledger` (place after `open_with_clock`):

```rust
    /// The clock's current timestamp (RFC3339 UTC, whole seconds) — the same
    /// source event timestamps use, so run ids are deterministic in tests.
    pub fn now_utc(&self) -> String {
        self.clock.now_utc()
    }
```

`crates/camp-core/src/formula/cook.rs`:

```rust
//! Cook: materialize a validated formula into the ledger (spec §8.2).
//! Files first (runs/<run-id>/ with the pinned copy + manifest), then ONE
//! append_batch transaction for root + steps + run.cooked. Gas City's
//! materialization property, kept: after cook the run is independent of
//! the formula file.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::RigConfig;
use crate::error::CoreError;
use crate::event::{EventInput, EventType};
use crate::formula::ast::Formula;
use crate::ledger::Ledger;

#[derive(Debug, Clone, PartialEq)]
pub struct CookedRun {
    pub run_id: String,
    pub root_bead: String,
    pub step_beads: BTreeMap<String, String>,
}

pub fn cook(
    ledger: &mut Ledger,
    formula: &Formula,
    run_dir: &Path,
    rig: &RigConfig,
    actor: &str,
) -> Result<CookedRun, CoreError> {
    if formula.steps.is_empty() {
        // parse_and_validate guarantees this (rule S3); cook re-checks its
        // own precondition rather than cooking an empty run.
        return Err(CoreError::Corrupt(format!(
            "cook: formula {:?} has no steps — cook requires parse_and_validate output",
            formula.name
        )));
    }

    let ts = ledger.now_utc();
    let run_id = format!(
        "{}-{:06x}",
        ts.replace(['-', ':'], ""),
        fastrand::u32(..) & 0xFF_FFFF
    );

    // ---- id block allocation (Phase 3 counter; see plan for the race note)
    let first = ledger.next_bead_id(&rig.prefix)?;
    let (prefix, n) = crate::id::parse_bead_id(&first).ok_or_else(|| {
        CoreError::Corrupt(format!("next_bead_id returned unparseable id {first:?}"))
    })?;
    let root_bead = format!("{prefix}-{n}");
    let mut step_beads: BTreeMap<String, String> = BTreeMap::new();
    for (offset, step) in formula.steps.iter().enumerate() {
        step_beads.insert(step.id.clone(), format!("{prefix}-{}", n + 1 + offset as i64));
    }

    // ---- files first: runs/<run-id>/ with pinned copy + manifest
    let dir = run_dir.join(&run_id);
    std::fs::create_dir_all(run_dir).map_err(|e| {
        CoreError::Corrupt(format!("cook: cannot create {}: {e}", run_dir.display()))
    })?;
    std::fs::create_dir(&dir).map_err(|e| {
        CoreError::Corrupt(format!("cook: cannot create {}: {e}", dir.display()))
    })?;
    let write = |name: &str, bytes: &[u8]| -> Result<(), CoreError> {
        std::fs::write(dir.join(name), bytes).map_err(|e| {
            CoreError::Corrupt(format!("cook: cannot write {}/{name}: {e}", dir.display()))
        })
    };
    write(&format!("{}.toml", formula.name), formula.source.as_bytes())?;
    let manifest = serde_json::json!({
        "run_id": run_id,
        "formula": formula.name,
        "rig": rig.name,
        "actor": actor,
        "cooked_ts": ts,
        "root": root_bead,
        "steps": step_beads,
    });
    write("manifest.json", format!("{manifest:#}").as_bytes())?;

    // ---- one transaction: root, steps, run.cooked
    let mut inputs = Vec::with_capacity(formula.steps.len() + 2);
    let mut root_data = serde_json::json!({
        "title": formula.name,
        "needs": step_beads.values().collect::<Vec<_>>(),
        "run_id": run_id,
    });
    if let Some(d) = &formula.description {
        root_data["description"] = serde_json::json!(d);
    }
    inputs.push(EventInput {
        kind: EventType::BeadCreated,
        rig: Some(rig.name.clone()),
        actor: actor.to_owned(),
        bead: Some(root_bead.clone()),
        data: root_data,
    });
    for step in &formula.steps {
        let needs: Vec<&String> = step
            .needs
            .iter()
            .filter_map(|id| step_beads.get(id))
            .collect();
        let mut data = serde_json::json!({
            "title": step.title,
            "run_id": run_id,
            "step_id": step.id,
        });
        if let Some(d) = &step.description {
            data["description"] = serde_json::json!(d);
        }
        if !needs.is_empty() {
            data["needs"] = serde_json::json!(needs);
        }
        if let Some(a) = &step.assignee {
            data["assignee"] = serde_json::json!(a);
        }
        inputs.push(EventInput {
            kind: EventType::BeadCreated,
            rig: Some(rig.name.clone()),
            actor: actor.to_owned(),
            bead: Some(step_beads[&step.id].clone()),
            data,
        });
    }
    inputs.push(EventInput {
        kind: EventType::RunCooked,
        rig: Some(rig.name.clone()),
        actor: actor.to_owned(),
        bead: Some(root_bead.clone()),
        data: serde_json::json!({
            "run_id": run_id,
            "formula": formula.name,
            "root": root_bead,
            "steps": step_beads,
        }),
    });

    if let Err(batch_err) = ledger.append_batch(inputs) {
        // Roll the files back too; a cleanup failure is reported WITH the
        // original error, never instead of it and never silently.
        return Err(match std::fs::remove_dir_all(&dir) {
            Ok(()) => batch_err,
            Err(cleanup) => CoreError::Corrupt(format!(
                "cook failed ({batch_err}) and the run dir {} could not be removed: {cleanup}",
                dir.display()
            )),
        });
    }

    Ok(CookedRun { run_id, root_bead, step_beads })
}
```

`formula/mod.rs` — add `mod cook;` and extend the re-exports:

```rust
pub use cook::{CookedRun, cook};
```

- [ ] **Step 4: Run and watch them pass**

Run: `cargo test -p camp-core --test cook` — all cook tests green. Then the full crate: `cargo test -p camp-core` (refold property, vocab pin, corpus all stay green).

- [ ] **Step 5: Commit**

```bash
git add crates/camp-core/src/formula/ crates/camp-core/src/ledger/mod.rs crates/camp-core/tests/cook.rs
git commit -m "feat: cook formulas into runs/<run-id> and the ledger in one transaction"
```

### Task 8: `camp doctor --formula <path>`

Shared-file task (`main.rs`): the Doctor variant gains one optional arg; nothing else in `main.rs` moves.

**Files:**
- Modify: `crates/camp/src/main.rs` (Doctor variant + dispatch)
- Modify: `crates/camp/src/cmd/doctor.rs` (add `run_formula`)
- Test: `crates/camp/tests/cli_doctor_formula.rs` (new file — leaves Phase 1's `cli_doctor.rs` untouched)

**Interfaces:**
- Consumes: `camp_core::formula::parse_and_validate`.
- Produces: `camp doctor --formula <path>` — exit 0 on a valid formula (prints `formula ok: <name> (<N> step(s))`), exit 1 printing EVERY violation (stdout, one per line, construct-prefixed) plus a stderr summary. `--refold` and `--formula` are mutually exclusive; exactly one is required.

- [ ] **Step 1: Write the failing test** — `crates/camp/tests/cli_doctor_formula.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;

fn camp() -> Command {
    let mut cmd = Command::cargo_bin("camp").unwrap();
    cmd.env_remove("CAMP_DIR");
    cmd
}

fn write(dir: &std::path::Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn doctor_formula_exits_0_on_a_valid_formula() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let f = write(
        dir.path(),
        "minimal.toml",
        "formula = \"minimal\"\n\n[[steps]]\nid = \"only\"\ntitle = \"t\"\n",
    );
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--formula"])
        .arg(&f)
        .assert()
        .success()
        .stdout(predicates::str::contains("formula ok: minimal (1 step(s))"));
}

#[test]
fn doctor_formula_exits_1_listing_every_violation() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    let f = write(
        dir.path(),
        "broken.toml",
        "formula = \"wrong-name\"\npour = true\n\n[[steps]]\nid = \"a\"\ntitle = \"t\"\ntags = [\"x\"]\nneeds = [\"ghost\"]\n",
    );
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--formula"])
        .arg(&f)
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("pour"))
        .stdout(predicates::str::contains("tags"))
        .stdout(predicates::str::contains("file stem"))
        .stdout(predicates::str::contains("ghost"))
        .stderr(predicates::str::contains("violation"));
}

#[test]
fn doctor_requires_exactly_one_of_refold_or_formula() {
    let dir = tempfile::tempdir().unwrap();
    camp().current_dir(dir.path()).arg("init").assert().success();
    camp()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .failure()
        .code(2); // clap usage error
    camp()
        .current_dir(dir.path())
        .args(["doctor", "--refold", "--formula", "x.toml"])
        .assert()
        .failure()
        .code(2);
}
```

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p camp --test cli_doctor_formula` — expected: FAIL (`--formula` unknown argument).

- [ ] **Step 3: Implement**

`crates/camp/src/main.rs` — replace the Doctor variant (only this variant changes; `required = true` moves off `refold` and onto a clap group so exactly one mode is chosen):

```rust
    /// Verify ledger invariants
    #[command(group(
        clap::ArgGroup::new("mode").required(true).args(["refold", "formula"])
    ))]
    Doctor {
        /// Rebuild state from the event log and report drift (spec §13.5)
        #[arg(long)]
        refold: bool,
        /// Replace the state tables with the refolded content
        #[arg(long, requires = "refold")]
        repair: bool,
        /// Validate a formula file against the camp subset (spec §8.2)
        #[arg(long, value_name = "PATH", conflicts_with = "refold")]
        formula: Option<PathBuf>,
    },
```

and the dispatch arm:

```rust
        Command::Doctor { refold: _, repair, formula } => match formula {
            Some(path) => cmd::doctor::run_formula(&path),
            None => {
                let camp = CampDir::resolve(cli.camp.as_deref())?;
                cmd::doctor::run(&camp, repair)
            }
        },
```

(`--formula` validates a file, not a camp — it must work without resolving a camp dir, exactly like `gc doctor --formula` inspects a file. `--refold` keeps its CampDir resolution.)

`crates/camp/src/cmd/doctor.rs` — append:

```rust
use std::path::Path;

/// `camp doctor --formula <path>`: validate one formula file against the
/// camp subset (spec §8.2). Exit 0 = valid camp formula (and therefore a
/// valid Gas City formula-v2 file, repo invariant 6); exit 1 = every
/// violation printed, not just the first.
pub fn run_formula(path: &Path) -> Result<()> {
    match camp_core::formula::parse_and_validate(path) {
        Ok(formula) => {
            println!("formula ok: {} ({} step(s))", formula.name, formula.steps.len());
            Ok(())
        }
        Err(err) => {
            for violation in &err.violations {
                println!("{violation}");
            }
            bail!(
                "{}: {} violation(s) — camp accepts a strict subset of Gas City formula v2 (spec §8.2)",
                err.path.display(),
                err.violations.len()
            );
        }
    }
}
```

- [ ] **Step 4: Run and watch it pass**

Run: `cargo test -p camp --test cli_doctor_formula` — 3 passed; then `cargo test -p camp` (all existing CLI tests, notably `cli_doctor.rs`, stay green — the `--refold` surface is unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/camp/src/main.rs crates/camp/src/cmd/doctor.rs crates/camp/tests/cli_doctor_formula.rs
git commit -m "feat: camp doctor --formula validates the camp subset with full violation listing"
```

### Task 9: Gates, push, PR

- [ ] **Step 1: Full gates**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

All three must be clean. Fix anything they surface before proceeding (fmt failures: run `cargo fmt --all` and re-check).

- [ ] **Step 2: Rebase check**

If the lead announced any sibling merge since the last rebase: `git fetch origin && git rebase origin/main`, resolve (expected zones: `main.rs`, `event.rs`, `vocab.rs`, `fold.rs`, `Cargo.toml`/`Cargo.lock`), and re-run Step 1 in full. Never open or update the PR from a branch not rebased onto current main.

- [ ] **Step 3: Push and PR**

```bash
git push -u origin phase-5-formula-subset
gh pr create --title "Phase 5: formula subset compiler and cook" --body "<summary per template below>"
gh pr checks --watch
```

PR body must state: the acceptance/rejection surface (link spec §8.2), the contract deviations (this plan's list, so the reviewer sees them without digging), the corpus layout, and the exit-criteria evidence (below). CI must be green before reporting done.

- [ ] **Step 4: Report to the lead** — PR number, CI status, and the master-plan exit criteria quoted line by line with evidence:

| Exit criterion (master plan) | Evidence |
|---|---|
| "corpus green under camp's validator" | `formula_corpus.rs` — `every_valid_fixture_is_accepted`, `every_invalid_fixture_is_rejected_naming_the_construct`, both green in CI |
| "cook produces dispatchable graphs" | `cook.rs` tests — `cooked_graphs_satisfy_phase_3_readiness_roots_ready_dependents_not` (dag roots ready, dependents and run root not), `cook_materializes_a_diamond_run_in_one_transaction` |
| "CI green" | `gh pr checks` output |

## Post-plan notes for the implementer

- **Phase 6 handoff:** `valid/` will be compiled by the real gc compiler in CI. If Phase 6 finds a valid fixture gc rejects, the bug is HERE (camp accepted something outside the subset) — fix camp's validator and the fixture, never relax the fixture alone. The `fan-out.toml` on_complete fixture is the first of its kind anywhere; treat a Phase 6 failure on it as expected-and-valuable signal, not noise.
- **Spec alignment:** no spec § contradicts this plan; the one behavioral sharpening (`timeout` requires `check`) is already how spec §8.2 words the construct ("step-level timeout (general bound on the check script)"). If implementation reveals any real spec divergence, stop and update the spec in this same PR.
- **What Phase 5 does NOT do:** no execution of checks/retries/fan-out (Phase 9), no dispatch (Phase 8), no `camp sling --formula` verb (Phase 8 wraps cook), no gc-compat CI job (Phase 6).

## Self-review (performed while writing)

1. **Spec coverage:** every master-plan Phase 5 bullet maps to a task — interfaces (T1/T4/T7), acceptance table (T3/T4), combination + explicit-declaration rules (T4), rejection table incl. deny-unknown (T3/T5), cook mechanics + `run.cooked` + one-transaction (T6/T7), fixture corpus incl. spec-verbatim guarded-change and the five named shapes (T5), doctor exit codes (T8), gates/CI (T9). The contract's five pinned struct/signature items appear verbatim in T1/T4/T7 with deviations flagged up top.
2. **Placeholder scan:** no TBDs; every step carries complete code or an exact command with expected outcome.
3. **Type consistency:** `RawFormula`/`RawStep`/`RawRetry` field names match between T3 (producer) and T4 (consumer); `Violation.construct` conventions (`bare key` vs `steps.<id>.<field>`) are defined once in T3 and used identically in T4/T5/T8 test assertions; `CookedRun` field names match T7 tests; `FORMULA_COMPILER_CAPABILITY` is declared in T4 and referenced nowhere else by literal.

