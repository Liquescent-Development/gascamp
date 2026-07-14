//! The LAYERED compiler (compat §9 rung 2a): `description_file` through the
//! layer stack, gc's >4096 pointer prompt, containment, and D1's
//! compiles-but-not-runnable verdict.
//!
//! The fixture is the real corpus shape in miniature: a camp root (the
//! CAMP-LOCAL tier, highest priority) that IMPORTS a parent pack. Every corpus
//! formula lives in an imported pack and reaches assets in another one.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(non_snake_case)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use camp_core::config::CampConfig;
use camp_core::formula::{FormulaLayers, compile, compile_named};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compose")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// A camp root whose CAMP-LOCAL tier is `fixtures/compose/local/` and which
/// imports `fixtures/compose/parent/` as the binding `gc`.
struct Camp {
    _dir: tempfile::TempDir,
    root: PathBuf,
    cfg: CampConfig,
    layers: FormulaLayers,
}

fn camp() -> Camp {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    copy_tree(&fixtures().join("local"), &root);
    let parent = fixtures().join("parent");
    std::fs::write(
        root.join("camp.toml"),
        format!(
            "[camp]\nname = \"t\"\n\n[agent_defaults]\ntools = [\"Read\"]\n\n[imports.gc]\nsource = {:?}\n",
            parent.display().to_string()
        ),
    )
    .unwrap();
    let cfg = CampConfig::load(&root.join("camp.toml")).unwrap();
    let layers = FormulaLayers::from_config(&cfg, &root).unwrap();
    Camp {
        _dir: dir,
        root,
        cfg,
        layers,
    }
}

fn no_vars() -> BTreeMap<String, String> {
    BTreeMap::new()
}

#[test]
fn description_file_contents_replace_the_step_description() {
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "inherited", &no_vars()).unwrap();
    assert_eq!(
        compiled.formula.steps[0].description.as_deref(),
        Some("Only the parent ships this.\n"),
        "the file's CONTENTS replace the description; the steps that carry a \
         description_file typically have no inline description at all, so ignoring \
         the key gives the worker zero instructions"
    );
}

#[test]
fn an_asset_reference_resolves_through_the_layers_highest_wins() {
    // Both the parent pack and the camp-local tier ship
    // `assets/workflows/implement.md`. The HIGHEST layer wins — that is how a
    // pack overrides prose while inheriting structure (gc `winningAssetPath`).
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "shadowed", &no_vars()).unwrap();
    assert_eq!(
        compiled.formula.steps[0].description.as_deref(),
        Some("LOCAL OVERRIDE.\n")
    );
}

#[test]
fn an_inherited_asset_in_the_parents_pack_resolves_and_is_not_an_escape() {
    // The containment root is the WINNING LAYER's pack root, not the declaring
    // formula's. 32 cross-pack `extends` edges inherit a step whose asset lives
    // in the parent's pack; anchoring on the child would call every one of them
    // an escape and the ceiling would collapse.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "inherited", &no_vars()).unwrap();
    assert!(compiled.formula.steps[0].description.is_some());
}

#[test]
fn an_oversize_description_file_becomes_gcs_pointer_prompt() {
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "oversize", &no_vars()).unwrap();
    let d = compiled.formula.steps[0].description.as_deref().unwrap();

    // gc's text, byte for byte (`descriptionFileReferenceDescription`). A
    // mis-transcribed paragraph is a divergence no camp-only test can see —
    // differential.py diffs its sha256 against gc's own output.
    assert!(d.starts_with("# External Prompt Required\n\n"), "{d}");
    assert!(d.contains("- Prompt file size: 5400 bytes\n\n"), "{d}");
    assert!(
        d.contains("- Original formula description_file: `../assets/workflows/big.md`\n"),
        "{d}"
    );
    // The file is NOT inlined.
    assert!(
        !d.contains("large prompt body"),
        "the body must not be inlined"
    );
}

