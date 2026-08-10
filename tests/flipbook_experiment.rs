use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use render_model::{
    MaterialUvStrategy, RenderDiff, RenderMaterialDescriptor, RenderMetadata, Transform,
};
use render_projection::{VoxelObjectProjectionInstance, VoxelObjectRenderProjector};
use rusty_engine::{
    render_model, render_projection, voxel_asset, voxel_convert, voxel_object_runtime,
};
use rusty_engine_voxels::assemble::{
    assemble_rough_schedule, select_pose_schedule, PoseSelectionSettings,
};
use rusty_engine_voxels::flipbook::{
    compile_flipbook, publish_compiled_flipbook, FlipbookCompileSettings, FrameAnchorSpec,
    FrameFactSource, HitRegionShape, HitRegionSpec,
};
use rusty_engine_voxels::fusion::{fuse_rough_schedule, FusionContext, FusionSettings};
use rusty_engine_voxels::kit::{load_kit, KitPart, VoxelKit};
use rusty_engine_voxels::pose::{RasterSettings, RigMap};
use serde_json::json;
use voxel_asset::VoxelObjectCollisionPrimitive;
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};
use voxel_object_runtime::{
    admit_voxel_object_json, VoxelObjectLoopMode, VoxelObjectPlaybackRate,
    VoxelObjectPlaybackStatus, VoxelObjectPlayer, VoxelObjectRuntimeLimits,
};

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
fn fused_run_compiles_publishes_and_plays_without_a_rig() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .expect("run clip");
    let selected = select_pose_schedule(
        &imported.model,
        clip_index,
        &PoseSelectionSettings::default(),
    )
    .expect("selected poses");
    let raster_settings = RasterSettings::default();
    let rough = assemble_rough_schedule(
        &kit,
        &rig_map,
        &imported.model,
        clip_index,
        &selected,
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
    let fused = fuse_rough_schedule(context, &selected, &rough, FusionSettings::default())
        .expect("fused schedule");
    let source_bytes = std::fs::read(root().join(RIFLEMAN_KIT)).expect("kit bytes");
    let settings = rifleman_settings(&kit);
    let compiled = compile_flipbook(context, &selected, &fused, &settings, &source_bytes)
        .expect("compile flipbook");
    let repeated = compile_flipbook(context, &selected, &fused, &settings, &source_bytes)
        .expect("repeat compilation");
    assert_eq!(compiled.canonical_json, repeated.canonical_json);
    assert_eq!(compiled.asset, repeated.asset);
    assert!(!compiled.canonical_json.contains("skeleton"));
    assert!(!compiled.canonical_json.contains("skinning"));
    let admitted = admit_voxel_object_json(
        &compiled.canonical_json,
        VoxelObjectRuntimeLimits::default(),
    )
    .expect("runtime admission");
    let run = admitted.clip("run").expect("run clip");
    assert_eq!(run.frame_indices.len(), selected.len());
    assert_eq!(admitted.frames().len(), selected.len() + 1);
    assert_eq!(
        run.frame_durations_micros,
        fused
            .iter()
            .map(|frame| frame.duration_microseconds)
            .collect::<Vec<_>>()
    );
    for runtime_index in &run.frame_indices {
        let frame = admitted.frame(*runtime_index).expect("runtime frame");
        for anchor in [
            "effect_origin",
            "head",
            "left_foot",
            "left_hand",
            "muzzle",
            "right_foot",
            "right_hand",
            "weapon_socket",
        ] {
            assert!(frame.anchor(anchor).is_some(), "missing {anchor}");
        }
        let collision = frame.collision.as_ref().expect("collision facts");
        assert!(collision.body.is_some());
        assert_eq!(
            collision
                .hit_regions
                .iter()
                .map(|region| region.id.as_str())
                .collect::<Vec<_>>(),
            ["head", "torso"]
        );
    }
    let first_hand = admitted
        .frame(run.frame_indices[0])
        .unwrap()
        .anchor("right_hand")
        .unwrap()
        .position;
    assert!(
        run.frame_indices.iter().skip(1).any(|index| {
            admitted
                .frame(*index)
                .unwrap()
                .anchor("right_hand")
                .unwrap()
                .position
                != first_hand
        }),
        "authored hand anchor must track its moving source part"
    );

    let mut player = VoxelObjectPlayer::new();
    player
        .play(
            &admitted,
            "run",
            VoxelObjectLoopMode::Once,
            VoxelObjectPlaybackRate::NORMAL,
            0,
        )
        .expect("play once");
    let terminal = player
        .sample_at(&admitted, run.duration_micros)
        .expect("terminal sample");
    assert!(terminal.ended);
    assert_eq!(
        terminal.frame,
        *run.frame_indices.last().expect("last frame")
    );
    player
        .play(
            &admitted,
            "run",
            VoxelObjectLoopMode::Repeat,
            VoxelObjectPlaybackRate::NORMAL,
            1_000_000,
        )
        .expect("play repeat");
    player.pause(1_100_000).expect("pause");
    let paused = player.sample_at(&admitted, 9_000_000).expect("paused");
    assert_eq!(paused.status, VoxelObjectPlaybackStatus::Paused);
    player.resume(10_000_000).expect("resume");
    assert_eq!(
        player
            .sample_at(&admitted, 10_100_000)
            .expect("resumed")
            .status,
        VoxelObjectPlaybackStatus::Playing
    );

    let materials = render_materials(&kit);
    let mut projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
    let instances = vec![
        projection_instance(&admitted, run.frame_indices[0], "rifleman-a"),
        projection_instance(&admitted, run.frame_indices[1], "rifleman-b"),
    ];
    let initial = projector
        .project(&instances, &materials)
        .expect("initial projection");
    assert_eq!(
        initial
            .frame
            .ops
            .iter()
            .filter(|operation| matches!(operation, RenderDiff::DefineVoxelObject { .. }))
            .count(),
        1,
        "instances share one character-type mesh definition"
    );
    let mut switched = instances;
    switched[0].frame = run.frame_indices[2];
    let swap = projector
        .project(&switched, &materials)
        .expect("frame swap");
    assert!(swap.mesh_resources.is_empty());
    assert_eq!(swap.frame.ops.len(), 1);
    assert!(matches!(
        swap.frame.ops[0],
        RenderDiff::SetVoxelObjectFrame { .. }
    ));

    let publication_root = temporary_publication_root();
    let first = publish_compiled_flipbook(&publication_root, "objects", &compiled)
        .expect("publish compiled object");
    assert_eq!(
        std::fs::read_to_string(publication_root.join(&first.path)).expect("published bytes"),
        compiled.canonical_json
    );
    assert_eq!(
        publish_compiled_flipbook(&publication_root, "objects", &compiled)
            .expect("idempotent publication"),
        first
    );
    std::fs::write(publication_root.join(&first.path), "sentinel").expect("corrupt exact path");
    assert!(publish_compiled_flipbook(&publication_root, "objects", &compiled).is_err());
    assert_eq!(
        std::fs::read_to_string(publication_root.join(&first.path)).expect("preserved sentinel"),
        "sentinel"
    );
    std::fs::remove_dir_all(&publication_root).expect("remove bounded test directory");

    let unique_mesh_payload_bytes = admitted
        .meshes()
        .iter()
        .map(|mesh| {
            mesh.positions.len() * std::mem::size_of::<[f32; 3]>()
                + mesh.normals.len() * std::mem::size_of::<[f32; 3]>()
                + mesh.indices.len() * std::mem::size_of::<u32>()
        })
        .sum::<usize>();
    let report = json!({
        "schemaVersion": 1,
        "assetId": compiled.asset.asset_id,
        "contentHash": compiled.asset.content_hash,
        "artifactBytes": compiled.canonical_json.len(),
        "clip": "run",
        "storedFrames": selected.len(),
        "runtimeFramesIncludingDefault": admitted.frames().len(),
        "uniqueMeshes": admitted.meshes().len(),
        "uniqueMeshPayloadBytes": unique_mesh_payload_bytes,
        "anchorsPerClipFrame": 8,
        "hitRegionsPerClipFrame": 2,
        "projection": {
            "sharedInstances": 2,
            "initialDefinitions": 1,
            "steadyStateOperations": swap.frame.ops.len(),
            "steadyStateBytes": serde_json::to_vec(&swap.frame).unwrap().len(),
            "steadyStateMeshResources": swap.mesh_resources.len()
        },
        "nonclaims": [
            "Payload bytes count deterministic Rust mesh arrays and exclude allocator overhead.",
            "Projection operation and byte counts are structural evidence, not a wall-clock performance claim.",
            "Coarse collision facts are authored local metadata; this experiment does not install them into a world collision service."
        ]
    });
    let checked: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("evidence/baked-flipbook-runtime.json"))
            .expect("checked M4 evidence"),
    )
    .expect("evidence JSON");
    assert_eq!(report, checked, "checked M4 evidence drifted");
}

