//! Static-mesh voxel density experiments ("how much detail is realistic").
//!
//! The checked animated experiments convert the Kenney retro character through
//! the Engine's animated conversion path. This module owns a complementary
//! harness for the *static* Engine path (`import_mesh_source` +
//! `plan_static_voxel_object_conversion`): it bakes one static GLB — whole or
//! as individually selected mesh pieces — at a ladder of grid resolutions and
//! records where Engine caps and practical costs land (per-frame represented
//! voxels, voxelization work, artifact bytes, admission/meshing time, mesh
//! payload). Each bake is admitted through `voxel-object-runtime` and
//! projected through `render-projection`, so density is proven end to end,
//! not just at conversion time.
//!
//! Every bake either publishes a content-addressed canonical object into the
//! spec's object directory or records a structured failure (stage + Engine
//! diagnostic) — a density ladder run never stops at the first capped rung.
//!
//! The Engine owns import, conversion, admission, and projection semantics;
//! this module owns experiment scheduling, palette assignment, publication,
//! and evidence. Static bakes declare no clips; animation behaviour is
//! covered by the animated experiments.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use voxel_asset::{
    VoxelAssetMaterialBinding, VoxelAssetMaterialMapping, VoxelConversionFitPolicy,
    VoxelConversionMode, VoxelConversionOriginPolicy, VoxelConversionSettings,
    MAX_REPRESENTED_VOXELS,
};
use voxel_convert::{
    identity_transform, import_mesh_source, plan_static_voxel_object_conversion,
    ConversionMaterialPolicy, ConversionPlanSettings, ImportedMeshSource, MeshSourceFormat,
    MeshSourceImportRequest, VoxelObjectConversionPlanRequest, VoxelObjectConversionSettings,
};
use voxel_object_runtime::{admit_voxel_object_json, VoxelObjectRuntimeLimits};

use crate::conversion::{object_path, publish_immutable};
use crate::model::{experiment_color, ProjectMaterial};
use crate::project::{atomic_write, read_bounded, safe_join, sha256, MAX_SOURCE_BYTES};
use crate::provider_pin::engine_revision;
use crate::runtime::projection_for_object;