#[test]
fn a_missing_description_file_is_a_hard_error_for_a_graph_v2_formula() {
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "missing-asset", &no_vars()).unwrap_err();
    assert!(err.names("description_file"), "{err}");
    assert!(err.to_string().contains("nope.md"), "{err}");
}

#[test]
fn a_description_file_escaping_the_pack_root_is_refused() {
    // Camp imports arbitrary third-party packs. gc's non-asset branch is a bare
    // join, so a pack could name `../../../../.ssh/id_rsa` and have it inlined
    // into a bead description that a tool-enabled worker then reads.
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "escape", &no_vars()).unwrap_err();
    assert!(err.names("description_file"), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("outside the pack root"),
        "the refusal must say WHY: {text}"
    );
}

#[test]
fn a_run_target_is_carried_verbatim_and_NOT_substituted_at_compile() {
    // F1 — the fact rev 2 got backwards, and the reason the phase was rewritten.
    // `{{var}}` does NOT resolve at compile. 55 corpus routes are still
    // `{{implementation_target}}` in gc's own compiled Recipe, EVEN WHERE THE VAR
    // HAS A DEFAULT. Substitution happens at instantiation (cook).
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "base", &no_vars()).unwrap();
    let md = &compiled.formula.steps[0].metadata;
    assert_eq!(
        md.get("gc.run_target").map(String::as_str),
        Some("{{implementation_target}}"),
        "the route survives compile verbatim"
    );
    // And an accepted-but-unhonoured annotation rides through untouched (a named
    // fidelity cost — camp carries it, camp does not act on it).
    assert_eq!(
        md.get("gc.build.artifact_schema").map(String::as_str),
        Some("x")
    );
    // The description came from an asset and still carries its `{{var}}` too.
    assert!(
        compiled.formula.steps[0]
            .description
            .as_deref()
            .unwrap()
            .contains("{{implementation_target}}")
            || compiled.formula.steps[0]
                .description
                .as_deref()
                .unwrap()
                .contains("LOCAL OVERRIDE"),
        "whichever layer won, compile did not substitute"
    );
}

#[test]
fn a_no_contract_formula_compiles_and_is_not_runnable() {
    // D1 — LOADABLE ≠ RUNNABLE. 19 of the 95 corpus formulas camp loads declare
    // no contract and 14 are expansions (disjoint): only 62 can be slung. "95/100"
    // alone is a misleading headline.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "no-contract", &no_vars()).unwrap();
    assert!(!compiled.is_runnable());
    let why = compiled.not_runnable.unwrap();
    assert!(why.reason.contains("graph.v2"), "{}", why.reason);

    // And a contract-bearing one IS runnable.
    let ok = compile_named(&c.layers, &c.cfg, "inherited", &no_vars()).unwrap();
    assert!(ok.is_runnable());
}

#[test]
fn an_imported_formula_compiles_at_the_permissive_tier_and_a_local_one_at_the_strict_one() {
    // D2′ — the origin decides, and `origin_of` reads it off the layer stack.
    let c = camp();
    let imported = fixtures().join("parent/formulas/base.formula.toml");
    assert_eq!(
        c.layers.origin_of(&imported),
        camp_core::formula::Origin::Imported
    );
    let local = c.root.join("formulas/no-contract.toml");
    assert_eq!(
        c.layers.origin_of(&local),
        camp_core::formula::Origin::CampLocal
    );
}

#[test]
fn compile_by_path_and_by_name_agree() {
    let c = camp();
    let by_name = compile_named(&c.layers, &c.cfg, "shadowed", &no_vars()).unwrap();
    let by_path = compile(
        &c.layers,
        &c.cfg,
        &c.root.join("formulas/shadowed.toml"),
        &no_vars(),
    )
    .unwrap();
    assert_eq!(by_name.formula, by_path.formula);
}

// ---- rung 2b: vars, condition pruning, and the {{var}} STAGE ---------------

fn ids(c: &camp_core::formula::Compiled) -> Vec<&str> {
    c.formula.steps.iter().map(|s| s.id.as_str()).collect()
}

