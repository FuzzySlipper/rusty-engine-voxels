use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use voxel_asset::{
    VoxelAssetMaterialBinding, VoxelAssetMaterialMapping, VoxelConversionFitPolicy,
    VoxelConversionMode, VoxelConversionOriginPolicy, VoxelConversionSettings,
    MAX_REPRESENTED_VOXELS,
};
use voxel_convert::{
    identity_transform, import_animated_mesh_source, plan_animated_voxel_object_conversion,
    AnimationEndPolicy, ConversionMaterialPolicy, ConversionPlanSettings, MeshSourceFormat,
    MeshSourceImportRequest, PreparedVoxelObjectConversion, VoxelObjectClipConversionRequest,
    VoxelObjectConversionPlanRequest, VoxelObjectConversionSettings,
};

use crate::model::{experiment_color, ProjectMaterial, ProjectVoxelObject};
use crate::project::{
    atomic_write, load_project, read_bounded, safe_join, save_project, sha256, LoadedProject,
    MAX_SOURCE_BYTES,
};
use crate::provider_pin::engine_revision;
use crate::quality::{analyze_prepared_quality, VoxelQualityEvidence};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipConversionEvidence {
    pub clip_id: String,
    pub source_clip: String,
    pub sampled_frames: usize,
    pub stored_frames: usize,
    pub duration_microseconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversionEvidence {
    pub engine_revision: String,
    pub source_sha256: String,
    pub plan_hash: String,
    pub settings_sha256: String,
    pub content_hash: String,
    pub source_vertices: usize,
    pub source_triangles: usize,
    pub deformation_work: u64,
    pub voxelization_work: u64,
    pub sampled_frames: usize,
    pub stored_frames: usize,
    pub aggregate_voxels: usize,
    pub artifact_bytes: usize,
    pub output_path: String,
    pub project_hash: String,
    pub source_import_microseconds: u128,
    pub conversion_microseconds: u128,
    pub amortized_conversion_microseconds_per_sampled_frame: u128,
    pub clips: Vec<ClipConversionEvidence>,
}

pub struct PreparedProjectConversion {
    pub loaded: LoadedProject,
    pub prepared: PreparedVoxelObjectConversion,
    pub materials: Vec<ProjectMaterial>,
    pub source_import_microseconds: u128,
    pub conversion_microseconds: u128,
    pub quality: Option<VoxelQualityEvidence>,
}

struct ConversionMaterials {
    palette: Vec<VoxelAssetMaterialBinding>,
    mappings: Vec<VoxelAssetMaterialMapping>,
    project: Vec<ProjectMaterial>,
}

pub fn prepare_project_conversion(
    root: &Path,
    relative_project: &str,
) -> Result<PreparedProjectConversion, String> {
    let loaded = load_project(root, relative_project)?;
    let source_path = safe_join(&loaded.root, &loaded.project.conversion.source_path)?;
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "animated mesh source")?;
    let actual_source_hash = sha256(&source_bytes);
    if actual_source_hash != loaded.project.conversion.expected_source_sha256 {
        return Err(format!(
            "source identity drift: expected {}, computed {actual_source_hash}",
            loaded.project.conversion.expected_source_sha256
        ));
    }
    let import_started = Instant::now();
    let imported = import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: loaded.project.conversion.source_asset_id.clone(),
        asset_version: 1,
        source_path: loaded.project.conversion.source_path.clone(),
        format: MeshSourceFormat::Glb,
        source_bytes,
        expected_source_sha256: Some(actual_source_hash),
        mesh_primitive: None,
    })
    .map_err(|error| error.to_string())?;
    let source_import_microseconds = import_started.elapsed().as_micros();
    let materials = conversion_materials(&imported.source.mesh.materials)?;
    let conversion = &loaded.project.conversion;
    // The grid product is the natural per-pose output bound, but the engine also
    // caps representable voxels per frame; clamp so finer grids remain admissible.
    let max_output_voxels = conversion
        .resolution
        .into_iter()
        .try_fold(1_u32, u32::checked_mul)
        .ok_or("conversion resolution product overflows u32")?
        .min(MAX_REPRESENTED_VOXELS as u32);
    let request = VoxelObjectConversionPlanRequest {
        source: imported.source.receipt.source.clone(),
        source_path: conversion.source_path.clone(),
        target_asset_id: conversion.target_asset_id.clone(),
        license_path: Some(conversion.license_path.clone()),
        settings: VoxelObjectConversionSettings {
            mesh: ConversionPlanSettings {
                conversion: VoxelConversionSettings {
                    resolution: conversion.resolution,
                    cell_size: conversion.cell_size,
                    chunk_size: conversion.chunk_size,
                    origin: [0, 0, 0],
                    fit_policy: VoxelConversionFitPolicy::Contain,
                    origin_policy: VoxelConversionOriginPolicy::Centered,
                    mode: VoxelConversionMode::Surface,
                    material_palette: materials.palette,
                    material_map: materials.mappings,
                    max_output_voxels,
                },
                transform: identity_transform(),
                material_policy: ConversionMaterialPolicy::default(),
            },
            pivot: conversion.pivot,
            anchor_policy: conversion.anchor_policy,
        },
        clips: conversion
            .clips
            .iter()
            .map(|clip| VoxelObjectClipConversionRequest {
                source_clip_name: clip.source_clip_name.clone(),
                output_clip_id: clip.output_clip_id.clone(),
                output_name: Some(clip.output_name.clone()),
                sample_rate_hz: clip.sample_rate_hz,
                start_microseconds: clip.start_microseconds,
                end_microseconds: clip.end_microseconds,
                end_policy: clip.end_policy,
            })
            .collect(),
        default_clip: Some(conversion.default_clip.clone()),
    };
    let started = Instant::now();
    let prepared = plan_animated_voxel_object_conversion(&request, &imported)
        .map_err(|error| error.to_string())?;
    let conversion_microseconds = started.elapsed().as_micros();
    let quality = analyze_prepared_quality(
        &imported,
        prepared.candidate(),
        &loaded.project.conversion.anchor_policy,
    )?;
    Ok(PreparedProjectConversion {
        loaded,
        prepared,
        materials: materials.project,
        source_import_microseconds,
        conversion_microseconds,
        quality: Some(quality),
    })
}

