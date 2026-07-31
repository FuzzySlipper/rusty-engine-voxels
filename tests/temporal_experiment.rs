use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusty_engine_voxels::assemble::{
    assemble_rough_schedule, select_pose_schedule, PoseSelectionSettings,
};
use rusty_engine_voxels::flipbook::{
    compile_flipbook, FlipbookCompileSettings, FrameAnchorSpec, FrameFactSource, HitRegionShape,
    HitRegionSpec,
};
use rusty_engine_voxels::fusion::{
    fuse_rough_schedule, FusedFrame, FusedVoxelOrigin, FusionContext, FusionSettings,
};
use rusty_engine_voxels::kit::{load_kit, KitPart, VoxelKit};
use rusty_engine_voxels::pose::{RasterSettings, RigMap};
use rusty_engine_voxels::project::sha256;
use rusty_engine_voxels::temporal::{
    analyze_temporal_clip, generate_flicker_review, TemporalSettings,
};
use serde_json::json;
use voxel_asset::VoxelObjectCollisionPrimitive;
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";
const RIFLEMAN_RIG_MAP: &str = "content/characters/rifleman/rig-map.json";
const RETRO_GLB: &str = "content/sources/kenney-retro-character/character-medium.glb";
type AnchorFrames = Vec<BTreeMap<String, [f64; 3]>>;

