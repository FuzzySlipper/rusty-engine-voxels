use std::path::{Path, PathBuf};

use rusty_engine_voxels::kit::load_kit;
use rusty_engine_voxels::pose::{
    evaluate_node_poses, rasterize_part, RasterSettings, RigMap, RigidTransform,
};
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";
const RIFLEMAN_RIG_MAP: &str = "content/characters/rifleman/rig-map.json";
const RETRO_GLB: &str = "content/sources/kenney-retro-character/character-medium.glb";

fn import_retro() -> voxel_convert::ImportedAnimatedMeshSource {
    let bytes = std::fs::read(root().join(RETRO_GLB)).expect("read retro glb");
    import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: "mesh-animation/retro-character".to_owned(),
        asset_version: 1,
        source_path: RETRO_GLB.to_owned(),
        format: MeshSourceFormat::Glb,
        source_bytes: bytes,
        expected_source_sha256: None,
        mesh_primitive: None,
    })
    .expect("import retro character")
}

fn load_rig_map() -> RigMap {
    let text = std::fs::read_to_string(root().join(RIFLEMAN_RIG_MAP)).expect("read rig map");
    serde_json::from_str(&text).expect("parse rig map")
}

#[test]
fn rig_map_validates_against_kit_and_model() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    rig_map
        .validate(&kit, &imported.model)
        .expect("checked rig map should validate against the kit and the retro-character model");

    // Every part is bound exactly once, to a real deform bone.
    assert_eq!(rig_map.bindings.len(), kit.parts.len());
}

#[test]
fn pose_evaluation_produces_stable_world_transforms() {
    let imported = import_retro();
    // The `run` clip (index 1) at time 0 vs a later time: deterministic and
    // non-degenerate.
    let at_zero = evaluate_node_poses(&imported.model, 1, 0).expect("pose at t=0");
    let again = evaluate_node_poses(&imported.model, 1, 0).expect("pose at t=0 again");
    assert_eq!(at_zero.len(), again.len());
    assert!(!at_zero.is_empty());
    // Deterministic.
    for (node, transform) in &at_zero {
        let other = &again[node];
        assert_eq!(transform.rotation, other.rotation);
        assert_eq!(transform.translation, other.translation);
    }
    // Some node actually moves between two different times in the run cycle.
    let later = evaluate_node_poses(&imported.model, 1, 333_333).expect("pose mid-run");
    let moved = at_zero.iter().any(|(node, a)| {
        let b = &later[node];
        (0..3).any(|axis| (a.translation[axis] - b.translation[axis]).abs() > 1e-6)
            || (0..4).any(|axis| (a.rotation[axis] - b.rotation[axis]).abs() > 1e-6)
    });
    assert!(
        moved,
        "run clip should move at least one bone between t=0 and mid-clip"
    );
}

#[test]
fn rigid_parts_are_stable_across_poses_and_moving_parts_move() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    rig_map.validate(&kit, &imported.model).expect("rig map");

    let settings = RasterSettings {
        supersample: 2,
        occupancy_threshold: 0.3,
    };
    // Find the clip index for `run`.
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip present");

    let pose_a = evaluate_node_poses(&imported.model, run_index, 0).expect("pose a");
    let pose_b = evaluate_node_poses(&imported.model, run_index, 333_333).expect("pose b");

    // For each bound part, rasterize under the bone's world transform at both
    // times and compare part cell sets.
    let mut moving_parts = 0usize;
    for binding in &rig_map.bindings {
        let part = kit.part(&binding.part_id).expect("part in kit");
        let bone_a = pose_a
            .get(&binding.bone_node_index)
            .copied()
            .unwrap_or(RigidTransform::IDENTITY);
        let bone_b = pose_b
            .get(&binding.bone_node_index)
            .copied()
            .unwrap_or(RigidTransform::IDENTITY);

        let cells_a = rasterize_part(part, bone_a, &settings).expect("rasterize a");
        let cells_b = rasterize_part(part, bone_b, &settings).expect("rasterize b");

        // Every part produces coherent, non-empty geometry at both poses.
        assert!(
            !cells_a.is_empty(),
            "{} should rasterize at pose a",
            binding.part_id
        );
        assert!(
            !cells_b.is_empty(),
            "{} should rasterize at pose b",
            binding.part_id
        );
        // Rigid rasterization is volume-stable (within a small tolerance for
        // rotation edge effects): a rigid part cannot gain or lose most of its
        // volume between poses.
        let volume_ratio = cells_b.len() as f64 / cells_a.len().max(1) as f64;
        assert!(
            (0.5..=2.0).contains(&volume_ratio),
            "{} volume should be stable across poses ({} -> {})",
            binding.part_id,
            cells_a.len(),
            cells_b.len()
        );

        let set_a: std::collections::BTreeSet<_> = cells_a.iter().map(|c| c.coordinate).collect();
        let set_b: std::collections::BTreeSet<_> = cells_b.iter().map(|c| c.coordinate).collect();
        if set_a != set_b {
            moving_parts += 1;
        }
    }

    // In a run cycle, several parts (legs, arms) must move; the head/torso may
    // be comparatively stable. We require that *some* parts move — proving the
    // rigid pipeline actually articulates — without demanding everything churn.
    assert!(
        moving_parts >= 2,
        "a run cycle should articulate multiple rigid parts, got {moving_parts}"
    );
}

#[test]
fn unmoved_part_has_zero_churn_under_identity() {
    // Direct churn claim at part granularity: rasterize the same part under the
    // same transform twice and confirm zero cell churn.
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let part = kit.part("head").expect("head part");
    let settings = RasterSettings::default();
    let transform = RigidTransform {
        rotation: [0.0, 0.0, 0.0, 1.0],
        translation: [0.0, 30.0, 0.0],
    };
    let a = rasterize_part(part, transform, &settings).expect("first");
    let b = rasterize_part(part, transform, &settings).expect("second");
    let set_a: std::collections::BTreeSet<_> = a.iter().map(|c| c.coordinate).collect();
    let set_b: std::collections::BTreeSet<_> = b.iter().map(|c| c.coordinate).collect();
    let churn = set_a.symmetric_difference(&set_b).count();
    assert_eq!(churn, 0, "identical part + transform must have zero churn");
}
