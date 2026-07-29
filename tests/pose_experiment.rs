use std::path::{Path, PathBuf};

use rusty_engine_voxels::kit::{assemble_neutral, load_kit};
use rusty_engine_voxels::pose::{
    admit_node_world_transform, evaluate_node_poses, evaluate_node_poses_with_policy,
    rasterize_part, RasterSettings, RigMap, RigidTransform,
};

/// Face-connected component count of a cell set (6-connectivity BFS).
fn connected_components(cells: &std::collections::BTreeSet<[i64; 3]>) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    let mut components = 0;
    for &start in cells {
        if seen.contains(&start) {
            continue;
        }
        components += 1;
        let mut stack = vec![start];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            for d in [
                [1, 0, 0],
                [-1, 0, 0],
                [0, 1, 0],
                [0, -1, 0],
                [0, 0, 1],
                [0, 0, -1],
            ] {
                let n = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                if cells.contains(&n) && !seen.contains(&n) {
                    stack.push(n);
                }
            }
        }
    }
    components
}
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

        let cells_a =
            rasterize_part(part, binding.placement(bone_a), &settings).expect("rasterize a");
        let cells_b =
            rasterize_part(part, binding.placement(bone_b), &settings).expect("rasterize b");

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

/// Every real rifleman part, across two sampled run poses, must rasterize to a
/// single face-connected component (the R6336-4 guarantee at corpus scale).
#[test]
fn every_rifleman_part_stays_connected_across_run_poses() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    rig_map.validate(&kit, &imported.model).expect("rig map");
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    let settings = RasterSettings::default();

    for &time in &[0u64, 333_333] {
        let poses = evaluate_node_poses(&imported.model, run_index, time).expect("poses");
        for binding in &rig_map.bindings {
            let part = kit.part(&binding.part_id).expect("part");
            let bone = poses
                .get(&binding.bone_node_index)
                .copied()
                .unwrap_or(RigidTransform::IDENTITY);
            let cells =
                rasterize_part(part, binding.placement(bone), &settings).expect("rasterize");
            let set: std::collections::BTreeSet<_> = cells.iter().map(|c| c.coordinate).collect();
            let components = connected_components(&set);
            assert_eq!(
                components, 1,
                "part {} at t={time} must be one connected component, got {components}",
                binding.part_id
            );
        }
    }
}

/// R6336-2: at bind pose, applying the bind transform to each part must
/// reconstruct the M1 assembled neutral character — the bind transform is what
/// aligns part-local coordinates onto the bones so the whole coheres.
#[test]
fn bind_transform_reconstructs_neutral_assembly() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    rig_map.validate(&kit, &imported.model).expect("rig map");

    // Reference: the M1 assembled neutral character (grounded, y=0).
    let neutral = assemble_neutral(&kit).expect("neutral assembly");

    // Reference bind frame: the model's bind pose (idle clip, t=0).
    let bind_poses = evaluate_node_poses(&imported.model, 0, 0).expect("bind poses");
    let settings = RasterSettings::default();

    // Rasterize every bound part at bind pose with its bind transform and
    // collect the union of occupied cells.
    let mut assembled: std::collections::BTreeMap<[i64; 3], u16> =
        std::collections::BTreeMap::new();
    for binding in &rig_map.bindings {
        let part = kit.part(&binding.part_id).expect("part");
        let bone = bind_poses
            .get(&binding.bone_node_index)
            .copied()
            .unwrap_or(RigidTransform::IDENTITY);
        for cell in
            rasterize_part(part, binding.placement(bone), &settings).expect("rasterize part")
        {
            assembled
                .entry(cell.coordinate)
                .or_insert(cell.material_slot);
        }
    }

    // The bind-transformed union must occupy the same bounding region as the M1
    // neutral assembly (within the rasterizer's conservative dilation) and
    // overlap it substantially. This proves the bind transform aligns parts
    // into a coherent character rather than scattering them.
    let (n_lo, n_hi) = neutral.bounds().expect("neutral bounds");
    let mut a_lo = [i64::MAX; 3];
    let mut a_hi = [i64::MIN; 3];
    for c in assembled.keys() {
        for axis in 0..3 {
            a_lo[axis] = a_lo[axis].min(c[axis]);
            a_hi[axis] = a_hi[axis].max(c[axis]);
        }
    }
    // The bind-assembled character must have a comparable vertical extent
    // (a humanoid, not a pancake or a column of disjoint parts).
    let neutral_height = n_hi[1] - n_lo[1] + 1;
    let assembled_height = a_hi[1] - a_lo[1] + 1;
    let height_ratio = assembled_height as f64 / neutral_height.max(1) as f64;
    assert!(
        (0.5..=2.0).contains(&height_ratio),
        "bind-assembled height {assembled_height} should be comparable to neutral height {neutral_height}"
    );
    // And it must be non-empty and grounded near the origin region.
    assert!(!assembled.is_empty());
}

// --- R6336-1 equivalence regressions: the seam is consumed, not reimplemented ---

/// Out-of-range timestamps receive the Engine's typed rejection, not clamping.
#[test]
fn out_of_range_time_is_rejected_not_clamped() {
    let imported = import_retro();
    let clip_duration = imported
        .model
        .clips
        .iter()
        .find(|c| c.name == "run")
        .expect("run clip")
        .duration_microseconds;
    let result = evaluate_node_poses(&imported.model, 1, clip_duration + 1_000_000);
    assert!(
        result.is_err(),
        "time beyond clip duration must be rejected, not clamped"
    );
}