#[test]
fn a_false_condition_prunes_the_step_its_children_AND_its_refusals() {
    // ⭐ BD2, and it is what separates a ceiling of 95 from one of 76.
    //
    // `impl-shared` carries a `gate` — a §4 rule-1 REFUSAL. Its condition is
    // false under the default `drain_policy = "separate"`, so the step prunes and
    // ITS REFUSAL MUST DIE WITH IT. Collect refusals at parse and never re-filter
    // them, and 19 corpus formulas with a conditional shared-drain arm refuse —
    // taking bmad-build, gstack-build and compound-build with them.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "guarded", &no_vars())
        .expect("the shared arm's refusal must die with the pruned step");
    assert!(
        !ids(&compiled).contains(&"impl-shared"),
        "{:?}",
        ids(&compiled)
    );
    assert!(ids(&compiled).contains(&"impl-separate"));
    assert!(compiled.refusals.is_empty(), "{:?}", compiled.refusals);
}

#[test]
fn a_true_condition_keeps_the_step_and_an_override_flips_which_arm_survives() {
    // gc's `Compile` takes vars, and conditions resolve at COMPILE — so a
    // sling-time override must change what is pruned. And when the SHARED arm
    // survives, its refusal survives with it: camp refuses rather than
    // approximate.
    let c = camp();
    let overrides = BTreeMap::from([("drain_policy".to_owned(), "same-session".to_owned())]);
    let err = compile_named(&c.layers, &c.cfg, "guarded", &overrides)
        .expect_err("the shared arm now survives, and it is refused");
    assert!(err.names("gate"), "{err}");
}

#[test]
fn review_mode_defaults_to_report_so_the_guarded_child_prunes() {
    // The 4 `{{review_mode}} != report` conditions. `review_mode`'s default
    // VARIES BY PACK, so the merged chain decides — here it is `report`, so the
    // guarded step prunes and is NOT a violation.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "guarded", &no_vars()).unwrap();
    assert!(!ids(&compiled).contains(&"apply-review-findings"));
    // And the dangling `needs` on it is DROPPED — a step still needing a pruned
    // step would never dispatch and the run would dead-end.
    let publish = compiled
        .formula
        .steps
        .iter()
        .find(|s| s.id == "publish")
        .unwrap();
    assert_eq!(
        publish.needs,
        vec!["impl-separate"],
        "the dangling need on the pruned step is dropped"
    );
}

#[test]
fn compile_does_NOT_substitute_double_brace_vars_anywhere() {
    // F1 — the fact rev 2 got backwards. `{{var}}` survives COMPILE, even where
    // the var HAS a default. 561 corpus steps still carry one afterwards.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "guarded", &no_vars()).unwrap();
    let publish = compiled
        .formula
        .steps
        .iter()
        .find(|s| s.id == "publish")
        .unwrap();
    assert_eq!(
        publish.title, "Publish {{drain_policy}}",
        "drain_policy HAS a default and the placeholder still survives compile"
    );
    assert!(
        publish
            .description
            .as_deref()
            .unwrap()
            .contains("{{implementation_target}}")
    );
}

#[test]
fn a_condition_outside_the_subset_is_a_violation_naming_the_step() {
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "bad-condition", &no_vars()).unwrap_err();
    assert!(err.names("steps.a.condition"), "{err}");
    assert!(err.to_string().contains("outside camp's subset"), "{err}");
}

#[test]
fn vars_with_no_default_are_declared_but_undefined() {
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "guarded", &no_vars()).unwrap();
    // The name EXISTS (gc's oversize prompt lists every declared name) …
    assert!(compiled.formula.vars.contains_key("implementation_target"));
    // … and it resolves to nothing, so its placeholder survives to the worker.
    assert_eq!(compiled.formula.vars["implementation_target"], None);
    assert_eq!(
        compiled.formula.vars["drain_policy"],
        Some("separate".to_owned())
    );
}

