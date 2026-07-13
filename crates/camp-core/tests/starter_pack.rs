//! The real `packs/starter` is a valid Gas City directory pack (compat §5.1).

use std::path::Path;

use camp_core::import::manifest::read_manifest;
use camp_core::pack::parse_agent_dir;

#[test]
fn starter_pack_is_a_valid_directory_pack() {
    let starter = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs/starter");
    let m = read_manifest(&starter).unwrap();
    assert_eq!(m.pack.name, "starter");
    assert!(
        starter.join("agents/dev/prompt.md").exists()
            || starter.join("agents/dev/prompt.template.md").exists()
    );
    let (agent, refusals) = parse_agent_dir(&starter.join("agents/dev")).unwrap();
    assert_eq!(agent.name, "dev");
    assert!(refusals.is_empty());
    assert!(starter.join("orders").is_dir());
    assert!(starter.join("formulas/guarded-change.toml").exists());
}