struct CheckedRun {
    kit: VoxelKit,
    fused: Vec<FusedFrame>,
    anchors: AnchorFrames,
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn finished_run_passes_identity_churn_and_generates_flicker_review() {
    let CheckedRun {
        kit,
        fused,
        anchors,
    } = checked_run();
    let settings = TemporalSettings {
        maximum_part_voxel_delta: 64,
        maximum_part_dimension_delta: 4,
        maximum_generated_voxels: 64,
        maximum_anchor_error_milli_cells: 1,
        required_anchors: anchors[0].keys().cloned().collect(),
        protected_parts: BTreeSet::new(),
    };
    let evidence = analyze_temporal_clip(
        &kit,
        &fused,
        &anchors,
        &anchors,
        &BTreeSet::new(),
        &settings,
    )
    .unwrap();
    assert_eq!(evidence.frame_count, 20);
    assert!(
        evidence.average_spatial_churn_millionths < 689_700,
        "canonical-parts clip must beat the straight-pipeline baseline"
    );
    assert!(
        evidence.average_canonical_identity_churn_millionths
            < evidence.average_spatial_churn_millionths,
        "canonical identities should remain much steadier than spatial occupancy; observed {}",
        evidence.average_canonical_identity_churn_millionths
    );
    assert_eq!(
        evidence
            .canonical_identity_churn_by_part_millionths
            .get("head"),
        Some(&0),
        "the protected head's authored identities must remain stable; all parts: {:?}",
        evidence.canonical_identity_churn_by_part_millionths
    );
    assert_eq!(
        evidence
            .canonical_identity_churn_by_part_millionths
            .get("torso"),
        Some(&0),
        "the protected torso's authored identities must remain stable; all parts: {:?}",
        evidence.canonical_identity_churn_by_part_millionths
    );
    assert!(evidence
        .anchor_trajectories
        .iter()
        .all(|trajectory| trajectory
            .samples
            .iter()
            .all(|sample| sample.proxy_error_milli_cells == 0)));
    assert!(evidence
        .transitions
        .iter()
        .all(|transition| transition.material_identity_changes == 0));
    let mut warning_counts = BTreeMap::new();
    for warning in &evidence.warnings {
        *warning_counts.entry(warning.code.clone()).or_insert(0usize) += 1;
    }

    let artifacts = generate_flicker_review(&fused).unwrap();
    assert!(artifacts.alternating_gif.starts_with(b"GIF89a"));
    assert!(artifacts.onion_skin_svg.contains("Three-frame onion skin"));
    assert!(artifacts
        .difference_heat_map_svg
        .contains("temporal difference heat map"));
    assert!(artifacts
        .silhouette_edge_motion_svg
        .contains("Silhouette-edge motion"));
    assert!(artifacts.palette_flicker_svg.contains("Palette flicker"));
    let report = json!({
        "schemaVersion": 1,
        "engineRevision": "07a648a545b13bf3f3bb82c7a77c92958c1b0feb",
        "character": kit.id,
        "clip": "run",
        "frameCount": evidence.frame_count,
        "straightPipelineBaselineSpatialChurnMillionths": 689700,
        "finishedSpatialChurnMillionths": evidence.average_spatial_churn_millionths,
        "canonicalIdentityChurnMillionths": evidence.average_canonical_identity_churn_millionths,
        "canonicalIdentityChurnByPartMillionths": evidence.canonical_identity_churn_by_part_millionths,
        "improvementMillionths": 1_000_000u64
            - u64::from(evidence.average_spatial_churn_millionths) * 1_000_000 / 689_700,
        "churnByHeightBand": evidence.churn_by_height_band,
        "generatedVoxelMinimum": evidence.generated_voxel_minimum,
        "generatedVoxelMaximum": evidence.generated_voxel_maximum,
        "anchorCount": evidence.anchor_trajectories.len(),
        "hardIdentityFailures": 0,
        "softWarningCount": evidence.warnings.len(),
        "softWarningsByCode": warning_counts,
        "reviewArtifacts": {
            "alternatingGif": {
                "path": "evidence/temporal-review/alternating.gif",
                "sha256": sha256(&artifacts.alternating_gif),
                "bytes": artifacts.alternating_gif.len()
            },
            "onionSkin": artifact_report(
                "evidence/temporal-review/onion-skin.svg",
                artifacts.onion_skin_svg.as_bytes()
            ),
            "differenceHeatMap": artifact_report(
                "evidence/temporal-review/difference-heat-map.svg",
                artifacts.difference_heat_map_svg.as_bytes()
            ),
            "silhouetteEdgeMotion": artifact_report(
                "evidence/temporal-review/silhouette-edge-motion.svg",
                artifacts.silhouette_edge_motion_svg.as_bytes()
            ),
            "paletteFlicker": artifact_report(
                "evidence/temporal-review/palette-flicker.svg",
                artifacts.palette_flicker_svg.as_bytes()
            )
        },
        "interpretationLimits": [
            "Spatial churn includes intentional rigid motion; canonical-identity churn separates identity stability from coordinate motion.",
            "Height bands are geometric proxies; per-part metrics and ID passes provide semantic attribution.",
            "GIF and SVG artifacts are deterministic human-review aids, not renderer or runtime authority."
        ]
    });

    if std::env::var_os("RUSTY_UPDATE_TEMPORAL_EVIDENCE").is_some() {
        let directory = root().join("evidence/temporal-review");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("alternating.gif"),
            &artifacts.alternating_gif,
        )
        .unwrap();
        std::fs::write(directory.join("onion-skin.svg"), &artifacts.onion_skin_svg).unwrap();
        std::fs::write(
            directory.join("difference-heat-map.svg"),
            &artifacts.difference_heat_map_svg,
        )
        .unwrap();
        std::fs::write(
            directory.join("silhouette-edge-motion.svg"),
            &artifacts.silhouette_edge_motion_svg,
        )
        .unwrap();
        std::fs::write(
            directory.join("palette-flicker.svg"),
            &artifacts.palette_flicker_svg,
        )
        .unwrap();
        std::fs::write(
            root().join("evidence/temporal-consistency.json"),
            format!("{}\n", serde_json::to_string_pretty(&report).unwrap()),
        )
        .unwrap();
    }