// ---- rung 2c: extends ------------------------------------------------------

#[test]
fn a_parents_steps_append_and_a_matching_child_id_replaces_in_place() {
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "chain-child", &no_vars()).unwrap();
    // Position PRESERVED: `first` stays first even though the child re-declares it.
    assert_eq!(
        ids(&compiled),
        vec!["first", "second", "refused-here", "third"]
    );
    let first = &compiled.formula.steps[0];
    assert_eq!(first.title, "First (child)");
    // REPLACED WHOLE — no field-level merge. The child omits `description`, so it
    // does NOT inherit the parent's.
    assert_eq!(
        first.description, None,
        "a replaced step is replaced WHOLE; there is no field-level merge"
    );
}

#[test]
fn a_refusal_on_a_parent_step_that_the_child_replaces_is_discarded() {
    // BD2's NEW failure mode. The parent's `refused-here` step carries a `gate`
    // (a §4 rule-1 refusal). The child REPLACES that step in place with a clean
    // one — so the parent's refusal must die with the step it belonged to.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "chain-child", &no_vars())
        .expect("the replaced parent step's refusal must be discarded");
    assert!(compiled.refusals.is_empty(), "{:?}", compiled.refusals);
    assert_eq!(compiled.formula.steps[2].title, "Clean replacement");
}

#[test]
fn the_child_seeds_scalars_and_inherits_the_parents_vars() {
    // `drain_policy = "separate"` is declared in gascity's `build-base`, NOT in
    // the children that depend on it — that inheritance is load-bearing for the
    // whole shared-drain pruning story.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "chain-child", &no_vars()).unwrap();
    assert_eq!(
        compiled.formula.vars["drain_policy"],
        Some("separate".to_owned())
    );
    // And `contract` INHERITS (gc parser.go:308) — the child declares none.
    assert!(compiled.is_runnable(), "contract inherits from the parent");
}

#[test]
fn a_parent_resolves_by_bare_name_through_the_layers() {
    // `chain-base` lives in the IMPORTED parent pack; the child is camp-local.
    // Parents resolve by bare name through the layer stack — §7.2 is what puts
    // `build-base` within reach of `bmad-build`.
    let c = camp();
    assert!(compile_named(&c.layers, &c.cfg, "chain-child", &no_vars()).is_ok());
}

#[test]
fn an_unresolvable_parent_is_a_hard_error_naming_it() {
    // `mol-polecat-work` extends `mol-polecat-base`, which is absent from the
    // corpus. gc fails it too — gc compiles 99/100.
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "orphan", &no_vars()).unwrap_err();
    assert!(err.names("extends"), "{err}");
    assert!(err.to_string().contains("no-such-parent"), "{err}");
}

#[test]
fn an_extends_cycle_is_a_hard_error_never_a_stack_overflow() {
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "cycle-a", &no_vars()).unwrap_err();
    assert!(err.to_string().contains("cycle"), "{err}");
}

#[test]
fn a_formula_that_inherits_drain_ONLY_from_its_parent_carries_it_in_the_merged_chain() {
    // ⭐ BD1 — this is what moves rung 2c from 57 to 49, and it is why the gate's
    // per-rung counts are what they are.
    //
    // `inherits-drain` declares NOTHING but `extends`. Camp resolves the chain at
    // stage 2 and validates the MERGED step list at stage 6, so the parent's
    // `drain` is camp's problem even though the child never typed the key: while
    // rung 2e was pending, this formula did not load. Seven corpus formulas
    // (`build-from-*`) inherit `drain` exactly this way, and one
    // (`github-issue-fix`) inherits `expand`/`expand_vars`.
    //
    // gc corroborates: the corpus AUTHORS 12 separate drain steps and gc COMPILES
    // 19 — the seven extra are inherited.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "inherits-drain", &no_vars())
        .expect("rung 2e has landed, so the inherited drain now compiles");
    let implement = compiled
        .formula
        .steps
        .iter()
        .find(|s| s.id == "implement")
        .expect("the parent's drain step is in the merged chain");
    assert!(
        implement.drain.is_some(),
        "the drain came from the PARENT — the child never typed the key"
    );
    // And it defaults exactly as gc's compiler defaults it.
    assert_eq!(implement.metadata["gc.kind"], "drain");
    assert_eq!(implement.metadata["gc.drain_member_access"], "read");
}