pub fn publish_project_conversion(
    prepared: PreparedProjectConversion,
) -> Result<ConversionEvidence, String> {
    let candidate = prepared.prepared.candidate();
    let output_relative = object_path(
        &prepared.loaded.project.conversion.object_directory,
        &candidate.asset.asset_id,
        &candidate.content_hash,
    )?;
    let output_path = safe_join(&prepared.loaded.root, &output_relative)?;
    publish_immutable(&output_path, candidate.canonical_json.as_bytes())?;

    let mut project = prepared.loaded.project.clone();
    for material in prepared.materials {
        project
            .materials
            .retain(|entry| entry.asset_id != material.asset_id);
        project.materials.push(material);
    }
    project
        .materials
        .sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
    project
        .voxel_objects
        .retain(|entry| entry.asset_id != candidate.asset.asset_id);
    project.voxel_objects.push(ProjectVoxelObject {
        asset_id: candidate.asset.asset_id.clone(),
        path: output_relative.clone(),
        expected_content_hash: candidate.content_hash.clone(),
    });
    project
        .voxel_objects
        .sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
    let saved = if project == prepared.loaded.project {
        prepared.loaded.clone()
    } else {
        project.revision = project.revision.saturating_add(1);
        save_project(&prepared.loaded, &project)?
    };

    Ok(ConversionEvidence {
        engine_revision: engine_revision()?,
        source_sha256: candidate.source_sha256.clone(),
        plan_hash: prepared.prepared.plan().plan_hash.clone(),
        settings_sha256: candidate.settings_sha256.clone(),
        content_hash: candidate.content_hash.clone(),
        source_vertices: candidate.source_vertices,
        source_triangles: candidate.source_triangles,
        deformation_work: candidate.deformation_work,
        voxelization_work: candidate.voxelization_work,
        sampled_frames: candidate.sampled_frames,
        stored_frames: candidate.stored_frames,
        aggregate_voxels: candidate.aggregate_voxels,
        artifact_bytes: candidate.artifact_bytes,
        output_path: output_relative,
        project_hash: saved.project_hash,
        source_import_microseconds: prepared.source_import_microseconds,
        conversion_microseconds: prepared.conversion_microseconds,
        amortized_conversion_microseconds_per_sampled_frame: prepared.conversion_microseconds
            / candidate.sampled_frames.max(1) as u128,
        clips: candidate
            .clips
            .iter()
            .map(|clip| ClipConversionEvidence {
                clip_id: clip.output_clip_id.clone(),
                source_clip: clip.source_clip_name.clone(),
                sampled_frames: clip.sampled_frame_count,
                stored_frames: clip.stored_frame_count,
                duration_microseconds: clip.duration_microseconds,
            })
            .collect(),
    })
}

