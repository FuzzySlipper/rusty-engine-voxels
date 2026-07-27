use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use voxel_convert::{AnimationAnchorPolicy, AnimationEndPolicy};

pub const PROJECT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoxelLabProject {
    pub schema_version: u32,
    pub project_id: String,
    pub name: String,
    pub entry_scene: String,
    pub revision: u64,
    pub conversion: ConversionExperiment,
    pub materials: Vec<ProjectMaterial>,
    pub voxel_objects: Vec<ProjectVoxelObject>,
    pub instances: Vec<ProjectVoxelObjectInstance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversionExperiment {
    pub source_asset_id: String,
    pub source_path: String,
    pub expected_source_sha256: String,
    pub license_path: String,
    pub target_asset_id: String,
    pub object_directory: String,
    pub resolution: [u32; 3],
    pub cell_size: f64,
    pub chunk_size: u32,
    pub pivot: [f64; 3],
    pub anchor_policy: AnimationAnchorPolicy,
    pub clips: Vec<ExperimentClip>,
    pub default_clip: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExperimentClip {
    pub source_clip_name: String,
    pub output_clip_id: String,
    pub output_name: String,
    pub sample_rate_hz: u32,
    pub start_microseconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_microseconds: Option<u64>,
    pub end_policy: AnimationEndPolicy,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectMaterial {
    pub asset_id: String,
    pub display_name: String,
    pub color: [f32; 4],
    pub roughness: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectVoxelObject {
    pub asset_id: String,
    pub path: String,
    pub expected_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectVoxelObjectInstance {
    pub entity_id: u64,
    pub instance_id: String,
    pub voxel_object_asset_id: String,
    pub frame: ProjectFrameSelection,
    pub translation: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub collision_policy: ProjectCollisionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProjectFrameSelection {
    Default,
    Clip { clip_id: String, frame_index: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProjectCollisionPolicy {
    None,
    StableFrame { frame: ProjectFrameSelection },
}

impl VoxelLabProject {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROJECT_SCHEMA_VERSION {
            return Err(format!(
                "project schema {} is unsupported; expected {PROJECT_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        require_identity(&self.project_id, "projectId")?;
        require_identity(&self.name, "name")?;
        if !self.entry_scene.starts_with("scene/") {
            return Err("entryScene must be a scene/... identity".to_owned());
        }
        let conversion = &self.conversion;
        if !conversion.source_asset_id.starts_with("mesh-animation/") {
            return Err("conversion.sourceAssetId must be mesh-animation/...".to_owned());
        }
        if !conversion.target_asset_id.starts_with("voxel-object/") {
            return Err("conversion.targetAssetId must be voxel-object/...".to_owned());
        }
        require_relative_path(&conversion.source_path, "conversion.sourcePath")?;
        require_relative_path(&conversion.license_path, "conversion.licensePath")?;
        require_relative_path(&conversion.object_directory, "conversion.objectDirectory")?;
        if !valid_sha256(&conversion.expected_source_sha256) {
            return Err("conversion.expectedSourceSha256 is not a canonical SHA-256".to_owned());
        }
        if conversion.resolution.contains(&0)
            || conversion.cell_size <= 0.0
            || !conversion.cell_size.is_finite()
            || conversion.chunk_size == 0
            || !conversion.pivot.iter().all(|value| value.is_finite())
        {
            return Err("conversion grid settings are invalid".to_owned());
        }
        let mut clip_ids = BTreeSet::new();
        for clip in &conversion.clips {
            require_identity(&clip.source_clip_name, "conversion.clips.sourceClipName")?;
            if !clip.output_clip_id.starts_with("clip/") {
                return Err(format!(
                    "output clip {} must be a clip/... identity",
                    clip.output_clip_id
                ));
            }
            require_identity(&clip.output_name, "conversion.clips.outputName")?;
            if !clip_ids.insert(clip.output_clip_id.as_str()) {
                return Err(format!("duplicate output clip {}", clip.output_clip_id));
            }
            if clip.sample_rate_hz == 0 || clip.sample_rate_hz > 240 {
                return Err(format!("invalid sample rate for {}", clip.output_clip_id));
            }
        }
        if !clip_ids.contains(conversion.default_clip.as_str()) {
            return Err("conversion.defaultClip must name a configured output clip".to_owned());
        }
        unique(
            self.materials.iter().map(|value| value.asset_id.as_str()),
            "material",
        )?;
        for material in &self.materials {
            if !material.asset_id.starts_with("material/")
                || !material
                    .color
                    .iter()
                    .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
                || !material.roughness.is_finite()
                || !(0.0..=1.0).contains(&material.roughness)
            {
                return Err(format!("invalid material {}", material.asset_id));
            }
        }
        unique(
            self.voxel_objects
                .iter()
                .map(|value| value.asset_id.as_str()),
            "voxel object",
        )?;
        for object in &self.voxel_objects {
            if !object.asset_id.starts_with("voxel-object/")
                || !valid_sha256(&object.expected_content_hash)
            {
                return Err(format!("invalid voxel object entry {}", object.asset_id));
            }
            require_relative_path(&object.path, "voxelObjects.path")?;
        }
        let object_ids = self
            .voxel_objects
            .iter()
            .map(|object| object.asset_id.as_str())
            .collect::<BTreeSet<_>>();
        unique(
            self.instances
                .iter()
                .map(|value| value.instance_id.as_str()),
            "instance",
        )?;
        let mut entity_ids = BTreeSet::new();
        for instance in &self.instances {
            if instance.entity_id == 0 || !entity_ids.insert(instance.entity_id) {
                return Err(format!(
                    "instance {} has an invalid or duplicate entity id {}",
                    instance.instance_id, instance.entity_id
                ));
            }
            require_identity(&instance.instance_id, "instances.instanceId")?;
            if !object_ids.contains(instance.voxel_object_asset_id.as_str()) {
                return Err(format!(
                    "instance {} references unknown {}",
                    instance.instance_id, instance.voxel_object_asset_id
                ));
            }
            if !instance.translation.iter().all(|value| value.is_finite())
                || !instance.rotation.iter().all(|value| value.is_finite())
                || !instance
                    .scale
                    .iter()
                    .all(|value| value.is_finite() && *value != 0.0)
            {
                return Err(format!("invalid transform for {}", instance.instance_id));
            }
        }
        Ok(())
    }
}

fn unique<'a>(values: impl Iterator<Item = &'a str>, kind: &str) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(format!("duplicate {kind} identity {value}"));
        }
    }
    Ok(())
}

fn require_identity(value: &str, path: &str) -> Result<(), String> {
    if value.trim().is_empty() || value != value.trim() {
        Err(format!("{path} must be non-empty canonical text"))
    } else {
        Ok(())
    }
}

fn require_relative_path(value: &str, field: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(format!(
            "{field} must be a normalized project-relative path"
        ))
    } else {
        Ok(())
    }
}

pub fn valid_sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn experiment_color(index: usize) -> [f32; 4] {
    const COLORS: [[f32; 4]; 8] = [
        [0.86, 0.42, 0.24, 1.0],
        [0.22, 0.48, 0.82, 1.0],
        [0.30, 0.68, 0.38, 1.0],
        [0.84, 0.72, 0.25, 1.0],
        [0.55, 0.35, 0.72, 1.0],
        [0.24, 0.72, 0.70, 1.0],
        [0.82, 0.38, 0.58, 1.0],
        [0.72, 0.72, 0.72, 1.0],
    ];
    COLORS[index % COLORS.len()]
}