fn rifleman_settings(kit: &VoxelKit) -> FlipbookCompileSettings {
    let left_foot = extreme_voxel_index(kit.part("left_lower_leg").unwrap(), 1, false);
    let right_foot = extreme_voxel_index(kit.part("right_lower_leg").unwrap(), 1, false);
    let muzzle = extreme_voxel_index(kit.part("rifle").unwrap(), 2, false);
    FlipbookCompileSettings {
        asset_id: "voxel-object/rifleman-baked".to_owned(),
        clip_id: "run".to_owned(),
        clip_name: "Run".to_owned(),
        source_path: RIFLEMAN_KIT.to_owned(),
        chunk_size: 16,
        anchors: vec![
            anchor("head", pivot("head")),
            anchor("left_hand", socket("left_lower_arm", "wrist")),
            anchor("right_hand", socket("right_lower_arm", "wrist")),
            anchor("left_foot", voxel("left_lower_leg", left_foot)),
            anchor("right_foot", voxel("right_lower_leg", right_foot)),
            anchor("muzzle", voxel("rifle", muzzle)),
            anchor("weapon_socket", socket("rifle", "grip")),
            anchor("effect_origin", pivot("torso")),
        ],
        body_collision: Some(VoxelObjectCollisionPrimitive::Capsule {
            center: [0.0, 7.0, 0.0],
            radius: 4.0,
            half_height: 7.0,
        }),
        hit_regions: vec![
            HitRegionSpec {
                id: "head".to_owned(),
                center: pivot("head"),
                shape: HitRegionShape::Box {
                    half_extents: [3.5, 3.5, 3.5],
                },
            },
            HitRegionSpec {
                id: "torso".to_owned(),
                center: pivot("torso"),
                shape: HitRegionShape::Box {
                    half_extents: [4.0, 5.0, 3.0],
                },
            },
        ],
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

fn extreme_voxel_index(part: &KitPart, axis: usize, maximum: bool) -> u32 {
    part.cells
        .iter()
        .enumerate()
        .min_by_key(|(_, cell)| {
            if maximum {
                -cell.coordinate[axis]
            } else {
                cell.coordinate[axis]
            }
        })
        .map(|(index, _)| index as u32)
        .expect("part has cells")
}

fn render_materials(kit: &VoxelKit) -> BTreeMap<String, RenderMaterialDescriptor> {
    kit.palette
        .iter()
        .flat_map(|group| {
            group.slots.iter().map(|slot| {
                let id = format!("material/{}-{}", kit.id, slot.slot);
                (
                    id.clone(),
                    RenderMaterialDescriptor {
                        schema_version: 1,
                        id,
                        color: slot.color,
                        texture: None,
                        roughness: 0.82,
                        texture_tint: [1.0; 4],
                        emission_color: [0.0; 3],
                        emission_intensity: 0.0,
                        uv_strategy: MaterialUvStrategy::Flat,
                        voxel_surface: None,
                    },
                )
            })
        })
        .collect()
}

fn projection_instance<'a>(
    object: &'a voxel_object_runtime::AdmittedVoxelObject,
    frame: u32,
    instance_id: &str,
) -> VoxelObjectProjectionInstance<'a> {
    VoxelObjectProjectionInstance {
        instance_id: instance_id.to_owned(),
        object,
        frame,
        transform: Transform::IDENTITY,
        visible: true,
        material_overrides: Vec::new(),
        metadata: RenderMetadata {
            source_entity: None,
            source_scene_node: None,
            tags: vec!["baked-rifleman".to_owned()],
            label: Some("Baked Rifleman".to_owned()),
        },
    }
}

fn temporary_publication_root() -> PathBuf {
    let path = std::env::temp_dir().join(format!("rusty-engine-voxels-m4-{}", std::process::id()));
    if path.exists() {
        std::fs::remove_dir_all(&path).expect("remove stale bounded test directory");
    }
    std::fs::create_dir(&path).expect("create test directory");
    path
}
