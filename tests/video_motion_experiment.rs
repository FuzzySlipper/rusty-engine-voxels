use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusty_engine_voxels::assemble::{
    assemble_rough_schedule, select_pose_schedule, PoseSelectionSettings,
};
use rusty_engine_voxels::flipbook::{
    compile_flipbook, FlipbookCompileSettings, FrameAnchorSpec, FrameFactSource,
};
use rusty_engine_voxels::fusion::{fuse_rough_schedule, FusionContext, FusionSettings};
use rusty_engine_voxels::kit::{load_kit, KitPart};
use rusty_engine_voxels::pose::RasterSettings;
use rusty_engine_voxels::project::sha256;
use rusty_engine_voxels::temporal::{
    analyze_temporal_clip, generate_flicker_review, TemporalSettings,
};
use rusty_engine_voxels::video_motion::{
    fit_multiview_landmarks_json, fitted_motion_rig_map, FittedMotion,
};
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};
use voxel_object_runtime::{admit_voxel_object_json, VoxelObjectRuntimeLimits};

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";
const LANDMARKS: &str = "evidence/video-motion/landmarks.json";
const FITTED: &str = "evidence/video-motion/fitted-motion.json";
const PROXY_MOTION: &str = "evidence/video-motion/proxy-motion.json";
const AUTHORED_TEMPORAL: &str = "evidence/temporal-consistency.json";
const MOTION_GLB: &str = "content/sources/video-fitted-rifleman/motion.glb";
const SOURCE_GLB: &str = "content/sources/kenney-retro-character/character-medium.glb";
const SOURCE_VIDEO: &str = "content/sources/kenney-retro-character/run-multiview.nut";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn checked_multiview_fit_is_fixed_length_contact_corrected_and_reproducible() {
    let landmark_json = std::fs::read_to_string(root().join(LANDMARKS)).expect("landmarks");
    let fitted = fit_multiview_landmarks_json(&landmark_json).expect("fit");
    let checked: FittedMotion = serde_json::from_str(
        &std::fs::read_to_string(root().join(FITTED)).expect("checked fitted motion"),
    )
    .expect("checked fitted document");
    assert_eq!(fitted, checked);
    assert_eq!(fitted.frames.len(), 16);
    assert_eq!(fitted.bone_lengths.len(), 17);
    assert_eq!(
        fitted.source_video_sha256,
        sha256(&std::fs::read(root().join(SOURCE_VIDEO)).expect("source video"))
            .trim_start_matches("sha256:")
    );

    for frame in &fitted.frames {
        for (bone, expected) in &fitted.bone_lengths {
            let (parent, child) = bone.split_once('>').expect("bone identity");
            let observed = distance(frame.joints[parent], frame.joints[child]);
            assert!(
                (observed - expected).abs() < 1.0e-9,
                "{bone} length drifted in frame {}",
                frame.frame_index
            );
        }
    }
    let corrected_right_foot = fitted
        .contact_corrections
        .iter()
        .filter(|receipt| receipt.feet.iter().any(|foot| foot == "rightFoot"))
        .map(|receipt| fitted.frames[receipt.frame_index as usize].joints["rightFoot"])
        .collect::<Vec<_>>();
    assert!(corrected_right_foot.len() >= 2);
    for point in corrected_right_foot.iter().skip(1) {
        assert!(distance(*point, corrected_right_foot[0]) < 1.0e-9);
    }

    let source: serde_json::Value = serde_json::from_str(&landmark_json).expect("source value");
    assert_eq!(
        source["source"]["sha256"],
        sha256(&std::fs::read(root().join(SOURCE_GLB)).expect("source glb"))
            .trim_start_matches("sha256:")
    );
    let interpolated = source["frames"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|frame| frame["views"].as_array().unwrap())
        .filter(|view| view["observationKind"] == "interpolatedDetectionGap")
        .count();
    assert_eq!(
        interpolated, 2,
        "only isolated observed detection gaps may interpolate"
    );

    let mut malformed = source;
    malformed["frames"][0]["views"][0]["weaponEndpoints"]["muzzle"]["x"] = serde_json::json!(9.0);
    let error = fit_multiview_landmarks_json(&serde_json::to_string(&malformed).unwrap())
        .expect_err("out-of-view weapon endpoint must reject");
    assert_eq!(error.code, "videoMotion.invalidWeaponEndpoint");

    let proxy: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join(PROXY_MOTION)).expect("proxy motion"),
    )
    .expect("proxy motion value");
    assert_eq!(proxy["retargetPolicy"]["rotationGain"], 0.25);
    assert_eq!(proxy["retargetPolicy"]["translationGain"], 0.25);
    assert_eq!(
        proxy["calibrationSourceSha256"],
        sha256(&std::fs::read(root().join(SOURCE_GLB)).expect("calibration source"))
    );
}