    let checked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("evidence/temporal-consistency.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(report, checked, "checked M6 evidence drifted");
    assert_eq!(
        std::fs::read(root().join("evidence/temporal-review/alternating.gif")).unwrap(),
        artifacts.alternating_gif
    );
}

#[test]
fn boiling_anchor_and_hard_identity_defects_are_classified() {
    let CheckedRun {
        kit,
        fused,
        anchors,
    } = checked_run();
    let mut duplicate = fused[..2].to_vec();
    let duplicated_cell = duplicate[1].voxels[0].clone();
    duplicate[1].voxels.push(duplicated_cell);
    let error = analyze_temporal_clip(
        &kit,
        &duplicate,
        &anchors[..2],
        &anchors[..2],
        &BTreeSet::new(),
        &TemporalSettings::default(),
    )
    .unwrap_err();
    assert_eq!(error.code(), "temporal.duplicateCanonicalIdentity");

    let head_index = kit.parts.iter().position(|part| part.id == "head").unwrap() as u32;
    let mut missing = fused[..2].to_vec();
    let missing_identity = missing[1]
        .voxels
        .iter()
        .find_map(|cell| match cell.origin {
            FusedVoxelOrigin::Canonical {
                part_id,
                source_voxel_index,
            } if part_id == head_index => Some(source_voxel_index),
            _ => None,
        })
        .unwrap();
    missing[1].voxels.retain(|cell| {
        !matches!(
            cell.origin,
            FusedVoxelOrigin::Canonical {
                part_id,
                source_voxel_index
            } if part_id == head_index && source_voxel_index == missing_identity
        )
    });
    missing[1].discarded_origins.retain(|discarded| {
        discarded.part_id != head_index || discarded.source_voxel_index != missing_identity
    });
    let error = analyze_temporal_clip(
        &kit,
        &missing,
        &anchors[..2],
        &anchors[..2],
        &BTreeSet::new(),
        &TemporalSettings {
            protected_parts: BTreeSet::from([head_index]),
            ..TemporalSettings::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code(), "temporal.protectedIdentityChanged");

    let mut observed = anchors[..2].to_vec();
    observed[1].get_mut("head").unwrap()[0] += 2.0;
    let mut boiling = fused[..2].to_vec();
    for cell in &mut boiling[1].voxels {
        if matches!(
            cell.origin,
            FusedVoxelOrigin::Canonical { part_id, .. } if part_id == head_index
        ) {
            cell.material_slot = 2;
            break;
        }
    }
    let evidence = analyze_temporal_clip(
        &kit,
        &boiling,
        &observed,
        &anchors[..2],
        &BTreeSet::new(),
        &TemporalSettings {
            maximum_part_voxel_delta: 0,
            maximum_part_dimension_delta: 0,
            maximum_generated_voxels: 0,
            maximum_anchor_error_milli_cells: 10,
            required_anchors: BTreeSet::from(["head".to_owned()]),
            protected_parts: BTreeSet::new(),
        },
    )
    .unwrap();
    assert!(evidence
        .warnings
        .iter()
        .any(|warning| warning.code == "temporal.anchor_proxy_drift"
            && warning.region.as_deref() == Some("head")
            && warning.view.as_deref() == Some("anchor_overlay")));
    assert!(evidence.warnings.iter().any(|warning| warning.code
        == "temporal.canonical_material_changed"
        && warning.view.as_deref() == Some("palette_flicker")));
}

#[test]
fn protected_identity_inventory_is_exact_and_admitted() {
    let CheckedRun {
        kit,
        fused,
        anchors,
    } = checked_run();
    let head_index = kit.parts.iter().position(|part| part.id == "head").unwrap() as u32;
    let torso_index = kit
        .parts
        .iter()
        .position(|part| part.id == "torso")
        .unwrap() as u32;
    let source_voxel_index = fused[1]
        .voxels
        .iter()
        .find_map(|cell| match cell.origin {
            FusedVoxelOrigin::Canonical {
                part_id,
                source_voxel_index,
            } if part_id == head_index => Some(source_voxel_index),
            _ => None,
        })
        .unwrap();
    let settings = TemporalSettings {
        protected_parts: BTreeSet::from([head_index]),
        ..TemporalSettings::default()
    };

    let mut missing = fused[..2].to_vec();
    remove_identity(&mut missing[1], (head_index, source_voxel_index));
    assert_temporal_error(
        &kit,
        &missing,
        &anchors,
        &settings,
        "temporal.protectedIdentityChanged",
    );

    let mut equal_count_substitution = fused[..2].to_vec();
    replace_identity(
        &mut equal_count_substitution[1],
        (head_index, source_voxel_index),
        (head_index, u32::MAX),
    );
    assert_temporal_error(
        &kit,
        &equal_count_substitution,
        &anchors,
        &settings,
        "temporal.invalidCanonicalIdentity",
    );

    let mut cross_part_substitution = fused[..2].to_vec();
    replace_identity(
        &mut cross_part_substitution[1],
        (head_index, source_voxel_index),
        (torso_index, 0),
    );
    assert_temporal_error(
        &kit,
        &cross_part_substitution,
        &anchors,
        &settings,
        "temporal.protectedIdentityChanged",
    );

    let mut invalid_part = fused[..2].to_vec();
    replace_identity(
        &mut invalid_part[1],
        (head_index, source_voxel_index),
        (u32::MAX, source_voxel_index),
    );
    assert_temporal_error(
        &kit,
        &invalid_part,
        &anchors,
        &settings,
        "temporal.invalidCanonicalIdentity",
    );

    let mut extra = fused[..2].to_vec();
    let mut extra_cell = extra[1]
        .voxels
        .iter()
        .find(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical { part_id, .. } if part_id == head_index
            )
        })
        .unwrap()
        .clone();
    extra_cell.coordinate = free_neighbor(&extra[1], extra_cell.coordinate);
    extra_cell.origin = FusedVoxelOrigin::Canonical {
        part_id: head_index,
        source_voxel_index: u32::MAX,
    };
    extra[1].voxels.push(extra_cell);
    assert_temporal_error(
        &kit,
        &extra,
        &anchors,
        &settings,
        "temporal.invalidCanonicalIdentity",
    );

