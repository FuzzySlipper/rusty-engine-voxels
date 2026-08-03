//! Mesh→kit bake integration test: the checked knight kit-spec regenerates
//! the checked kit deterministically, and the kit validates and assembles
//! under the existing M1 gates.

use std::path::{Path, PathBuf};

use rusty_engine_voxels::kit::{assemble_neutral, load_kit};
use rusty_engine_voxels::kit_bake::{run_kit_bake, write_kit_bake_output};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const KNIGHT_SPEC: &str = "content/characters/knight/kit-spec.json";
const KNIGHT_KIT: &str = "content/characters/knight/character.json";
const KNIGHT_REPORT: &str = "evidence/kit-bake-knight.json";

#[test]
fn knight_bake_regenerates_the_checked_kit_deterministically() {
    let first = run_kit_bake(&root(), KNIGHT_SPEC).expect("knight bake runs");
    let second = run_kit_bake(&root(), KNIGHT_SPEC).expect("knight bake reruns");
    assert_eq!(first.kit_json, second.kit_json, "bake is deterministic");
    assert_eq!(
        first.evidence.assembly.fingerprint, second.evidence.assembly.fingerprint,
        "assembly fingerprint is deterministic"
    );

    // The emitted kit matches the checked-in kit byte-for-byte.
    let checked = std::fs::read_to_string(root().join(KNIGHT_KIT)).expect("checked kit reads");
    assert_eq!(
        first.kit_json, checked,
        "regenerating the knight kit from its spec reproduces the checked document"
    );

    // Writing the same output again is idempotent.
    write_kit_bake_output(&root(), KNIGHT_KIT, KNIGHT_REPORT, &first).expect("output writes");
}

#[test]
fn knight_kit_validates_and_assembles_under_m1_gates() {
    let kit = load_kit(&root(), KNIGHT_KIT).expect("checked knight kit validates");
    assert_eq!(kit.parts.len(), 11);
    assert_eq!(kit.total_cells(), 167_962);

    let assembled = assemble_neutral(&kit).expect("neutral assembly");
    assert_eq!(assembled.len(), 167_962);
    assert_eq!(assembled.fingerprint(), 16_955_304_374_580_601_702);

    // Identity gates from the spec: grounded at y = 0, protected parts
    // present, and the character is roughly the expected height (292 cells at
    // ~6.3 mm cells ≈ 1.85 m).
    let (min, max) = assembled.bounds().expect("assembly bounds");
    assert_eq!(min[1], 0);
    assert_eq!(max[1] - min[1] + 1, 292);
    for protected in ["helmet", "torso", "sword"] {
        assert!(
            kit.part(protected)
                .is_some_and(|part| !part.cells.is_empty()),
            "protected part {protected} is present"
        );
    }

    // Every limb part satisfies the declared minimum limb thickness (the kit
    // validation enforces this against the thinnest bounding dimension).
    for part in kit.parts.iter().filter(|part| part.limb) {
        let Some((lo, hi)) = part.cells.iter().map(|cell| cell.coordinate).fold(
            None,
            |bounds: Option<([i64; 3], [i64; 3])>, c| {
                Some(match bounds {
                    None => (c, c),
                    Some((mut lo, mut hi)) => {
                        for axis in 0..3 {
                            lo[axis] = lo[axis].min(c[axis]);
                            hi[axis] = hi[axis].max(c[axis]);
                        }
                        (lo, hi)
                    }
                })
            },
        ) else {
            panic!("limb part {} has no cells", part.id);
        };
        let thinnest = (0..3).map(|axis| hi[axis] - lo[axis] + 1).min().unwrap();
        assert!(
            thinnest >= i64::from(kit.invariants.min_limb_thickness),
            "limb part {} is too thin: {thinnest}",
            part.id
        );
    }
}
