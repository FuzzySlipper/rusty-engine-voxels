use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusty_engine_voxels::assemble::{
    assemble_rough_schedule, select_pose_schedule, socket_constrained_part_placements,
    DiscardedCanonicalVoxel, PoseSelectionSettings,
};
use rusty_engine_voxels::fusion::{
    fuse_rough_frame, fuse_rough_schedule, FusedVoxelOrigin, FusionContext, FusionSettings,
};
use rusty_engine_voxels::kit::load_kit;
use rusty_engine_voxels::pose::{RasterSettings, RigMap};
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";
const RIFLEMAN_RIG_MAP: &str = "content/characters/rifleman/rig-map.json";
const RETRO_GLB: &str = "content/sources/kenney-retro-character/character-medium.glb";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

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
fn run_fusion_is_deterministic_structural_and_joint_local() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .expect("run clip");
    let schedule = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings::default(),
    )
    .expect("schedule");
    for pose in &schedule {
        let placements = socket_constrained_part_placements(
            &kit,
            &rig_map,
            &imported.model,
            clip_index,
            pose.time_microseconds,
        )
        .expect("socket-constrained placements");
        for child in &kit.parts {
            for socket in &child.sockets {
                let Some(mate) = socket.mate.as_deref() else {
                    continue;
                };
                let (parent_id, parent_socket_id) = mate.split_once('.').expect("validated mate");
                let parent = kit.part(parent_id).expect("validated parent");
                let parent_socket = parent.socket(parent_socket_id).expect("validated socket");
                let child_center = placements[&child.id].apply(socket.position);
                let parent_center = placements[parent_id].apply(parent_socket.position);
                assert!(
                    (0..3)
                        .all(|axis| { (child_center[axis] - parent_center[axis]).abs() <= 1.0e-6 }),
                    "{}.{} must remain attached to {mate}",
                    child.id,
                    socket.id
                );
            }
        }
    }
    let raster_settings = RasterSettings::default();
    let rough = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &schedule,
        &raster_settings,
    )
    .expect("rough schedule");
    let context = FusionContext {
        kit: &kit,
        rig_map: &rig_map,
        model: &imported.model,
        clip_index,
        raster_settings: &raster_settings,
    };
    let fused = fuse_rough_schedule(context, &schedule, &rough, FusionSettings::default())
        .expect("fused schedule");
    let repeated =
        fuse_rough_schedule(context, &schedule, &rough, FusionSettings::default()).expect("repeat");
    assert_eq!(fused, repeated, "fusion must be bit-deterministic");
    assert_eq!(
        serde_json::to_vec(&fused).expect("serialize"),
        serde_json::to_vec(&repeated).expect("serialize")
    );
    let mut spatial_churn = Vec::new();
    for pair in fused.windows(2) {
        let left: BTreeSet<_> = pair[0].voxels.iter().map(|cell| cell.coordinate).collect();
        let right: BTreeSet<_> = pair[1].voxels.iter().map(|cell| cell.coordinate).collect();
        spatial_churn.push(
            left.symmetric_difference(&right).count() as f64 / left.union(&right).count() as f64,
        );
    }
    let average_spatial_churn = spatial_churn.iter().sum::<f64>() / spatial_churn.len() as f64;
    let baseline: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("evidence/churn-study-high-fidelity.json"))
            .expect("baseline evidence"),
    )
    .expect("baseline JSON");
    let baseline_run_churn = baseline["clips"]
        .as_array()
        .expect("clips")
        .iter()
        .find(|clip| clip["clipId"] == "clip/run")
        .and_then(|clip| clip["averageChurnFraction"].as_f64())
        .expect("run baseline");
    assert!(
        average_spatial_churn < baseline_run_churn,
        "canonical-part run churn {average_spatial_churn} must beat straight-pipeline baseline {baseline_run_churn}"
    );
    let generated: Vec<usize> = fused.iter().map(|frame| frame.generated_voxels).collect();
    let report = serde_json::json!({
        "schemaVersion": 1,
        "clip": "run",
        "selectedFrames": fused.len(),
        "straightPipelineBaseline": {
            "artifact": "evidence/churn-study-high-fidelity.json",
            "storedFrames": 4,
            "averageSpatialChurnFraction": round_six(baseline_run_churn)
        },
        "canonicalPartsFirstPass": {
            "averageSpatialChurnFraction": round_six(average_spatial_churn),
            "improvementFraction": round_six(1.0 - average_spatial_churn / baseline_run_churn),
            "generatedVoxelMinimum": generated.iter().copied().min().unwrap_or(0),
            "generatedVoxelMaximum": generated.iter().copied().max().unwrap_or(0),
            "generatedVoxelAverage": round_six(
                generated.iter().sum::<usize>() as f64 / generated.len() as f64
            ),
            "allGeneratedVoxelsWithinFourCellsOfMarkedSeam": true
        },
        "interpretationLimits": [
            "The straight and canonical-parts schedules retain different frame counts; average occupied-coordinate symmetric difference remains useful directional evidence, not a controlled visual-quality score.",
            "Spatial churn includes intentional rigid motion as well as aliasing. Joint locality is separately proven from generated-operation provenance and M2 seam markers.",
            "This is deterministic authoring evidence for the checked rifleman/run corpus, not a runtime performance measurement."
        ]
    });
    let checked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("evidence/joint-fusion-study.json"))
            .expect("checked fusion evidence"),
    )
    .expect("checked fusion JSON");
    assert_eq!(report, checked, "checked fusion evidence drifted");

    for (rough, fused) in rough.iter().zip(&fused) {
        assert!(fused.generated_voxels > 0, "joints should add geometry");
        assert_eq!(
            fused
                .voxels
                .iter()
                .map(|cell| cell.coordinate)
                .collect::<BTreeSet<_>>()
                .len(),
            fused.voxels.len(),
            "coordinates are unique"
        );
        assert_eq!(
            fused.discarded_origins.len(),
            rough.discarded_overlaps.len(),
            "all overlap losers remain diagnostic provenance"
        );
        assert!(
            fused
                .voxels
                .iter()
                .filter(|cell| matches!(cell.origin, FusedVoxelOrigin::Generated { .. }))
                .all(|cell| !cell.operations.is_empty()),
            "every generated voxel names its cleanup/fusion operation"
        );
        let seam: Vec<[i64; 3]> = rough
            .voxels
            .iter()
            .filter(|cell| cell.needs_fusion)
            .map(|cell| cell.coordinate)
            .collect();
        for generated in fused
            .voxels
            .iter()
            .filter(|cell| matches!(cell.origin, FusedVoxelOrigin::Generated { .. }))
        {
            assert!(
                seam.iter().any(|coordinate| {
                    (0..3)
                        .map(|axis| coordinate[axis].abs_diff(generated.coordinate[axis]))
                        .max()
                        .unwrap_or(0)
                        <= 4
                }),
                "generated cell {:?} must remain joint-local",
                generated.coordinate
            );
        }
    }
}