// ---- rung 2d: expansion, and the compile-stage {name} grammar --------------

#[test]
fn expand_replaces_the_target_step_with_the_expansion_formulas_template() {
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "expansion-host", &no_vars()).unwrap();
    // `review` is GONE — REPLACED, in position, by the expansion's template.
    // `{target}` resolved to the target step's id.
    assert_eq!(
        ids(&compiled),
        vec!["prepare", "review.gather", "publish"],
        "the target is replaced in position; the guarded child is pruned"
    );
}

#[test]
fn a_single_brace_var_in_step_metadata_resolves_AT_COMPILE() {
    // F4 — rev 2 had NO single-brace grammar at all and claimed "0 bare route
    // values". There are 8, all in `children.metadata.gc.run_target`, and
    // `superpowers-code-review.formula.toml:63` proves it: authored
    // `{implementation_target}`, and gc's compiled Recipe carries
    // `superpowers.implementer` — RESOLVED. Get the stages backwards and 55
    // routes silently corrupt.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "expansion-host", &no_vars()).unwrap();
    let gather = &compiled.formula.steps[1];
    assert_eq!(
        gather.metadata.get("gc.run_target").map(String::as_str),
        Some("gc.implementer"),
        "a SINGLE-brace route resolves at COMPILE"
    );
}

#[test]
fn a_BOUND_double_brace_var_inside_an_expansion_template_survives_expansion() {
    // ⭐ THE UNEXERCISED PATH, three revisions running: `{{x}}` inside an
    // expansion template where x IS BOUND. Every earlier fixture was a bare `{x}`
    // (resolves) or a `{{x}}` with x UNBOUND (survives for the WRONG REASON —
    // binding was doing the protecting, not staging). 52 real corpus instances,
    // and gc CORRUPTS every one of them.
    //
    // `implementation_target` HAS a default here, and the token must still come
    // out byte-identical.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "expansion-host", &no_vars()).unwrap();
    let gather = &compiled.formula.steps[1];
    assert!(
        gather
            .description
            .as_deref()
            .unwrap()
            .contains("{{implementation_target}}"),
        "a BOUND {{{{var}}}} inside an expansion template must survive byte-for-byte \
         (gc would have written `{{gc.implementer}}` here): {:?}",
        gather.description
    );
}

#[test]
fn a_double_brace_condition_inside_an_expansion_template_survives_expansion() {
    // gc EXEMPTS Condition from substituteVars (expand.go:272) with a source
    // comment naming this exact bug. All four `{{review_mode}} != report`
    // conditions live on the template/children tree — inside expandStep's reach.
    // Substitute them and `{{review_mode}} != report` becomes `{report} != report`,
    // which eval_condition REJECTS ⇒ the four code-review formulas stop loading
    // ⇒ THE CEILING IS NO LONGER 95.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "expansion-host", &no_vars())
        .expect("the condition must survive expansion intact and then PRUNE, not error");
    // review_mode defaults to "report" ⇒ the guarded child is PRUNED, and that is
    // a pruning, not a violation.
    assert!(!ids(&compiled).contains(&"review.apply-review-findings"));
    assert!(compiled.refusals.is_empty());
    assert!(
        compiled.formula.steps.iter().all(|s| !s.id.contains('{')),
        "no step id may still carry a brace: {:?}",
        ids(&compiled)
    );
}

