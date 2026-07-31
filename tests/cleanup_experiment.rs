use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusty_engine_voxels::assemble::{
    assemble_rough_schedule, select_pose_schedule, PoseSelectionSettings,
};
use rusty_engine_voxels::cleanup::{
    build_agent_input_bundle, evaluate_cleanup_diff, replay_frame_edits, CleanupDecision,
    CleanupMetricGates, CleanupPass, EditBounds, EditPolicy, FrameEditDiff, FrameEditOperation,
};
use rusty_engine_voxels::fusion::{
    fuse_rough_schedule, FusedFrame, FusedVoxelCell, FusedVoxelOrigin, FusionContext,
    FusionSettings,
};
use rusty_engine_voxels::kit::load_kit;
use rusty_engine_voxels::pose::{RasterSettings, RigMap};
use rusty_engine_voxels::project::sha256;
use serde_json::json;
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";
const RIFLEMAN_RIG_MAP: &str = "content/characters/rifleman/rig-map.json";
const RETRO_GLB: &str = "content/sources/kenney-retro-character/character-medium.glb";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn synthetic_frame(coordinates: &[[i64; 3]]) -> FusedFrame {
    FusedFrame {
        time_microseconds: 0,
        duration_microseconds: 100_000,
        voxels: coordinates
            .iter()
            .enumerate()
            .map(|(index, coordinate)| FusedVoxelCell {
                coordinate: *coordinate,
                material_slot: 1,
                origin: FusedVoxelOrigin::Canonical {
                    part_id: 0,
                    source_voxel_index: index as u32,
                },
                operations: Vec::new(),
            })
            .collect(),
        discarded_origins: Vec::new(),
        generated_voxels: 0,
        applied_operations: Vec::new(),
    }
}

fn open_policy(max_voxels: usize) -> EditPolicy {
    EditPolicy {
        declared_regions: vec![EditBounds {
            min: [-16, -16, -16],
            max: [16, 16, 16],
        }],
        material_slots: BTreeSet::from([1, 2]),
        max_voxel_count: max_voxels,
        max_operation_cells: 32_768,
        max_operations_per_diff: 64,
        required_anchors: BTreeSet::new(),
        protected_origins: BTreeSet::new(),
        protected_parts: BTreeSet::new(),
        protected_dimension_tolerance: [0, 0, 0],
        connected_parts: BTreeSet::new(),
    }
}

fn replay_one(
    base: &FusedFrame,
    previous: Option<&FusedFrame>,
    next: Option<&FusedFrame>,
    policy: &EditPolicy,
    operation: FrameEditOperation,
) -> Result<rusty_engine_voxels::cleanup::EditedFrame, rusty_engine_voxels::cleanup::EditError> {
    let diff = FrameEditDiff::new(
        base,
        CleanupPass::AgentGeometry { pass: 1 },
        vec![operation],
    )
    .unwrap();
    replay_frame_edits(base, previous, next, &BTreeMap::new(), policy, &[diff])
}