fn round_six(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

#[test]
fn protected_origin_loss_is_a_typed_hard_failure() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "idle")
        .expect("idle clip");
    let schedule = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings::default(),
    )
    .expect("schedule");
    let raster_settings = RasterSettings::default();
    let mut rough = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &schedule,
        &raster_settings,
    )
    .expect("rough schedule")
    .remove(0);
    let protected_part = kit
        .parts
        .iter()
        .position(|part| part.id == "head")
        .expect("head") as u32;
    let removed_index = rough
        .voxels
        .iter()
        .position(|cell| cell.part_id == protected_part)
        .expect("head cell");
    let removed = rough.voxels.remove(removed_index);
    let context = FusionContext {
        kit: &kit,
        rig_map: &rig_map,
        model: &imported.model,
        clip_index,
        raster_settings: &raster_settings,
    };

    let error = fuse_rough_frame(context, &schedule[0], &rough, FusionSettings::default())
        .expect_err("protected provenance loss must reject");
    assert_eq!(error.code(), "fusion.protectedRegionRemoved");

    rough.discarded_overlaps.push(DiscardedCanonicalVoxel {
        coordinate: removed.coordinate,
        part_id: removed.part_id,
        source_voxel_index: removed.source_voxel_index,
        material_slot: removed.material_slot,
        winner_part_id: removed.part_id,
        winner_source_voxel_index: removed.source_voxel_index,
    });
    let error = fuse_rough_frame(context, &schedule[0], &rough, FusionSettings::default())
        .expect_err("forged diagnostics cannot authorize protected provenance loss");
    assert_eq!(error.code(), "fusion.overlapLedgerMismatch");
}

#[test]
fn overlap_ledger_rejects_wrong_duplicate_and_missing_records() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .expect("run clip");
    let schedule = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings::default(),
    )
    .expect("schedule");
    let raster_settings = RasterSettings::default();
    let rough_schedule = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &schedule,
        &raster_settings,
    )
    .expect("rough schedule");
    let frame_index = rough_schedule
        .iter()
        .position(|frame| !frame.discarded_overlaps.is_empty())
        .expect("real corpus has overlap diagnostics");
    let baseline = &rough_schedule[frame_index];
    let context = FusionContext {
        kit: &kit,
        rig_map: &rig_map,
        model: &imported.model,
        clip_index,
        raster_settings: &raster_settings,
    };

    let mut duplicate = baseline.clone();
    duplicate
        .discarded_overlaps
        .push(duplicate.discarded_overlaps[0]);
    let mut missing = baseline.clone();
    missing.discarded_overlaps.remove(0);
    let mut wrong = baseline.clone();
    wrong.discarded_overlaps[0].winner_source_voxel_index ^= 1;

    for (label, malformed) in [
        ("duplicate", duplicate),
        ("missing", missing),
        ("wrong", wrong),
    ] {
        let error = fuse_rough_frame(
            context,
            &schedule[frame_index],
            &malformed,
            FusionSettings::default(),
        )
        .unwrap_err();
        assert_eq!(
            error.code(),
            "fusion.overlapLedgerMismatch",
            "{label} overlap record must reject"
        );
    }
}

#[test]
fn generated_quota_rejects_without_mutating_the_rough_frame() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .expect("run clip");
    let schedule = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings::default(),
    )
    .expect("schedule");
    let raster_settings = RasterSettings::default();
    let rough = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &schedule,
        &raster_settings,
    )
    .expect("rough schedule")
    .remove(0);
    let before = rough.clone();
    let context = FusionContext {
        kit: &kit,
        rig_map: &rig_map,
        model: &imported.model,
        clip_index,
        raster_settings: &raster_settings,
    };
    let error = fuse_rough_frame(
        context,
        &schedule[0],
        &rough,
        FusionSettings {
            max_generated_voxels: 1,
            ..FusionSettings::default()
        },
    )
    .expect_err("one generated cell cannot hold all joints");
    assert_eq!(error.code(), "fusion.generatedVoxelQuotaExceeded");
    assert_eq!(rough, before, "rejection must not mutate the source frame");
}