    let mut multiple_footprints = fused[..2].to_vec();
    let mut repeated_cell = multiple_footprints[1]
        .voxels
        .iter()
        .find(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical {
                    part_id,
                    source_voxel_index: source
                } if part_id == head_index && source == source_voxel_index
            )
        })
        .unwrap()
        .clone();
    repeated_cell.coordinate = free_neighbor(&multiple_footprints[1], repeated_cell.coordinate);
    multiple_footprints[1].voxels.push(repeated_cell);
    analyze_temporal_clip(
        &kit,
        &multiple_footprints,
        &anchors[..2],
        &anchors[..2],
        &BTreeSet::new(),
        &settings,
    )
    .expect("multiple spatial footprints for one admitted identity remain valid");
}

#[test]
fn discarded_overlap_winner_identity_must_be_admitted() {
    let CheckedRun {
        kit,
        fused,
        anchors,
    } = checked_run();
    let discarded_index = fused[1]
        .discarded_origins
        .iter()
        .position(|discarded| {
            discarded.winner_part_id != discarded.part_id
                || discarded.winner_source_voxel_index != discarded.source_voxel_index
        })
        .expect("checked fused frame has a real overlap winner");

    let mut invalid_winner_part = fused[..2].to_vec();
    invalid_winner_part[1].discarded_origins[discarded_index].winner_part_id = u32::MAX;
    assert_temporal_error(
        &kit,
        &invalid_winner_part,
        &anchors,
        &TemporalSettings::default(),
        "temporal.invalidCanonicalIdentity",
    );

    let mut invalid_winner_source = fused[..2].to_vec();
    invalid_winner_source[1].discarded_origins[discarded_index].winner_source_voxel_index =
        u32::MAX;
    assert_temporal_error(
        &kit,
        &invalid_winner_source,
        &anchors,
        &TemporalSettings::default(),
        "temporal.invalidCanonicalIdentity",
    );
}

fn remove_identity(frame: &mut FusedFrame, identity: (u32, u32)) {
    frame.voxels.retain(|cell| {
        !matches!(
            cell.origin,
            FusedVoxelOrigin::Canonical {
                part_id,
                source_voxel_index
            } if (part_id, source_voxel_index) == identity
        )
    });
    frame
        .discarded_origins
        .retain(|discarded| (discarded.part_id, discarded.source_voxel_index) != identity);
}

fn replace_identity(frame: &mut FusedFrame, from: (u32, u32), to: (u32, u32)) {
    for cell in &mut frame.voxels {
        if matches!(
            cell.origin,
            FusedVoxelOrigin::Canonical {
                part_id,
                source_voxel_index
            } if (part_id, source_voxel_index) == from
        ) {
            cell.origin = FusedVoxelOrigin::Canonical {
                part_id: to.0,
                source_voxel_index: to.1,
            };
        }
    }
    for discarded in &mut frame.discarded_origins {
        if (discarded.part_id, discarded.source_voxel_index) == from {
            discarded.part_id = to.0;
            discarded.source_voxel_index = to.1;
        }
    }
}

fn free_neighbor(frame: &FusedFrame, coordinate: [i64; 3]) -> [i64; 3] {
    let occupied = frame
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    (1..)
        .map(|offset| [coordinate[0] + offset, coordinate[1], coordinate[2]])
        .find(|candidate| !occupied.contains(candidate))
        .unwrap()
}