#[test]
fn children_are_flattened_preserving_position() {
    // camp's step list has no nesting, where gc keeps children on the Step. With
    // review_mode flipped, the guarded child SURVIVES and must land right after
    // its parent.
    let c = camp();
    let overrides = BTreeMap::from([("review_mode".to_owned(), "agent".to_owned())]);
    let compiled = compile_named(&c.layers, &c.cfg, "expansion-host", &overrides).unwrap();
    assert_eq!(
        ids(&compiled),
        vec![
            "prepare",
            "review.gather",
            "review.apply-review-findings",
            "publish"
        ]
    );
}

#[test]
fn a_single_brace_token_in_description_file_is_NOT_resolved() {
    // 121 corpus asset files are named, ON DISK, literally `{target}.*.md`, and
    // 130 `description_file` values carry the braces. gc never substitutes there,
    // so it opens the LITERAL path. An implementer who "helpfully" applies the
    // {target} family to every field breaks 130 asset resolutions — each a hard
    // error in a graph.v2 formula ⇒ mass refusal ⇒ the ceiling collapses.
    //
    // The fixture asset is literally named `{target}.note.md`.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "brace-asset", &no_vars()).unwrap();
    assert_eq!(
        compiled.formula.steps[0].description.as_deref(),
        Some("Literal brace asset.\n"),
        "description_file opened the LITERAL `{{target}}.note.md` path"
    );
}

#[test]
fn an_expansion_formula_compiles_and_is_not_runnable() {
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "review-flow", &no_vars()).unwrap();
    assert!(!compiled.is_runnable());
    assert!(
        compiled.not_runnable.unwrap().reason.contains("expansion"),
        "an expansion formula supplies template steps; it is not directly runnable"
    );
}

#[test]
fn an_expansion_formula_with_no_template_is_a_violation() {
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "empty-expansion", &no_vars()).unwrap_err();
    assert!(err.names("template"), "{err}");
}

#[test]
fn an_expand_target_that_does_not_exist_is_a_hard_error() {
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "bad-expand", &no_vars()).unwrap_err();
    assert!(err.names("expand"), "{err}");
    assert!(err.to_string().contains("no-such-expansion"), "{err}");
}

// ---- rung 2e: drain (the compile side) — THE CEILING ----------------------

#[test]
fn the_corpus_build_formulas_compile_clean_because_the_shared_arm_IS_PRUNED() {
    // ⭐ THE LOAD-BEARING ONE (BD2). `bmad-build` / `gstack-build` /
    // `compound-build` each carry TWO drain steps on mutually exclusive
    // conditions. The default `drain_policy = "separate"` prunes the SHARED one —
    // AND ITS REFUSAL — before stage 6 ever collects it.
    //
    // gc corroborates: 13 authored shared drains compile to exactly 1.
    // Refuse at parse instead and 19 corpus formulas fail: the ceiling is 76, not 95.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "build-arms", &no_vars())
        .expect("the shared arm and its refusal are pruned together");
    assert_eq!(ids(&compiled), vec!["decompose", "implement"]);
    assert!(compiled.refusals.is_empty(), "{:?}", compiled.refusals);
}

#[test]
fn setting_drain_policy_to_same_session_refuses_instead_of_approximating() {
    // Flip the var and the shared arm SURVIVES — so its refusal survives too.
    // Camp refuses rather than silently running a same-session drain as separate.
    let c = camp();
    let overrides = BTreeMap::from([("drain_policy".to_owned(), "same-session".to_owned())]);
    let err = compile_named(&c.layers, &c.cfg, "build-arms", &overrides).unwrap_err();
    assert!(err.names("context"), "{err}");
    assert!(err.to_string().contains("same-session"), "{err}");
}

#[test]
fn an_UNCONDITIONAL_shared_drain_is_refused_and_nothing_can_prune_it() {
    // `gascity/formulas/same-session-implement.formula.toml` — one of the 5 camp
    // cannot load, and one §9 did not anticipate. 12 of the 13 corpus shared
    // drains sit behind a condition; this one has none.
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "unconditional-shared", &no_vars()).unwrap_err();
    assert!(err.names("context"), "{err}");
    assert_eq!(err.refusals.len(), 1);
    assert_eq!(err.refusals[0].key, "context");
    assert_eq!(err.refusals[0].step.as_deref(), Some("implement"));
}

