use std::collections::BTreeMap;
use std::fs;
use std::mem::size_of_val;
use std::path::Path;
use std::time::Instant;

use render_model::{
    MaterialUvStrategy, MeshMaterialSlot, RenderDiff, RenderFrameDiff, RenderMaterialDescriptor,
    RenderMetadata, Transform,
};
use render_projection::{
    VoxelObjectProjectionInstance, VoxelObjectProjectionResult, VoxelObjectRenderProjector,
};
use serde::Serialize;
use voxel_object_runtime::{
    admit_voxel_object_json, AdmittedVoxelObject, VoxelObjectCollisionPolicy,
    VoxelObjectCollisionResolution, VoxelObjectLoopMode, VoxelObjectPlaybackRate,
    VoxelObjectPlaybackStatus, VoxelObjectPlayer, VoxelObjectRuntimeLimits,
};

use crate::model::{ProjectFrameSelection, ProjectMaterial, ProjectVoxelObjectInstance};
use crate::project::{load_project, read_bounded_text, safe_join, LoadedProject, MAX_OBJECT_BYTES};
use crate::provider_pin::engine_revision;
use crate::surface::{load_surface_assets, SurfaceAssets};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaybackSampleEvidence {
    pub time_microseconds: u64,
    pub runtime_frame: u32,
    pub clip_frame: Option<u32>,
    pub mesh_index: u32,
    pub voxel_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeEvidence {
    pub engine_revision: String,
    pub project_hash: String,
    pub asset_id: String,
    pub content_hash: String,
    pub frame_count: u32,
    pub clip_count: u32,
    pub unique_mesh_count: u32,
    pub resolved_voxels: usize,
    pub projection_operation_count: usize,
    pub defined_voxel_objects: usize,
    pub created_voxel_instances: usize,
    pub projection_json_bytes: usize,
    pub load_milliseconds: u128,
    pub resources: RuntimeResourceEvidence,
    pub frame_switch: FrameSwitchEvidence,
    pub behavior: RuntimeBehaviorEvidence,
    pub playback_clip: String,
    pub playback_samples: Vec<PlaybackSampleEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeResourceEvidence {
    pub canonical_object_bytes: usize,
    pub resolved_cell_bytes: usize,
    pub unique_mesh_payload_bytes: usize,
    pub mesh_vertices: u64,
    pub mesh_indices: u64,
    pub mesh_faces: u64,
    pub admission_and_meshing_microseconds: u128,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameSwitchEvidence {
    pub requested_switches: usize,
    pub emitted_frame_swaps: usize,
    pub projection_cpu_nanoseconds: u128,
    pub average_projection_cpu_nanoseconds_per_swap: u128,
    pub incremental_projection_bytes: usize,
    pub unique_meshes_reused: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeBehaviorEvidence {
    pub saved_frame: String,
    pub default_runtime_frame: u32,
    pub selected_clip: String,
    pub once_terminal_frame: u32,
    pub once_ended: bool,
    pub repeat_wrapped_to_first_frame: bool,
    pub paused_frame_stayed_stable: bool,
    pub resumed_to_later_frame: bool,
    pub posture_round_trip_matched: bool,
    pub project_reopen_matched: bool,
    pub missing_asset_rejected: bool,
    pub corrupt_asset_rejected: bool,
    pub collision_kind: String,
    pub collision_voxel_data_hash: Option<String>,
    pub collision_stayed_stable_during_playback: bool,
    pub durable_project_bytes_unchanged: bool,
    pub durable_object_bytes_unchanged: bool,
}

pub struct RuntimeProject {
    pub loaded: LoadedProject,
    pub objects: BTreeMap<String, AdmittedVoxelObject>,
    pub surface: SurfaceAssets,
    pub resources: RuntimeResourceEvidence,
}

pub fn load_runtime_project(root: &Path, relative_project: &str) -> Result<RuntimeProject, String> {
    let loaded = load_project(root, relative_project)?;
    load_runtime_project_from_loaded(loaded)
}

pub(crate) fn load_runtime_project_from_loaded(
    loaded: LoadedProject,
) -> Result<RuntimeProject, String> {
    let surface = load_surface_assets(&loaded)?;
    load_runtime_project_from_loaded_with_surface(loaded, surface)
}

pub(crate) fn load_runtime_project_from_loaded_with_surface(
    loaded: LoadedProject,
    surface: SurfaceAssets,
) -> Result<RuntimeProject, String> {
    let mut objects = BTreeMap::new();
    let mut canonical_object_bytes = 0usize;
    let mut admission_and_meshing_microseconds = 0u128;
    for entry in &loaded.project.voxel_objects {
        let path = safe_join(&loaded.root, &entry.path)?;
        let text = read_bounded_text(&path, MAX_OBJECT_BYTES, "voxel object")?;
        canonical_object_bytes = canonical_object_bytes.saturating_add(text.len());
        let admission_started = Instant::now();
        let object = admit_voxel_object_json(&text, VoxelObjectRuntimeLimits::default())
            .map_err(|error| format!("{}: {error}", path.display()))?;
        admission_and_meshing_microseconds = admission_and_meshing_microseconds
            .saturating_add(admission_started.elapsed().as_micros());
        if object.asset_id() != entry.asset_id
            || object.content_hash() != entry.expected_content_hash
        {
            return Err(format!(
                "{} does not match its project identity",
                entry.path
            ));
        }
        objects.insert(entry.asset_id.clone(), object);
    }
    for instance in &loaded.project.instances {
        let object = objects
            .get(&instance.voxel_object_asset_id)
            .ok_or_else(|| {
                format!(
                    "instance {} references unloaded {}",
                    instance.instance_id, instance.voxel_object_asset_id
                )
            })?;
        resolve_frame(object, &instance.frame)?;
        object
            .resolve_collision(&runtime_collision_policy(&instance.collision_policy))
            .map_err(|error| {
                format!(
                    "instance {} collision policy: {error}",
                    instance.instance_id
                )
            })?;
    }
    let resources = resource_evidence(
        &objects,
        canonical_object_bytes,
        admission_and_meshing_microseconds,
    );
    Ok(RuntimeProject {
        loaded,
        objects,
        surface,
        resources,
    })
}

pub fn verify_runtime_project(
    root: &Path,
    relative_project: &str,
) -> Result<RuntimeEvidence, String> {
    let started = Instant::now();
    let runtime = load_runtime_project(root, relative_project)?;
    let load_milliseconds = started.elapsed().as_millis();
    let primary = runtime
        .loaded
        .project
        .voxel_objects
        .first()
        .ok_or("project has no voxel object")?;
    let object = runtime
        .objects
        .get(&primary.asset_id)
        .ok_or("primary voxel object was not loaded")?;
    let readout = object.readout();
    let resolved_voxels = object.frames().iter().map(|frame| frame.cells.len()).sum();
    let clip = runtime.loaded.project.conversion.default_clip.clone();
    let mut player = VoxelObjectPlayer::new();
    player
        .play(
            object,
            &clip,
            VoxelObjectLoopMode::Repeat,
            VoxelObjectPlaybackRate::NORMAL,
            0,
        )
        .map_err(|error| error.to_string())?;
    let mut playback_samples = Vec::new();
    for time_microseconds in [0_u64, 100_000, 200_000, 400_000, 800_000] {
        let sample = player
            .sample_at(object, time_microseconds)
            .map_err(|error| error.to_string())?;
        let frame = object
            .frame(sample.frame)
            .ok_or_else(|| format!("runtime selected missing frame {}", sample.frame))?;
        playback_samples.push(PlaybackSampleEvidence {
            time_microseconds,
            runtime_frame: sample.frame,
            clip_frame: sample.clip_frame,
            mesh_index: frame.mesh_index,
            voxel_count: frame.cells.len(),
        });
    }
    let frame = complete_projection(&runtime)?;
    let projection_operation_count = frame.ops.len();
    let defined_voxel_objects = frame
        .ops
        .iter()
        .filter(|operation| matches!(operation, RenderDiff::DefineVoxelObject { .. }))
        .count();
    let created_voxel_instances = frame
        .ops
        .iter()
        .filter(|operation| matches!(operation, RenderDiff::CreateVoxelObjectInstance { .. }))
        .count();
    let projection_json_bytes = serde_json::to_vec(&frame)
        .map_err(|error| error.to_string())?
        .len();
    let frame_switch = benchmark_frame_switches(&runtime, object, &clip)?;
    let behavior = verify_runtime_behavior(&runtime, object, &clip)?;
    Ok(RuntimeEvidence {
        engine_revision: engine_revision()?,
        project_hash: runtime.loaded.project_hash,
        asset_id: readout.asset_id.to_owned(),
        content_hash: readout.content_hash.to_owned(),
        frame_count: readout.frame_count,
        clip_count: readout.clip_count,
        unique_mesh_count: readout.unique_mesh_count,
        resolved_voxels,
        projection_operation_count,
        defined_voxel_objects,
        created_voxel_instances,
        projection_json_bytes,
        load_milliseconds,
        resources: runtime.resources.clone(),
        frame_switch,
        behavior,
        playback_clip: clip,
        playback_samples,
    })
}

fn resource_evidence(
    objects: &BTreeMap<String, AdmittedVoxelObject>,
    canonical_object_bytes: usize,
    admission_and_meshing_microseconds: u128,
) -> RuntimeResourceEvidence {
    let mut resolved_cell_bytes = 0usize;
    let mut unique_mesh_payload_bytes = 0usize;
    let mut mesh_vertices = 0u64;
    let mut mesh_indices = 0u64;
    let mut mesh_faces = 0u64;
    for object in objects.values() {
        for frame in object.frames() {
            resolved_cell_bytes =
                resolved_cell_bytes.saturating_add(size_of_val(frame.cells.as_ref()));
        }
        for mesh in object.meshes() {
            unique_mesh_payload_bytes = unique_mesh_payload_bytes
                .saturating_add(size_of_val(mesh.positions.as_slice()))
                .saturating_add(size_of_val(mesh.normals.as_slice()))
                .saturating_add(size_of_val(mesh.indices.as_slice()))
                .saturating_add(size_of_val(mesh.groups.as_slice()));
            mesh_vertices = mesh_vertices.saturating_add(u64::from(mesh.stats.vertices));
            mesh_indices = mesh_indices.saturating_add(u64::from(mesh.stats.indices));
            mesh_faces = mesh_faces.saturating_add(u64::from(mesh.stats.faces_emitted));
        }
    }
    RuntimeResourceEvidence {
        canonical_object_bytes,
        resolved_cell_bytes,
        unique_mesh_payload_bytes,
        mesh_vertices,
        mesh_indices,
        mesh_faces,
        admission_and_meshing_microseconds,
    }
}

fn benchmark_frame_switches(
    runtime: &RuntimeProject,
    object: &AdmittedVoxelObject,
    clip_id: &str,
) -> Result<FrameSwitchEvidence, String> {
    const SWITCHES: usize = 512;
    let clip = object
        .clip(clip_id)
        .ok_or_else(|| format!("unknown benchmark clip {clip_id}"))?;
    let instance = runtime
        .loaded
        .project
        .instances
        .first()
        .ok_or("project has no runtime instance")?;
    let mut projector = VoxelObjectRenderProjector::new();
    project_runtime_with_instance_frame(runtime, &mut projector, None)?;
    let started = Instant::now();
    let mut diffs = Vec::with_capacity(SWITCHES);
    for index in 0..SWITCHES {
        let frame = clip.frame_indices[index % clip.frame_indices.len()];
        diffs.push(
            project_runtime_with_instance_frame(
                runtime,
                &mut projector,
                Some((&instance.instance_id, frame)),
            )?
            .frame,
        );
    }
    let projection_cpu_nanoseconds = started.elapsed().as_nanos();
    let emitted_frame_swaps = diffs
        .iter()
        .flat_map(|diff| &diff.ops)
        .filter(|operation| matches!(operation, RenderDiff::SetVoxelObjectFrame { .. }))
        .count();
    let incremental_projection_bytes = diffs.iter().try_fold(0usize, |total, diff| {
        serde_json::to_vec(diff)
            .map(|bytes| total.saturating_add(bytes.len()))
            .map_err(|error| error.to_string())
    })?;
    Ok(FrameSwitchEvidence {
        requested_switches: SWITCHES,
        emitted_frame_swaps,
        projection_cpu_nanoseconds,
        average_projection_cpu_nanoseconds_per_swap: projection_cpu_nanoseconds
            / emitted_frame_swaps.max(1) as u128,
        incremental_projection_bytes,
        unique_meshes_reused: object.readout().unique_mesh_count,
    })
}

fn verify_runtime_behavior(
    runtime: &RuntimeProject,
    object: &AdmittedVoxelObject,
    clip_id: &str,
) -> Result<RuntimeBehaviorEvidence, String> {
    let instance = runtime
        .loaded
        .project
        .instances
        .first()
        .ok_or("project has no runtime instance")?;
    let project_before = fs::read(&runtime.loaded.path).map_err(|error| error.to_string())?;
    let object_entry = runtime
        .loaded
        .project
        .voxel_objects
        .iter()
        .find(|entry| entry.asset_id == instance.voxel_object_asset_id)
        .ok_or("runtime instance has no project object entry")?;
    let object_path = safe_join(&runtime.loaded.root, &object_entry.path)?;
    let object_before = fs::read(&object_path).map_err(|error| error.to_string())?;
    let clip = object
        .clip(clip_id)
        .ok_or_else(|| format!("unknown behavior clip {clip_id}"))?;
    let duration = clip.duration_micros;

    let mut once = VoxelObjectPlayer::new();
    once.play(
        object,
        clip_id,
        VoxelObjectLoopMode::Once,
        VoxelObjectPlaybackRate::NORMAL,
        0,
    )
    .map_err(|error| error.to_string())?;
    let once_terminal = once
        .sample_at(object, duration.saturating_add(1))
        .map_err(|error| error.to_string())?;
    let once_terminal_frame = once_terminal.frame;
    let once_ended = once_terminal.ended;

    let mut repeat = VoxelObjectPlayer::new();
    repeat
        .play(
            object,
            clip_id,
            VoxelObjectLoopMode::Repeat,
            VoxelObjectPlaybackRate::NORMAL,
            1_000_000,
        )
        .map_err(|error| error.to_string())?;
    let repeat_wrapped_to_first_frame = repeat
        .sample_at(object, 1_000_000_u64.saturating_add(duration))
        .map_err(|error| error.to_string())?
        .frame
        == clip.frame_indices[0];
    let first_duration = clip.frame_durations_micros.first().copied().unwrap_or(1);
    repeat
        .pause(1_000_000_u64.saturating_add(first_duration))
        .map_err(|error| error.to_string())?;
    let paused = repeat
        .sample_at(object, 2_000_000_u64.saturating_add(duration))
        .map_err(|error| error.to_string())?;
    let paused_status = paused.status;
    let paused_frame = paused.frame;
    let paused_later = repeat
        .sample_at(object, 3_000_000_u64.saturating_add(duration))
        .map_err(|error| error.to_string())?;
    let paused_later_status = paused_later.status;
    let paused_later_frame = paused_later.frame;
    let posture_json = serde_json::to_string(
        &repeat
            .posture_at(3_000_000_u64.saturating_add(duration))
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let posture = serde_json::from_str(&posture_json).map_err(|error| error.to_string())?;
    let restored =
        VoxelObjectPlayer::restore(object, posture, 3_000_000_u64.saturating_add(duration))
            .map_err(|error| error.to_string())?;
    let restored_sample = restored
        .sample_at(object, 3_000_000_u64.saturating_add(duration))
        .map_err(|error| error.to_string())?;
    let restored_status = restored_sample.status;
    let restored_frame = restored_sample.frame;
    repeat
        .resume(4_000_000_u64.saturating_add(duration))
        .map_err(|error| error.to_string())?;
    let resumed = repeat
        .sample_at(
            object,
            4_000_000_u64
                .saturating_add(duration)
                .saturating_add(first_duration),
        )
        .map_err(|error| error.to_string())?;
    let resumed_status = resumed.status;
    let resumed_frame = resumed.frame;

    let collision_policy = runtime_collision_policy(&instance.collision_policy);
    let collision_before = collision_readout(
        object
            .resolve_collision(&collision_policy)
            .map_err(|error| error.to_string())?,
    );
    let collision_after = collision_readout(
        object
            .resolve_collision(&collision_policy)
            .map_err(|error| error.to_string())?,
    );
    let missing_path = runtime
        .loaded
        .root
        .join("content/voxel-objects/missing.voxel-object.json");
    let missing_asset_rejected =
        read_bounded_text(&missing_path, MAX_OBJECT_BYTES, "voxel object").is_err();
    let corrupt_asset_rejected =
        admit_voxel_object_json("{\"schemaVersion\":", VoxelObjectRuntimeLimits::default())
            .is_err();
    let reopened = load_project(&runtime.loaded.root, &runtime.loaded.relative_path)?;
    let saved_frame = match &instance.frame {
        ProjectFrameSelection::Default => "default".to_owned(),
        ProjectFrameSelection::Clip {
            clip_id,
            frame_index,
        } => {
            format!("{clip_id}[{frame_index}]")
        }
    };

    Ok(RuntimeBehaviorEvidence {
        saved_frame,
        default_runtime_frame: object.default_frame().index,
        selected_clip: clip_id.to_owned(),
        once_terminal_frame,
        once_ended,
        repeat_wrapped_to_first_frame,
        paused_frame_stayed_stable: paused_status == VoxelObjectPlaybackStatus::Paused
            && paused_frame == paused_later_frame,
        resumed_to_later_frame: resumed_status == VoxelObjectPlaybackStatus::Playing
            && resumed_frame != paused_frame,
        posture_round_trip_matched: restored_frame == paused_later_frame
            && restored_status == paused_later_status,
        project_reopen_matched: reopened.project_hash == runtime.loaded.project_hash
            && reopened.project.instances[0].frame == instance.frame,
        missing_asset_rejected,
        corrupt_asset_rejected,
        collision_kind: collision_before.0.clone(),
        collision_voxel_data_hash: collision_before.1.clone(),
        collision_stayed_stable_during_playback: collision_before == collision_after,
        durable_project_bytes_unchanged: fs::read(&runtime.loaded.path)
            .map_err(|error| error.to_string())?
            == project_before,
        durable_object_bytes_unchanged: fs::read(&object_path)
            .map_err(|error| error.to_string())?
            == object_before,
    })
}

fn runtime_collision_policy(
    policy: &crate::model::ProjectCollisionPolicy,
) -> VoxelObjectCollisionPolicy {
    match policy {
        crate::model::ProjectCollisionPolicy::None => VoxelObjectCollisionPolicy::VisualOnly,
        crate::model::ProjectCollisionPolicy::StableFrame {
            frame: ProjectFrameSelection::Default,
        } => VoxelObjectCollisionPolicy::StableDefaultFrame,
        crate::model::ProjectCollisionPolicy::StableFrame {
            frame:
                ProjectFrameSelection::Clip {
                    clip_id,
                    frame_index,
                },
        } => VoxelObjectCollisionPolicy::StableClipFrame {
            clip: clip_id.clone(),
            frame: *frame_index,
        },
    }
}

fn collision_readout(resolution: VoxelObjectCollisionResolution<'_>) -> (String, Option<String>) {
    match resolution {
        VoxelObjectCollisionResolution::VisualOnly => ("visualOnly".to_owned(), None),
        VoxelObjectCollisionResolution::StableFrame(frame) => (
            "stableFrame".to_owned(),
            Some(frame.voxel_data_hash.clone()),
        ),
        VoxelObjectCollisionResolution::ExternalProxy(asset) => {
            ("externalProxy".to_owned(), Some(asset.to_owned()))
        }
    }
}

pub fn complete_projection(
    runtime: &RuntimeProject,
) -> Result<render_model::RenderFrameDiff, String> {
    complete_projection_with_instance_frame(runtime, None)
}

pub(crate) fn complete_projection_with_instance_frame(
    runtime: &RuntimeProject,
    frame_override: Option<(&str, u32)>,
) -> Result<render_model::RenderFrameDiff, String> {
    let mut projector = VoxelObjectRenderProjector::new();
    project_runtime_with_instance_frame(runtime, &mut projector, frame_override)
        .map(|result| result.frame)
}

pub(crate) fn complete_packed_projection(
    runtime: &RuntimeProject,
) -> Result<VoxelObjectProjectionResult, String> {
    let mut projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
    project_runtime_with_instance_frame(runtime, &mut projector, None)
}

pub(crate) fn project_runtime_with_instance_frame(
    runtime: &RuntimeProject,
    projector: &mut VoxelObjectRenderProjector,
    frame_override: Option<(&str, u32)>,
) -> Result<VoxelObjectProjectionResult, String> {
    let instances = runtime
        .loaded
        .project
        .instances
        .iter()
        .map(|instance| {
            let frame = frame_override
                .filter(|(instance_id, _)| *instance_id == instance.instance_id)
                .map(|(_, frame)| frame);
            projection_instance(runtime, instance, frame)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let mut projection = projector
        .project(&instances, &runtime.surface.render_materials)
        .map_err(|error| format!("voxel-object projection rejected: {error:?}"))?;
    projection.frame = frame_with_textures(&runtime.surface.texture_descriptors, projection.frame)?;
    Ok(projection)
}

pub fn projection_for_object(
    object: &AdmittedVoxelObject,
    frame: u32,
    materials: &[ProjectMaterial],
    label: &str,
) -> Result<VoxelObjectProjectionResult, String> {
    let mut projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
    let instance = VoxelObjectProjectionInstance {
        instance_id: "studio-candidate".to_owned(),
        object,
        frame,
        transform: Transform::IDENTITY,
        visible: true,
        material_overrides: Vec::new(),
        metadata: RenderMetadata {
            source_entity: None,
            source_scene_node: None,
            tags: vec!["voxel-object-candidate".to_owned()],
            label: Some(label.to_owned()),
        },
    };
    projector
        .project(&[instance], &render_materials(materials))
        .map_err(|error| format!("candidate projection rejected: {error:?}"))
}

fn frame_with_textures(
    textures: &[render_model::TextureDescriptor],
    frame: RenderFrameDiff,
) -> Result<RenderFrameDiff, String> {
    if textures.is_empty() {
        return Ok(frame);
    }
    let mut operations = Vec::with_capacity(textures.len() + frame.ops.len());
    operations.extend(
        textures
            .iter()
            .cloned()
            .map(|texture| RenderDiff::DefineTexture { texture }),
    );
    operations.extend(frame.ops);
    RenderFrameDiff::try_from_ops(operations)
        .map_err(|error| format!("textured voxel projection rejected: {error:?}"))
}

pub fn render_materials(
    materials: &[ProjectMaterial],
) -> BTreeMap<String, RenderMaterialDescriptor> {
    materials
        .iter()
        .map(|material| {
            (
                material.asset_id.clone(),
                RenderMaterialDescriptor {
                    schema_version: 1,
                    id: material.asset_id.clone(),
                    color: material.color,
                    texture: None,
                    roughness: material.roughness,
                    texture_tint: [1.0; 4],
                    emission_color: [0.0; 3],
                    emission_intensity: 0.0,
                    uv_strategy: MaterialUvStrategy::Flat,
                    voxel_surface: None,
                },
            )
        })
        .collect()
}

pub fn resolve_frame(
    object: &AdmittedVoxelObject,
    selection: &ProjectFrameSelection,
) -> Result<u32, String> {
    match selection {
        ProjectFrameSelection::Default => Ok(0),
        ProjectFrameSelection::Clip {
            clip_id,
            frame_index,
        } => object
            .clip(clip_id)
            .and_then(|clip| clip.frame_indices.get(*frame_index as usize))
            .copied()
            .ok_or_else(|| format!("unknown frame {clip_id}[{frame_index}]")),
    }
}

fn projection_instance<'a>(
    runtime: &'a RuntimeProject,
    instance: &ProjectVoxelObjectInstance,
    frame_override: Option<u32>,
) -> Result<VoxelObjectProjectionInstance<'a>, String> {
    let object = runtime
        .objects
        .get(&instance.voxel_object_asset_id)
        .ok_or_else(|| format!("unknown object {}", instance.voxel_object_asset_id))?;
    Ok(VoxelObjectProjectionInstance {
        instance_id: instance.instance_id.clone(),
        object,
        frame: frame_override.unwrap_or(resolve_frame(object, &instance.frame)?),
        transform: Transform {
            translation: instance.translation,
            rotation: instance.rotation,
            scale: instance.scale,
        },
        visible: true,
        material_overrides: instance
            .material_overrides
            .iter()
            .map(|binding| MeshMaterialSlot {
                slot: binding.material_slot,
                material: binding.material_asset_id.clone(),
            })
            .collect(),
        metadata: RenderMetadata {
            source_entity: Some(instance.entity_id),
            source_scene_node: Some(instance.entity_id),
            tags: vec!["voxel-object".to_owned()],
            label: Some(instance.instance_id.clone()),
        },
    })
}
