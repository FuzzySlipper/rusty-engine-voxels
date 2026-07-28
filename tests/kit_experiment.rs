use std::path::{Path, PathBuf};

use rusty_engine_voxels::kit::{assemble_neutral, load_kit, KitError, VoxelOrigin};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";

#[test]
fn rifleman_kit_validates_and_assembles_deterministically() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("checked rifleman kit should validate");

    // 13 parts: torso, head, pelvis, 2x(upper+lower arm/leg), rifle, backpack.
    assert_eq!(kit.parts.len(), 13);
    assert!(kit.total_cells() > 1_200);

    let first = assemble_neutral(&kit).expect("neutral assembly should succeed");
    let second = assemble_neutral(&kit).expect("re-assembly should succeed");

    // Regeneration is content-stable.
    assert_eq!(first, second);
    assert_eq!(first.fingerprint(), second.fingerprint());

    // The assembled character is coherent: occupied, bounded, grounded.
    assert!(first.len() > 1_000);
    let (lo, hi) = first.bounds().expect("assembled character has bounds");
    assert!(hi[1] > lo[1], "character has vertical extent");
    // Feet should reach the ground plane.
    assert!(lo[1] <= kit.convention.ground_y);

    // Every assembled voxel traces to a canonical part (no synthetic origins in M1).
    assert!(first
        .voxels
        .values()
        .all(|voxel| matches!(voxel.origin, VoxelOrigin::Canonical(_))));

    // The head sits above the torso, which sits above the pelvis (neutral pose
    // sanity, in assembly coordinates).
    let centroid_y = |slot: u16| -> f64 {
        let (mut sum, mut count) = (0.0f64, 0usize);
        for voxel in first.voxels.values() {
            if voxel.material_slot == slot {
                sum += voxel.coordinate[1] as f64;
                count += 1;
            }
        }
        if count == 0 {
            0.0
        } else {
            sum / count as f64
        }
    };
    // skin (1, head/forearms) centroid above coat (2, torso) above trouser (3, legs).
    assert!(centroid_y(1) > centroid_y(2));
    assert!(centroid_y(2) > centroid_y(3));
}

#[test]
fn malformed_kit_is_rejected_with_actionable_error() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit should load");
    // Break a mate reference and confirm validation fails with a useful message.
    let mut broken = kit.clone();
    broken.parts[0].sockets[0].mate = Some("nonexistent.socket".to_owned());
    let result = broken.validate();
    match result {
        Err(KitError::Validation(message)) => {
            assert!(
                message.contains("nonexistent"),
                "error should name the mate: {message}"
            );
        }
        other => panic!("expected validation error, got {other:?}"),
    }
}

#[test]
fn regenerated_neutral_matches_checked_fingerprint() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit should load");
    let frame = assemble_neutral(&kit).expect("assembly should succeed");
    // Pin the regeneration fingerprint so accidental changes to parts, sockets,
    // or assembly order are caught. Update deliberately when the character is
    // intentionally revised (and bump the affected part versions).
    assert_eq!(frame.fingerprint(), RIFLEMAN_NEUTRAL_FINGERPRINT);
}

/// The checked rifleman neutral-assembly fingerprint (see
/// `AssembledFrame::fingerprint`). Derived from the checked corpus; an
/// intentional character revision must update this value and bump the affected
/// part versions.
const RIFLEMAN_NEUTRAL_FINGERPRINT: u64 = 0x2ced_8030_1cc5_6d5e;
