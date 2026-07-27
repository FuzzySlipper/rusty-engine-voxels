use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use render_model::{
    MaterialUvStrategy, RenderDiff, RenderMaterialDescriptor, RenderMetadata, Transform,
};
use render_projection::{VoxelObjectProjectionInstance, VoxelObjectRenderProjector};
use serde::Serialize;
use voxel_object_runtime::{
    admit_voxel_object_json, AdmittedVoxelObject, VoxelObjectLoopMode, VoxelObjectPlaybackRate,
    VoxelObjectPlayer, VoxelObjectRuntimeLimits,
};

use crate::conversion::ENGINE_REVISION;
use crate::model::{ProjectFrameSelection, ProjectMaterial, ProjectVoxelObjectInstance};
use crate::project::{load_project, read_bounded_text, safe_join, LoadedProject, MAX_OBJECT_BYTES};

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
    pub playback_clip: String,
    pub playback_samples: Vec<PlaybackSampleEvidence>,
}

pub struct RuntimeProject {
    pub loaded: LoadedProject,
    pub objects: BTreeMap<String, AdmittedVoxelObject>,
}

pub fn load_runtime_project(root: &Path, relative_project: &str) -> Result<RuntimeProject, String> {
    let loaded = load_project(root, relative_project)?;
    let mut objects = BTreeMap::new();
    for entry in &loaded.project.voxel_objects {
        let path = safe_join(&loaded.root, &entry.path)?;
        let text = read_bounded_text(&path, MAX_OBJECT_BYTES, "voxel object")?;
        let object = admit_voxel_object_json(&text, VoxelObjectRuntimeLimits::default())
            .map_err(|error| format!("{}: {error}", path.display()))?;
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
    }
    Ok(RuntimeProject { loaded, objects })
}

pub fn verify_runtime_project(
    root: &Path,
    relative_project: &str,
) -> Result<RuntimeEvidence, String> {
    let started = Instant::now();
    let runtime = load_runtime_project(root, relative_project)?;
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
    Ok(RuntimeEvidence {
        engine_revision: ENGINE_REVISION.to_owned(),
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
        load_milliseconds: started.elapsed().as_millis(),
        playback_clip: clip,
        playback_samples,
    })
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
    projector
        .project(
            &instances,
            &render_materials(&runtime.loaded.project.materials),
        )
        .map(|result| result.frame)
        .map_err(|error| format!("voxel-object projection rejected: {error:?}"))
}

pub fn projection_for_object(
    object: &AdmittedVoxelObject,
    frame: u32,
    materials: &[ProjectMaterial],
    label: &str,
) -> Result<render_model::RenderFrameDiff, String> {
    let mut projector = VoxelObjectRenderProjector::new();
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
        .map(|result| result.frame)
        .map_err(|error| format!("candidate projection rejected: {error:?}"))
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
        material_overrides: Vec::new(),
        metadata: RenderMetadata {
            source_entity: Some(instance.entity_id),
            source_scene_node: Some(instance.entity_id),
            tags: vec!["voxel-object".to_owned()],
            label: Some(instance.instance_id.clone()),
        },
    })
}