#[test]
fn a_drain_step_compiles_to_gcs_gc_drain_metadata() {
    // F2/F3 — gc's compiled Recipe has NO Drain struct: a drain lives entirely in
    // METADATA. Assertion B of the differential gate diffs this map against gc's
    // own 20 drain steps.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "build-arms", &no_vars()).unwrap();
    let md = &compiled.formula.steps[1].metadata;
    assert_eq!(md["gc.kind"], "drain");
    assert_eq!(md["gc.drain_context"], "separate");
    assert_eq!(md["gc.drain_formula"], "item");
    assert_eq!(md["gc.drain_member_access"], "exclusive");
    // DEFAULTED by the compiler — the author wrote no `on_item_failure`.
    assert_eq!(md["gc.drain_on_item_failure"], "continue");
    assert!(!md.contains_key("gc.drain_item_single_lane"));
}

#[test]
fn drain_defaults_follow_gcs_compiler() {
    use camp_core::formula::{MemberAccess, OnItemFailure};
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "drain-no-needs", &no_vars());
    // (that fixture fails S17; compile the arms one instead for the defaults)
    drop(compiled);
    let compiled = compile_named(&c.layers, &c.cfg, "build-arms", &no_vars()).unwrap();
    let d = compiled.formula.steps[1].drain.as_ref().unwrap();
    assert_eq!(d.member_access, MemberAccess::Exclusive);
    assert_eq!(
        d.on_item_failure,
        OnItemFailure::Continue,
        "gc defaults a SEPARATE drain to `continue` (skip_remaining is the shared \
         context camp refuses)"
    );
    // F5/F6: parsed, round-tripped, and NEVER READ at runtime — because gc never
    // reads them either.
    assert!(!d.item.single_lane);
}

#[test]
fn a_drain_step_with_no_needs_is_a_violation() {
    // S17. Without a `needs` the anchor is claimed at cook time, before any member
    // exists: it scatters zero members and gathers `pass` immediately. Every
    // corpus drain has `needs`.
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "drain-no-needs", &no_vars()).unwrap_err();
    assert!(err.names("steps.implement.needs"), "{err}");
    assert!(err.to_string().contains("creates its members"), "{err}");
}

#[test]
fn a_templated_drain_formula_is_rejected_at_validation() {
    // F8 — gc's OWN rule (graphv2_validation.go:417-419). §9 said `drain.formula`
    // was EXEMPT from `{{var}}` substitution; it is not — gc substitutes it into
    // `gc.drain_formula`. It is blocked by a VALIDATION reject instead, exactly as
    // gc blocks it.
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "templated-drain", &no_vars()).unwrap_err();
    assert!(err.to_string().contains("templated item formula"), "{err}");
}

#[test]
fn continuation_group_and_max_units_are_refused_by_name() {
    let c = camp();
    let err = compile_named(&c.layers, &c.cfg, "drain-refused-keys", &no_vars()).unwrap_err();
    assert!(err.names("continuation_group"), "{err}");
    assert!(err.names("max_units"), "{err}");
}

#[test]
fn the_metadata_key_gc_continuation_group_is_ACCEPTED_and_carried() {
    // 29 authored uses, 14 surviving compilation — and it is a DIFFERENT THING
    // from the `drain.continuation_group` KEY (0 uses), which is refused. Rev 2
    // conflated them. Camp carries this one verbatim and does not honour it
    // (§11.4) — a NAMED fidelity cost.
    let c = camp();
    let compiled = compile_named(&c.layers, &c.cfg, "base", &no_vars()).unwrap();
    assert_eq!(
        compiled.formula.steps[0]
            .metadata
            .get("gc.build.artifact_schema")
            .map(String::as_str),
        Some("x"),
        "an accepted-but-unhonoured annotation rides through untouched"
    );
}