const SILHOUETTE_RESOLUTION: u32 = 48;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DensitySpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub source: DensitySource,
    pub object_directory: String,
    pub bakes: Vec<DensityBakeSpec>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DensitySource {
    pub asset_id: String,
    pub path: String,
    pub expected_source_sha256: String,
    pub license_path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DensityBakeSpec {
    pub bake_id: String,
    pub target_asset_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_primitive: Option<String>,
    pub resolution: [u32; 3],
    pub cell_size: f64,
    pub chunk_size: u32,
    pub pivot: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DensityEvidence {
    pub schema_version: u32,
    pub engine_revision: String,
    pub experiment_id: String,
    pub source_asset_id: String,
    pub source_path: String,
    pub source_sha256: String,
    pub bakes: Vec<DensityBakeEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DensityBakeEvidence {
    pub bake_id: String,
    pub target_asset_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh_primitive: Option<String>,
    pub resolution: [u32; 3],
    pub cell_size: f64,
    pub chunk_size: u32,
    pub pivot: [f64; 3],
    #[serde(flatten)]
    pub outcome: DensityBakeOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum DensityBakeOutcome {
    Published(Box<DensityBakeMetrics>),
    Failed { stage: String, error: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DensityBakeMetrics {
    pub source_vertices: usize,
    pub source_triangles: usize,
    pub plan_hash: String,
    pub settings_sha256: String,
    pub content_hash: String,
    pub object_path: String,
    pub artifact_bytes: usize,
    pub aggregate_voxels: usize,
    pub voxelization_work: u64,
    pub max_output_voxels: u32,
    pub import_microseconds: u128,
    pub conversion_microseconds: u128,
    pub admission_microseconds: u128,
    pub resolved_voxels: usize,
    pub frame_count: u32,
    pub unique_mesh_count: u32,
    pub unique_mesh_payload_bytes: usize,
    pub mesh_vertices: u64,
    pub mesh_indices: u64,
    pub mesh_faces: u64,
    pub projection_operation_count: usize,
    pub projection_json_bytes: usize,
    pub silhouette_jaccard: f64,
}

pub fn run_density_experiment(root: &Path, relative_spec: &str) -> Result<DensityEvidence, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    let spec_path = safe_join(&root, relative_spec)?;
    let spec_text = crate::project::read_bounded_text(&spec_path, 1024 * 1024, "density spec")?;
    let spec: DensitySpec = serde_json::from_str(&spec_text)
        .map_err(|error| format!("{}: {error}", spec_path.display()))?;
    spec.validate()?;
    let source_path = safe_join(&root, &spec.source.path)?;
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "density mesh source")?;
    let actual_source_hash = sha256(&source_bytes);
    if actual_source_hash != spec.source.expected_source_sha256 {
        return Err(format!(
            "source identity drift: expected {}, computed {actual_source_hash}",
            spec.source.expected_source_sha256
        ));
    }
    let mut bakes = Vec::with_capacity(spec.bakes.len());
    for bake in &spec.bakes {
        let outcome = match run_bake(&root, &spec, bake, &source_bytes) {
            Ok(metrics) => DensityBakeOutcome::Published(Box::new(metrics)),
            Err((stage, error)) => DensityBakeOutcome::Failed { stage, error },
        };
        bakes.push(DensityBakeEvidence {
            bake_id: bake.bake_id.clone(),
            target_asset_id: bake.target_asset_id.clone(),
            mesh_primitive: bake.mesh_primitive.clone(),
            resolution: bake.resolution,
            cell_size: bake.cell_size,
            chunk_size: bake.chunk_size,
            pivot: bake.pivot,
            outcome,
        });
    }
    Ok(DensityEvidence {
        schema_version: 1,
        engine_revision: engine_revision()?,
        experiment_id: spec.experiment_id.clone(),
        source_asset_id: spec.source.asset_id.clone(),
        source_path: spec.source.path.clone(),
        source_sha256: actual_source_hash,
        bakes,
    })
}

pub fn write_density_evidence(
    root: &Path,
    relative_report: &str,
    evidence: &DensityEvidence,
) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    let path = safe_join(&root, relative_report)?;
    let bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(evidence).map_err(|error| error.to_string())?
    );
    atomic_write(&path, bytes.as_bytes())
}

fn run_bake(
    root: &Path,
    spec: &DensitySpec,
    bake: &DensityBakeSpec,
    source_bytes: &[u8],
) -> Result<DensityBakeMetrics, (String, String)> {
    let stage = |stage: &'static str| move |error: String| (stage.to_owned(), error);
    let import_started = Instant::now();
    let imported = import_mesh_source(&MeshSourceImportRequest {
        source_asset_id: spec.source.asset_id.clone(),
        asset_version: 1,
        source_path: spec.source.path.clone(),
        format: MeshSourceFormat::Glb,
        source_bytes: source_bytes.to_vec(),
        expected_source_sha256: Some(spec.source.expected_source_sha256.clone()),
        mesh_primitive: bake.mesh_primitive.clone(),
    })
    .map_err(|error| error.to_string())
    .map_err(stage("import"))?;
    let import_microseconds = import_started.elapsed().as_micros();
    let materials = bake_materials(&spec.experiment_id, &imported);
    let max_output_voxels = bake
        .resolution
        .into_iter()
        .try_fold(1_u32, u32::checked_mul)
        .ok_or_else(|| stage("plan")("conversion resolution product overflows u32".to_owned()))?
        .min(MAX_REPRESENTED_VOXELS as u32);
    let request = VoxelObjectConversionPlanRequest {
        source: imported.receipt.source.clone(),
        source_path: spec.source.path.clone(),
        target_asset_id: bake.target_asset_id.clone(),
        license_path: Some(spec.source.license_path.clone()),
        settings: VoxelObjectConversionSettings {
            mesh: ConversionPlanSettings {
                conversion: VoxelConversionSettings {
                    resolution: bake.resolution,
                    cell_size: bake.cell_size,
                    chunk_size: bake.chunk_size,
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
            pivot: bake.pivot,
            anchor_policy: voxel_convert::AnimationAnchorPolicy::PreserveSourceSpace,
        },
        clips: Vec::new(),
        default_clip: None,
    };
    let conversion_started = Instant::now();
    let prepared = plan_static_voxel_object_conversion(&request, &imported)
        .map_err(|error| error.to_string())
        .map_err(stage("plan"))?;
    let conversion_microseconds = conversion_started.elapsed().as_micros();
    let candidate = prepared.candidate();
    let output_relative = object_path(
        &spec.object_directory,
        &candidate.asset.asset_id,
        &candidate.content_hash,
    )
    .map_err(stage("publish"))?;
    let output_path = safe_join(root, &output_relative).map_err(stage("publish"))?;
    publish_immutable(&output_path, candidate.canonical_json.as_bytes())
        .map_err(stage("publish"))?;
    let admission_started = Instant::now();
    let object = admit_voxel_object_json(
        &candidate.canonical_json,
        VoxelObjectRuntimeLimits::default(),
    )
    .map_err(|error| error.to_string())
    .map_err(stage("admit"))?;
    let admission_microseconds = admission_started.elapsed().as_micros();
    let readout = object.readout();
    let resolved_voxels = object.frames().iter().map(|frame| frame.cells.len()).sum();
    let mut unique_mesh_payload_bytes = 0usize;
    let mut mesh_vertices = 0u64;
    let mut mesh_indices = 0u64;
    let mut mesh_faces = 0u64;
    for mesh in object.meshes() {
        unique_mesh_payload_bytes = unique_mesh_payload_bytes
            .saturating_add(std::mem::size_of_val(mesh.positions.as_slice()))
            .saturating_add(std::mem::size_of_val(mesh.normals.as_slice()))
            .saturating_add(std::mem::size_of_val(mesh.indices.as_slice()))
            .saturating_add(std::mem::size_of_val(mesh.groups.as_slice()));
        mesh_vertices = mesh_vertices.saturating_add(u64::from(mesh.stats.vertices));
        mesh_indices = mesh_indices.saturating_add(u64::from(mesh.stats.indices));
        mesh_faces = mesh_faces.saturating_add(u64::from(mesh.stats.faces_emitted));
    }
    let projection = projection_for_object(&object, 0, &materials.project, &bake.bake_id)
        .map_err(stage("project"))?;
    let projection_json_bytes = serde_json::to_vec(&projection.frame)
        .map_err(|error| error.to_string())
        .map_err(stage("project"))?
        .len();
    let silhouette_jaccard =
        silhouette_fidelity(&imported, object.frames().first()).map_err(stage("silhouette"))?;
    Ok(DensityBakeMetrics {
        source_vertices: candidate.source_vertices,
        source_triangles: candidate.source_triangles,
        plan_hash: prepared.plan().plan_hash.clone(),
        settings_sha256: candidate.settings_sha256.clone(),
        content_hash: candidate.content_hash.clone(),
        object_path: output_relative,
        artifact_bytes: candidate.artifact_bytes,
        aggregate_voxels: candidate.aggregate_voxels,
        voxelization_work: candidate.voxelization_work,
        max_output_voxels,
        import_microseconds,
        conversion_microseconds,
        admission_microseconds,
        resolved_voxels,
        frame_count: readout.frame_count,
        unique_mesh_count: readout.unique_mesh_count,
        unique_mesh_payload_bytes,
        mesh_vertices,
        mesh_indices,
        mesh_faces,
        projection_operation_count: projection.frame.ops.len(),
        projection_json_bytes,
        silhouette_jaccard,
    })
}

struct BakeMaterials {
    palette: Vec<VoxelAssetMaterialBinding>,
    mappings: Vec<VoxelAssetMaterialMapping>,
    project: Vec<ProjectMaterial>,
}

fn bake_materials(experiment_id: &str, imported: &ImportedMeshSource) -> BakeMaterials {
    imported
        .mesh
        .materials
        .iter()
        .enumerate()
        .map(|(index, material)| {
            let material_slot = u16::try_from(index + 1)
                .expect("density sources have far fewer than u16::MAX materials");
            let asset_id = format!(
                "material/{experiment_id}-slot-{}",
                material.source_material_slot
            );
            let display_name = material
                .source_material_name
                .clone()
                .unwrap_or_else(|| format!("Source material {}", material.source_material_slot));
            (
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
            )
        })
        .fold(
            BakeMaterials {
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
}

/// Front-view silhouette fidelity: rasterize the imported source triangles
/// and the baked frame's occupied cells into the same coarse grid (each
/// normalized over its own front-view bounds) and compare them with a
/// Jaccard index. This is the static-bake counterpart of the animated
/// quality readouts.
fn silhouette_fidelity(
    imported: &ImportedMeshSource,
    frame: Option<&voxel_object_runtime::VoxelObjectRuntimeFrame>,
) -> Result<f64, String> {
    let frame = frame.ok_or("admitted object has no frames")?;
    let mesh = &imported.mesh;
    let first = mesh
        .positions
        .first()
        .ok_or("imported mesh has no positions")?;
    let mut min = *first;
    let mut max = *first;
    for position in &mesh.positions {
        for axis in 0..3 {
            min[axis] = min[axis].min(position[axis]);
            max[axis] = max[axis].max(position[axis]);
        }
    }
    let span = [max[0] - min[0], max[1] - min[1]];
    if span.iter().any(|value| *value <= f64::EPSILON) {
        return Err("imported mesh has a degenerate front-view extent".to_owned());
    }
    let last = f64::from(SILHOUETTE_RESOLUTION - 1);
    let mut source_cells: BTreeSet<(u32, u32)> = BTreeSet::new();
    for triangle in &mesh.triangles {
        let points = triangle.indices.map(|index| {
            let position = mesh.positions[index as usize];
            (
                ((position[0] - min[0]) / span[0] * last)
                    .round()
                    .clamp(0.0, last) as u32,
                ((position[1] - min[1]) / span[1] * last)
                    .round()
                    .clamp(0.0, last) as u32,
            )
        });
        rasterize_triangle(points, &mut source_cells);
    }
    let first_cell = frame
        .cells
        .first()
        .ok_or("admitted frame has no cells")?
        .coordinate;
    let mut cell_min = first_cell;
    let mut cell_max = first_cell;
    for cell in frame.cells.iter() {
        for axis in 0..3 {
            cell_min[axis] = cell_min[axis].min(cell.coordinate[axis]);
            cell_max[axis] = cell_max[axis].max(cell.coordinate[axis]);
        }
    }
    let cell_span = [
        cell_max[0].saturating_sub(cell_min[0]),
        cell_max[1].saturating_sub(cell_min[1]),
    ];
    if cell_span.iter().any(|value| *value <= 0) {
        return Err("admitted frame has a degenerate front-view extent".to_owned());
    }
    let mut voxel_cells: BTreeSet<(u32, u32)> = BTreeSet::new();
    for cell in frame.cells.iter() {
        let x = (cell.coordinate[0] - cell_min[0]) as f64 / cell_span[0] as f64 * last;
        let y = (cell.coordinate[1] - cell_min[1]) as f64 / cell_span[1] as f64 * last;
        voxel_cells.insert((
            x.round().clamp(0.0, last) as u32,
            y.round().clamp(0.0, last) as u32,
        ));
    }
    let intersection = source_cells.intersection(&voxel_cells).count();
    let union = source_cells.union(&voxel_cells).count();
    if union == 0 {
        return Err("silhouette comparison produced no cells".to_owned());
    }
    #[allow(clippy::cast_precision_loss)]
    Ok(((intersection as f64 / union as f64) * 10_000.0).round() / 10_000.0)
}

fn rasterize_triangle(points: [(u32, u32); 3], output: &mut BTreeSet<(u32, u32)>) {
    let min_x = points.iter().map(|point| point.0).min().unwrap_or(0);
    let max_x = points.iter().map(|point| point.0).max().unwrap_or(0);
    let min_y = points.iter().map(|point| point.1).min().unwrap_or(0);
    let max_y = points.iter().map(|point| point.1).max().unwrap_or(0);
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if point_in_triangle((x, y), points) {
                output.insert((x, y));
            }
        }
    }
}

fn point_in_triangle(point: (u32, u32), triangle: [(u32, u32); 3]) -> bool {
    let cross = |a: (u32, u32), b: (u32, u32)| {
        (i64::from(b.0) - i64::from(a.0)) * (i64::from(point.1) - i64::from(a.1))
            - (i64::from(b.1) - i64::from(a.1)) * (i64::from(point.0) - i64::from(a.0))
    };
    let d0 = cross(triangle[0], triangle[1]);
    let d1 = cross(triangle[1], triangle[2]);
    let d2 = cross(triangle[2], triangle[0]);
    let has_negative = d0 < 0 || d1 < 0 || d2 < 0;
    let has_positive = d0 > 0 || d1 > 0 || d2 > 0;
    !(has_negative && has_positive)
}

impl DensitySpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "density spec schema {} is unsupported; expected 1",
                self.schema_version
            ));
        }
        if self.experiment_id.is_empty()
            || !self
                .experiment_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err("experimentId must be a non-empty identity".to_owned());
        }
        if !self.source.asset_id.starts_with("mesh/") {
            return Err("source.assetId must be a mesh/... static mesh identity".to_owned());
        }
        for (field, value) in [
            ("source.path", self.source.path.as_str()),
            ("source.licensePath", self.source.license_path.as_str()),
            ("objectDirectory", self.object_directory.as_str()),
        ] {
            let path = Path::new(value);
            if value.is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(format!("{field} must be a safe relative path"));
            }
        }
        if !self
            .source
            .expected_source_sha256
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err("source.expectedSourceSha256 is not a canonical SHA-256".to_owned());
        }
        if self.bakes.is_empty() {
            return Err("bakes must not be empty".to_owned());
        }
        let mut bake_ids = BTreeSet::new();
        let mut target_ids = BTreeSet::new();
        for bake in &self.bakes {
            if bake.bake_id.is_empty()
                || !bake
                    .bake_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                return Err(format!("bake id {:?} is not an identity", bake.bake_id));
            }
            if !bake_ids.insert(bake.bake_id.as_str()) {
                return Err(format!("duplicate bake id {}", bake.bake_id));
            }
            if !bake.target_asset_id.starts_with("voxel-object/") {
                return Err(format!(
                    "bake {} targetAssetId must be a voxel-object/... identity",
                    bake.bake_id
                ));
            }
            if !target_ids.insert(bake.target_asset_id.as_str()) {
                return Err(format!("duplicate targetAssetId {}", bake.target_asset_id));
            }
            if bake.resolution.contains(&0)
                || bake.cell_size <= 0.0
                || !bake.cell_size.is_finite()
                || bake.chunk_size == 0
                || !bake.pivot.iter().all(|value| value.is_finite())
            {
                return Err(format!("bake {} grid settings are invalid", bake.bake_id));
            }
            if let Some(primitive) = &bake.mesh_primitive {
                let valid = primitive
                    .strip_prefix("node/")
                    .is_some_and(|value| value.parse::<u32>().is_ok())
                    || primitive
                        .strip_prefix("group/")
                        .is_some_and(|value| value.parse::<usize>().is_ok());
                if !valid {
                    return Err(format!(
                        "bake {} meshPrimitive must name one node or group such as node/0",
                        bake.bake_id
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_spec() -> DensitySpec {
        DensitySpec {
            schema_version: 1,
            experiment_id: "unit-density".to_owned(),
            source: DensitySource {
                asset_id: "mesh/unit".to_owned(),
                path: "content/sources/unit/unit.glb".to_owned(),
                expected_source_sha256: format!("sha256:{}", "0".repeat(64)),
                license_path: "content/sources/unit/LICENSE.txt".to_owned(),
            },
            object_directory: ".density-cache/objects".to_owned(),
            bakes: vec![DensityBakeSpec {
                bake_id: "whole-64".to_owned(),
                target_asset_id: "voxel-object/unit-64".to_owned(),
                mesh_primitive: None,
                resolution: [32, 64, 32],
                cell_size: 0.01,
                chunk_size: 16,
                pivot: [15.5, 0.0, 15.5],
            }],
        }
    }

    #[test]
    fn valid_spec_passes() {
        valid_spec().validate().expect("valid spec");
    }

    #[test]
    fn animated_source_identity_is_rejected() {
        let mut spec = valid_spec();
        spec.source.asset_id = "mesh-animation/unit".to_owned();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn zero_resolution_is_rejected() {
        let mut spec = valid_spec();
        spec.bakes[0].resolution = [0, 64, 32];
        assert!(spec.validate().is_err());
    }

    #[test]
    fn duplicate_bake_ids_are_rejected() {
        let mut spec = valid_spec();
        let mut second = spec.bakes[0].clone();
        second.target_asset_id = "voxel-object/unit-64b".to_owned();
        spec.bakes.push(second);
        assert!(spec.validate().is_err());
    }

    #[test]
    fn unsafe_paths_are_rejected() {
        let mut spec = valid_spec();
        spec.object_directory = "../outside".to_owned();
        assert!(spec.validate().is_err());
        let mut spec = valid_spec();
        spec.source.path = "/absolute.glb".to_owned();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn malformed_mesh_primitive_is_rejected() {
        let mut spec = valid_spec();
        spec.bakes[0].mesh_primitive = Some("piece/0".to_owned());
        assert!(spec.validate().is_err());
        spec.bakes[0].mesh_primitive = Some("node/3".to_owned());
        spec.validate().expect("node/3 is valid");
    }
}