pub fn convert_project(root: &Path, relative_project: &str) -> Result<ConversionEvidence, String> {
    publish_project_conversion(prepare_project_conversion(root, relative_project)?)
}

fn conversion_materials(
    source: &[voxel_convert::ImportedMaterial],
) -> Result<ConversionMaterials, String> {
    source
        .iter()
        .enumerate()
        .map(|(index, material)| {
            let material_slot = u16::try_from(index + 1)
                .map_err(|_| "source has too many materials for voxel slots".to_owned())?;
            let asset_id = format!("material/retro-slot-{}", material.source_material_slot);
            let display_name = material
                .source_material_name
                .clone()
                .unwrap_or_else(|| format!("Source material {}", material.source_material_slot));
            Ok((
                VoxelAssetMaterialBinding {
                    material_slot,
                    material_asset_id: asset_id.clone(),
                    display_name: Some(display_name.clone()),
                },
                VoxelAssetMaterialMapping {
                    source_material_slot: material.source_material_slot,
                    source_material_name: material.source_material_name.clone(),
                    voxel_material_slot: material_slot,
                },
                ProjectMaterial::flat(asset_id, display_name, experiment_color(index), 0.82),
            ))
        })
        .collect::<Result<Vec<_>, String>>()
        .map(|values| {
            values.into_iter().fold(
                ConversionMaterials {
                    palette: Vec::new(),
                    mappings: Vec::new(),
                    project: Vec::new(),
                },
                |mut materials, (binding, mapping, material)| {
                    materials.palette.push(binding);
                    materials.mappings.push(mapping);
                    materials.project.push(material);
                    materials
                },
            )
        })
}

fn object_path(directory: &str, asset_id: &str, content_hash: &str) -> Result<String, String> {
    let name = asset_id
        .strip_prefix("voxel-object/")
        .ok_or("voxel object identity has no voxel-object/ prefix")?
        .replace('/', "-");
    let hash = content_hash
        .strip_prefix("sha256:")
        .ok_or("voxel object content hash is malformed")?;
    Ok(format!("{directory}/{name}-{hash}.voxel-object.json"))
}

fn publish_immutable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() {
        let existing =
            std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if existing == bytes {
            return Ok(());
        }
        return Err(format!(
            "content-addressed object already exists with different bytes: {}",
            path.display()
        ));
    }
    atomic_write(path, bytes)
}

pub fn report_path(root: &Path) -> PathBuf {
    root.join("evidence/initial-animated-voxel-report.json")
}

pub fn clip_end_policy_name(policy: AnimationEndPolicy) -> &'static str {
    match policy {
        AnimationEndPolicy::IncludeClipEnd => "includeClipEnd",
        AnimationEndPolicy::ExcludeLoopSeam => "excludeLoopSeam",
    }
}