#[test]
fn every_edit_operation_is_closed_bounded_and_executable() {
    let base = synthetic_frame(&[[0, 0, 0], [1, 0, 0], [2, 0, 0]]);
    let previous = synthetic_frame(&[[0, 0, 0], [0, 1, 0]]);
    let next = synthetic_frame(&[[0, 0, 0], [0, 0, 1]]);
    let policy = open_policy(10_000);
    let region = EditBounds {
        min: [-2, -2, -2],
        max: [6, 6, 6],
    };
    let operations = vec![
        FrameEditOperation::AddVoxel {
            at: [0, 1, 0],
            material_slot: 1,
        },
        FrameEditOperation::RemoveVoxel { at: [2, 0, 0] },
        FrameEditOperation::MoveVoxel {
            from: [2, 0, 0],
            to: [3, 0, 0],
        },
        FrameEditOperation::FillBox {
            region: EditBounds::point([4, 0, 0]),
            material_slot: 2,
        },
        FrameEditOperation::ClearBox {
            region: EditBounds::point([2, 0, 0]),
        },
        FrameEditOperation::ReplaceMaterial {
            region,
            from_material_slot: 1,
            to_material_slot: 2,
        },
        FrameEditOperation::BridgeRegions {
            from: [2, 0, 0],
            to: [5, 0, 0],
            material_slot: 1,
        },
        FrameEditOperation::ThickenRegion {
            region,
            material_slot: 1,
            layers: 1,
        },
        FrameEditOperation::ThinRegion { region, layers: 1 },
        FrameEditOperation::CopyCanonicalRegion {
            source: EditBounds {
                min: [0, 0, 0],
                max: [2, 0, 0],
            },
            offset: [0, 2, 0],
        },
        FrameEditOperation::RestoreFromPreviousFrame {
            region: EditBounds {
                min: [0, 0, 0],
                max: [0, 1, 0],
            },
        },
        FrameEditOperation::RestoreFromNextFrame {
            region: EditBounds {
                min: [0, 0, 0],
                max: [0, 0, 1],
            },
        },
        FrameEditOperation::SmoothLocalSurface {
            region,
            material_slot: 1,
        },
        FrameEditOperation::CarveLocalSurface {
            region,
            maximum_face_neighbors: 1,
        },
        FrameEditOperation::EnforceConnectivity {
            region,
            material_slot: 1,
            maximum_bridge_length: 16,
        },
        FrameEditOperation::ShiftComponent {
            region: EditBounds::point([2, 0, 0]),
            offset: [0, 1, 0],
        },
        FrameEditOperation::SetAnchor {
            id: "effect_origin".to_owned(),
            position: [0, 0, 0],
        },
    ];

    for operation in operations {
        let encoded = serde_json::to_string(&operation).unwrap();
        let decoded: FrameEditOperation = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, operation);
        assert!(operation.affected_bounds().is_ok());
        replay_one(&base, Some(&previous), Some(&next), &policy, operation)
            .expect("closed operation executes against its valid bounded input");
    }
}

#[test]
fn safety_rejections_are_typed_fail_atomic_and_neighbor_local() {
    let base = synthetic_frame(&[[0, 0, 0], [1, 0, 0], [2, 0, 0]]);
    let previous = synthetic_frame(&[[0, 1, 0]]);
    let next = synthetic_frame(&[[0, 0, 1]]);
    let base_before = serde_json::to_vec(&base).unwrap();
    let previous_before = serde_json::to_vec(&previous).unwrap();
    let next_before = serde_json::to_vec(&next).unwrap();

    let mut protected = open_policy(16);
    protected.protected_origins.insert((0, 0));
    let error = replay_one(
        &base,
        Some(&previous),
        Some(&next),
        &protected,
        FrameEditOperation::RemoveVoxel { at: [0, 0, 0] },
    )
    .unwrap_err();
    assert_eq!(error.code(), "edit.protectedRegionChanged");
    assert_eq!(error.operation_index(), None);

    let mut local = open_policy(16);
    local.declared_regions = vec![EditBounds::point([0, 0, 0])];
    assert_eq!(
        replay_one(
            &base,
            Some(&previous),
            Some(&next),
            &local,
            FrameEditOperation::AddVoxel {
                at: [5, 0, 0],
                material_slot: 1,
            },
        )
        .unwrap_err()
        .code(),
        "edit.undeclaredRegion"
    );
    assert_eq!(
        replay_one(
            &base,
            Some(&previous),
            Some(&next),
            &open_policy(16),
            FrameEditOperation::AddVoxel {
                at: [5, 0, 0],
                material_slot: 99,
            },
        )
        .unwrap_err()
        .code(),
        "edit.invalidMaterial"
    );
    assert_eq!(
        replay_one(
            &base,
            Some(&previous),
            Some(&next),
            &open_policy(3),
            FrameEditOperation::AddVoxel {
                at: [5, 0, 0],
                material_slot: 1,
            },
        )
        .unwrap_err()
        .code(),
        "edit.voxelQuotaExceeded"
    );

    let mut required_anchor = open_policy(16);
    required_anchor
        .required_anchors
        .insert("weapon_socket".to_owned());
    assert_eq!(
        replay_one(
            &base,
            Some(&previous),
            Some(&next),
            &required_anchor,
            FrameEditOperation::AddVoxel {
                at: [5, 0, 0],
                material_slot: 1,
            },
        )
        .unwrap_err()
        .code(),
        "edit.requiredAnchorMissing"
    );

    let mut connectivity = open_policy(16);
    connectivity.connected_parts.insert(0);
    assert_eq!(
        replay_one(
            &base,
            Some(&previous),
            Some(&next),
            &connectivity,
            FrameEditOperation::RemoveVoxel { at: [1, 0, 0] },
        )
        .unwrap_err()
        .code(),
        "edit.requiredComponentDisconnected"
    );

    assert_eq!(serde_json::to_vec(&base).unwrap(), base_before);
    assert_eq!(serde_json::to_vec(&previous).unwrap(), previous_before);
    assert_eq!(serde_json::to_vec(&next).unwrap(), next_before);

    let revise_diff = FrameEditDiff::new(
        &base,
        CleanupPass::AgentGeometry { pass: 1 },
        vec![FrameEditOperation::AddVoxel {
            at: [5, 0, 0],
            material_slot: 1,
        }],
    )
    .unwrap();
    let revise = evaluate_cleanup_diff(
        &load_kit(&root(), RIFLEMAN_KIT).unwrap(),
        &base,
        Some(&previous),
        Some(&next),
        &BTreeMap::new(),
        &open_policy(16),
        &[],
        revise_diff,
        Vec::new(),
        CleanupMetricGates {
            max_occupied_voxel_increase: 0,
            max_component_increase: 1,
            allow_additional_warnings: true,
        },
    )
    .unwrap();
    assert!(matches!(revise.decision, CleanupDecision::Revise { .. }));
}