#[test]
fn fitted_motion_glb_compiles_through_the_existing_m2_to_m6_pipeline() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let bytes = std::fs::read(root().join(MOTION_GLB)).expect("motion glb");
    let imported = import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: "mesh-animation/video-fitted-rifleman".to_owned(),
        asset_version: 1,
        source_path: MOTION_GLB.to_owned(),
        format: MeshSourceFormat::Glb,
        source_bytes: bytes,
        expected_source_sha256: None,
        mesh_primitive: None,
    })
    .expect("import fitted motion");
    let rig_map = fitted_motion_rig_map(&kit, &imported.model).expect("fitted rig map");
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "fitted_run")
        .expect("fitted clip");
    let selected = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings {
            event_translation_threshold: 1_000_000.0,
            event_rotation_threshold: 1_000_000.0,
            error_budget: 1_000_000.0,
            mandatory_timestamps: (0..16)
                .map(|index| (index * 1_000_000 / 24) as u64)
                .collect(),
            ..PoseSelectionSettings::default()
        },
    )
    .expect("pose schedule");
    assert_eq!(selected.len(), 16);
    let raster_settings = RasterSettings::default();
    let rough = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &selected,
        &raster_settings,
    )
    .expect("M2 rough schedule");
    let context = FusionContext {
        kit: &kit,
        rig_map: &rig_map,
        model: &imported.model,
        clip_index,
        raster_settings: &raster_settings,
    };
    let fused = fuse_rough_schedule(context, &selected, &rough, FusionSettings::default())
        .expect("M3 fused schedule");
    let compiled = compile_flipbook(
        context,
        &selected,
        &fused,
        &compile_settings(&kit),
        &std::fs::read(root().join(RIFLEMAN_KIT)).expect("kit bytes"),
    )
    .expect("M4 compiled flipbook");
    let admitted = admit_voxel_object_json(
        &compiled.canonical_json,
        VoxelObjectRuntimeLimits::default(),
    )
    .expect("M4 runtime admission");
    assert_eq!(
        admitted
            .clip("fitted-run")
            .expect("runtime fitted clip")
            .frame_indices
            .len(),
        selected.len()
    );

    let anchors = compiled.asset.clips[0]
        .frames
        .iter()
        .map(|frame| {
            frame
                .anchors
                .iter()
                .map(|anchor| (anchor.id.clone(), anchor.position))
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let required_anchors = anchors[0].keys().cloned().collect();
    let temporal = analyze_temporal_clip(
        &kit,
        &fused,
        &anchors,
        &anchors,
        &BTreeSet::new(),
        &TemporalSettings {
            maximum_part_voxel_delta: usize::MAX,
            maximum_part_dimension_delta: i64::MAX,
            maximum_generated_voxels: usize::MAX,
            maximum_anchor_error_milli_cells: 0,
            required_anchors,
            protected_parts: BTreeSet::new(),
        },
    )
    .expect("M6 temporal analysis");
    assert_eq!(temporal.frame_count, selected.len());
    assert_eq!(temporal.average_canonical_identity_churn_millionths, 0);
    let authored: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join(AUTHORED_TEMPORAL)).expect("authored evidence"),
    )
    .expect("authored evidence value");
    assert!(
        u64::from(temporal.average_spatial_churn_millionths)
            <= authored["finishedSpatialChurnMillionths"].as_u64().unwrap(),
        "fitted clip should remain structurally comparable to the authored reference"
    );
    assert!(
        anchors.iter().skip(1).any(|frame| frame != &anchors[0]),
        "fitted clip must contain visible proxy motion"
    );
    let review = generate_flicker_review(&fused).expect("M6 visual review");
    assert!(review.alternating_gif.starts_with(b"GIF89a"));
}

fn compile_settings(kit: &rusty_engine_voxels::kit::VoxelKit) -> FlipbookCompileSettings {
    let left_foot = extreme_voxel_index(kit.part("left_lower_leg").unwrap());
    let right_foot = extreme_voxel_index(kit.part("right_lower_leg").unwrap());
    FlipbookCompileSettings {
        asset_id: "voxel-object/video-fitted-rifleman".to_owned(),
        clip_id: "fitted-run".to_owned(),
        clip_name: "Video-fitted run".to_owned(),
        source_path: RIFLEMAN_KIT.to_owned(),
        chunk_size: 16,
        anchors: vec![
            FrameAnchorSpec {
                id: "head".to_owned(),
                source: FrameFactSource::PartPivot {
                    part_id: "head".to_owned(),
                },
            },
            FrameAnchorSpec {
                id: "left_foot".to_owned(),
                source: FrameFactSource::PartVoxel {
                    part_id: "left_lower_leg".to_owned(),
                    source_voxel_index: left_foot,
                },
            },
            FrameAnchorSpec {
                id: "right_foot".to_owned(),
                source: FrameFactSource::PartVoxel {
                    part_id: "right_lower_leg".to_owned(),
                    source_voxel_index: right_foot,
                },
            },
        ],
        body_collision: None,
        hit_regions: vec![],
    }
}

fn extreme_voxel_index(part: &KitPart) -> u32 {
    part.cells
        .iter()
        .enumerate()
        .min_by_key(|(_, cell)| cell.coordinate[1])
        .map(|(index, _)| index as u32)
        .expect("part cells")
}

fn distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    ((left[0] - right[0]).powi(2) + (left[1] - right[1]).powi(2) + (left[2] - right[2]).powi(2))
        .sqrt()
}