fn assert_temporal_error(
    kit: &VoxelKit,
    frames: &[FusedFrame],
    anchors: &AnchorFrames,
    settings: &TemporalSettings,
    expected_code: &str,
) {
    let error = analyze_temporal_clip(
        kit,
        frames,
        &anchors[..frames.len()],
        &anchors[..frames.len()],
        &BTreeSet::new(),
        settings,
    )
    .unwrap_err();
    assert_eq!(error.code(), expected_code);
}

fn checked_run() -> CheckedRun {
    let kit = load_kit(&root(), RIFLEMAN_KIT).unwrap();
    let bytes = std::fs::read(root().join(RETRO_GLB)).unwrap();
    let imported = import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: "mesh-animation/retro-character".to_owned(),
        asset_version: 1,
        source_path: RETRO_GLB.to_owned(),
        format: MeshSourceFormat::Glb,
        source_bytes: bytes,
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
    let source_bytes = std::fs::read(root().join(RIFLEMAN_KIT)).unwrap();
    let compiled = compile_flipbook(
        context,
        &selected,
        &fused,
        &rifleman_settings(&kit),
        &source_bytes,
    )
    .unwrap();
    let anchors = compiled.asset.clips[0]
        .frames
        .iter()
        .map(|frame| {
            frame
                .anchors
                .iter()
                .map(|anchor| (anchor.id.clone(), anchor.position))
                .collect()
        })
        .collect();
    CheckedRun {
        kit,
        fused,
        anchors,
    }
}

fn rifleman_settings(kit: &VoxelKit) -> FlipbookCompileSettings {
    let left_foot = extreme_voxel_index(kit.part("left_lower_leg").unwrap(), 1);
    let right_foot = extreme_voxel_index(kit.part("right_lower_leg").unwrap(), 1);
    let muzzle = extreme_voxel_index(kit.part("rifle").unwrap(), 2);
    FlipbookCompileSettings {
        asset_id: "voxel-object/rifleman-baked".to_owned(),
        clip_id: "run".to_owned(),
        clip_name: "Run".to_owned(),
        source_path: RIFLEMAN_KIT.to_owned(),
        chunk_size: 16,
        anchors: vec![
            anchor("head", pivot("head")),
            anchor("chest", pivot("torso")),
            anchor("pelvis", pivot("pelvis")),
            anchor("left_hand", socket("left_lower_arm", "wrist")),
            anchor("right_hand", socket("right_lower_arm", "wrist")),
            anchor("left_foot", voxel("left_lower_leg", left_foot)),
            anchor("right_foot", voxel("right_lower_leg", right_foot)),
            anchor("muzzle", voxel("rifle", muzzle)),
        ],
        body_collision: Some(VoxelObjectCollisionPrimitive::Capsule {
            center: [0.0, 7.0, 0.0],
            radius: 4.0,
            half_height: 7.0,
        }),
        hit_regions: vec![HitRegionSpec {
            id: "head".to_owned(),
            center: pivot("head"),
            shape: HitRegionShape::Box {
                half_extents: [3.5, 3.5, 3.5],
            },
        }],
    }
}

fn anchor(id: &str, source: FrameFactSource) -> FrameAnchorSpec {
    FrameAnchorSpec {
        id: id.to_owned(),
        source,
    }
}

fn pivot(part_id: &str) -> FrameFactSource {
    FrameFactSource::PartPivot {
        part_id: part_id.to_owned(),
    }
}

fn socket(part_id: &str, socket_id: &str) -> FrameFactSource {
    FrameFactSource::PartSocket {
        part_id: part_id.to_owned(),
        socket_id: socket_id.to_owned(),
    }
}

fn voxel(part_id: &str, source_voxel_index: u32) -> FrameFactSource {
    FrameFactSource::PartVoxel {
        part_id: part_id.to_owned(),
        source_voxel_index,
    }
}

fn extreme_voxel_index(part: &KitPart, axis: usize) -> u32 {
    part.cells
        .iter()
        .enumerate()
        .min_by_key(|(_, cell)| cell.coordinate[axis])
        .map(|(index, _)| index as u32)
        .unwrap()
}

fn artifact_report(path: &str, bytes: &[u8]) -> serde_json::Value {
    json!({
        "path": path,
        "sha256": sha256(bytes),
        "bytes": bytes.len()
    })
}