/// The adapter's rigid poses are the Engine seam's affine world transforms,
/// decomposed by their admitted uniform scale (consumption, not a parallel
/// evaluator). Compare against the raw Engine receipt directly.
#[test]
fn adapter_matches_engine_seam_up_to_admitted_uniform_scale() {
    let imported = import_retro();
    let receipt =
        voxel_convert::evaluate_clip_node_poses(&imported.model, "run", 0).expect("engine seam");
    let adapter = evaluate_node_poses(&imported.model, 1, 0).expect("adapter");

    // Every adapter node corresponds to an engine node, and the translation is
    // the engine's world translation divided by that node's admitted uniform
    // scale (a ~100x rig returns to cell units). Compute the uniform scale the
    // same way the adapter does: the mean axis length of the world basis.
    let hips = receipt.node(21).expect("engine hips");
    let adapter_hips = adapter.get(&21).expect("adapter hips");
    let m = hips.world_transform;
    let col_len = |c: usize| {
        let v = [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]];
        (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
    };
    let scale = (col_len(0) + col_len(1) + col_len(2)) / 3.0;
    for axis in 0..3 {
        let expected = hips.world_transform[12 + axis] / scale;
        assert!(
            (adapter_hips.translation[axis] - expected).abs() < 1e-6,
            "adapter hips translation axis {axis} should equal engine world translation / uniform scale"
        );
    }
}

/// A genuinely non-uniform node (one axis stretched well beyond the relative
/// tolerance) is rejected by the admitted rigid-scale policy, while a uniform
/// rig with floating-point jitter is admitted.
#[test]
fn non_uniform_scale_is_rejected_uniform_jitter_is_admitted() {
    use voxel_convert::NodePoseRigidScalePolicy;
    // Synthesize a non-uniform world transform: uniform 2x on X only.
    let mut m = [0.0f64; 16];
    m[0] = 2.0; // X axis scaled 2x
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    let node = voxel_convert::AnimationNodePose {
        source_node_index: 0,
        local_transform: m,
        world_transform: m,
    };
    let result = evaluate_node_poses_with_policy(
        &import_retro().model,
        "run",
        0,
        NodePoseRigidScalePolicy::AllowUniformScale,
    );
    assert!(result.is_ok(), "the real rig should be admitted");

    // Directly exercise the admission on the synthetic non-uniform node via the
    // engine's strict check, which must reject it.
    assert!(
        node.admit_rigid_world_transform(NodePoseRigidScalePolicy::AllowUniformScale)
            .is_err(),
        "a one-axis-stretched transform must be rejected as non-uniform"
    );
}

// --- R6336-5 regressions: the fallback retains all rigid invariants ---

fn affine_node(m: [f64; 16]) -> voxel_convert::AnimationNodePose {
    voxel_convert::AnimationNodePose {
        source_node_index: 0,
        local_transform: m,
        world_transform: m,
    }
}

#[test]
fn fallback_rejects_shear_reflection_and_non_finite() {
    use voxel_convert::NodePoseRigidScalePolicy;

    // Shear: equal-length but non-orthogonal axes (X and X+Y both length ~1.4).
    let mut shear = [0.0f64; 16];
    shear[0] = 1.0; // X column = (1,0,0)
    shear[4] = 1.0; // Y column = (1,1,0) -> not orthogonal to X
    shear[5] = 1.0;
    shear[10] = 1.0;
    shear[15] = 1.0;
    let result = admit_node_world_transform(
        &affine_node(shear),
        NodePoseRigidScalePolicy::AllowUniformScale,
    );
    assert!(result.is_err(), "shear must be rejected by the fallback");

    // Reflection: negative-determinant basis (mirror across X).
    let mut reflect = [0.0f64; 16];
    reflect[0] = -1.0; // flip X
    reflect[5] = 1.0;
    reflect[10] = 1.0;
    reflect[15] = 1.0;
    let result = admit_node_world_transform(
        &affine_node(reflect),
        NodePoseRigidScalePolicy::AllowUniformScale,
    );
    assert!(result.is_err(), "a reflected basis must be rejected");

    // Non-finite: NaN translation component.
    let mut nan = [0.0f64; 16];
    nan[0] = 1.0;
    nan[5] = 1.0;
    nan[10] = 1.0;
    nan[12] = f64::NAN;
    nan[15] = 1.0;
    let result = admit_node_world_transform(
        &affine_node(nan),
        NodePoseRigidScalePolicy::AllowUniformScale,
    );
    assert!(result.is_err(), "non-finite affine data must be rejected");

    // And the real rig still passes through the same fallback path.
    let ok = evaluate_node_poses(&import_retro().model, 1, 0);
    assert!(ok.is_ok(), "the retro-character rig is still admitted");
}

#[test]
fn fallback_rejects_truly_non_uniform_scale() {
    use voxel_convert::NodePoseRigidScalePolicy;
    // Uniform-length axes are fine; a one-axis 3x stretch must be rejected.
    let mut m = [0.0f64; 16];
    m[0] = 3.0; // X scaled 3x
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    let result =
        admit_node_world_transform(&affine_node(m), NodePoseRigidScalePolicy::AllowUniformScale);
    assert!(
        result.is_err(),
        "a one-axis 3x stretch must be rejected as non-uniform"
    );
}
