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
            "[camp]\nname = \"t\"\n\n[imports.gc]\nsource = {:?}\n",
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