#[test]
fn checked_rifleman_forearm_defect_is_fixed_by_replayable_dsl_diff() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).unwrap();
    let source_bytes = std::fs::read(root().join(RETRO_GLB)).unwrap();
    let imported = import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: "mesh-animation/retro-character".to_owned(),
        asset_version: 1,
        source_path: RETRO_GLB.to_owned(),
        format: MeshSourceFormat::Glb,
        source_bytes,
        expected_source_sha256: None,
        mesh_primitive: None,
    })
    .unwrap();
    let rig_map: RigMap =
        serde_json::from_str(&std::fs::read_to_string(root().join(RIFLEMAN_RIG_MAP)).unwrap())
            .unwrap();
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .unwrap();
    let selected = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings::default(),
    )
    .unwrap();
    let raster = RasterSettings::default();
    let rough = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &selected,
        &raster,
    )
    .unwrap();
    let context = FusionContext {
        kit: &kit,
        rig_map: &rig_map,
        model: &imported.model,
        clip_index,
        raster_settings: &raster,
    };
    let fused = fuse_rough_schedule(context, &selected, &rough, FusionSettings::default()).unwrap();
    let original = &fused[0];
    let forearm_index = kit
        .parts
        .iter()
        .position(|part| part.id == "left_lower_arm")
        .unwrap() as u32;
    let removed = original
        .voxels
        .iter()
        .find(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical { part_id, .. } if part_id == forearm_index
            )
        })
        .unwrap()
        .clone();
    let mut defective = original.clone();
    defective
        .voxels
        .retain(|cell| cell.coordinate != removed.coordinate);

    let mut policy = EditPolicy::for_kit(
        &kit,
        vec![EditBounds::point(removed.coordinate)],
        original.voxels.len() + 32,
    )
    .unwrap();
    policy.required_anchors.insert("wrist".to_owned());
    let anchors = BTreeMap::from([("wrist".to_owned(), removed.coordinate)]);
    let bundle = build_agent_input_bundle(
        &kit,
        Some(original),
        &defective,
        fused.get(1),
        vec![
            "preserve the chunky silhouette".to_owned(),
            "change only declared regions".to_owned(),
        ],
        anchors.clone(),
    )
    .unwrap();
    assert_eq!(bundle.multiview.len(), 3);
    assert_eq!(bundle.id_passes.len(), 3);
    assert_eq!(bundle.difference_overlays.len(), 2);
    assert_eq!(
        bundle.temporal_window.current.occupied_voxels,
        original.voxels.len() - 1
    );

    let diff = FrameEditDiff::new(
        &defective,
        CleanupPass::AgentGeometry { pass: 1 },
        vec![FrameEditOperation::RestoreFromPreviousFrame {
            region: EditBounds::point(removed.coordinate),
        }],
    )
    .unwrap();
    let fixed = replay_frame_edits(
        &defective,
        Some(original),
        fused.get(1),
        &anchors,
        &policy,
        std::slice::from_ref(&diff),
    )
    .unwrap();
    let evaluation = evaluate_cleanup_diff(
        &kit,
        &defective,
        Some(original),
        fused.get(1),
        &anchors,
        &policy,
        &[],
        diff.clone(),
        bundle.style_rules.clone(),
        CleanupMetricGates::default(),
    )
    .unwrap();
    assert_eq!(evaluation.decision, CleanupDecision::Accept);
    assert_eq!(evaluation.candidate, fixed);
    let repeated = replay_frame_edits(
        &defective,
        Some(original),
        fused.get(1),
        &anchors,
        &policy,
        &[diff],
    )
    .unwrap();
    assert_eq!(fixed, repeated);
    assert_eq!(fixed.frame.voxels.len(), original.voxels.len());
    assert_eq!(
        fixed
            .frame
            .voxels
            .iter()
            .find(|cell| cell.coordinate == removed.coordinate)
            .unwrap(),
        &removed
    );

    let removable = original
        .voxels
        .iter()
        .find(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical { part_id, .. }
                    if kit.parts[part_id as usize].id == "rifle"
            )
        })
        .unwrap();
    let mut hand_policy = EditPolicy::for_kit(
        &kit,
        vec![EditBounds::point(removable.coordinate)],
        original.voxels.len(),
    )
    .unwrap();
    hand_policy.connected_parts.clear();
    let hand_edit = replay_one(
        original,
        fused.get(19),
        fused.get(1),
        &hand_policy,
        FrameEditOperation::RemoveVoxel {
            at: removable.coordinate,
        },
    )
    .unwrap();
    assert_eq!(hand_edit.frame.voxels.len(), original.voxels.len() - 1);

    let report = json!({
        "schemaVersion": 1,
        "engineRevision": "07a648a545b13bf3f3bb82c7a77c92958c1b0feb",
        "character": kit.id,
        "clip": "run",
        "frameTimeMicroseconds": defective.time_microseconds,
        "baseFrameSha256": bundle.frame_sha256,
        "dslOperationKinds": 17,
        "agentBundle": {
            "canonicalPartSummaries": bundle.canonical_parts.len(),
            "multiviewPasses": bundle.multiview.len(),
            "idPasses": bundle.id_passes.len(),
            "differenceOverlays": bundle.difference_overlays.len(),
            "temporalFrames": 3,
            "styleRules": bundle.style_rules.len(),
            "structuralWarnings": bundle.structural_warnings.len()
        },
        "forearmRepair": {
            "coordinate": removed.coordinate,
            "beforeVoxels": defective.voxels.len(),
            "afterVoxels": fixed.frame.voxels.len(),
            "decision": "accept",
            "diffSha256": sha256(&serde_json::to_vec(&fixed.diffs).unwrap()),
            "deterministicReplay": fixed == repeated
        },
        "handEdit": {
            "part": "rifle",
            "operation": "remove_voxel",
            "beforeVoxels": original.voxels.len(),
            "afterVoxels": hand_edit.frame.voxels.len()
        },
        "nonclaims": [
            "Diagnostic multiview and ID passes are deterministic agent-facing projections, not final art renders.",
            "The checked repair proves the bounded authoring loop, not autonomous model quality.",
            "Human passes remain explicit recorded diffs and are not invoked implicitly."
        ]
    });
    let checked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("evidence/cleanup-loop.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report, checked, "checked M5 evidence drifted");
}
