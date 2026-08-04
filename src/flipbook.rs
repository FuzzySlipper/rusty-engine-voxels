//! Compilation of finished exploded-kit frames into the canonical Engine
//! voxel-object format.
//!
//! This is an offline authoring boundary. The output contains complete
//! immutable frames, timing, named local anchors, and coarse authored
//! collision facts. Runtime playback remains owned by `voxel-object-runtime`;
//! no rig, scheduler, or alternate animation format is introduced here.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use voxel_asset::{
    encode_voxel_object, with_computed_voxel_frame_hash, with_computed_voxel_object_hashes,
    VoxelAssetBounds, VoxelAssetMaterialBinding, VoxelAssetMaterialMapping, VoxelCoordinateSystem,
    VoxelFrame, VoxelObjectAnimationFrame, VoxelObjectAsset, VoxelObjectClip,
    VoxelObjectCollisionPrimitive, VoxelObjectFrameAnchor, VoxelObjectFrameCollision,
    VoxelObjectGrid, VoxelObjectHitRegion, VoxelObjectProvenance, VoxelObjectProvenanceKind,
    VoxelRepresentation, VoxelRepresentationKind, VoxelSparseRun, VOXEL_OBJECT_SCHEMA_VERSION,
};

use crate::assemble::{socket_constrained_part_placements, RoughFrame, SelectedPose};
use crate::fusion::{FusedFrame, FusionContext};
use crate::kit::VoxelKit;
use crate::project::{atomic_write, safe_join, sha256};

const COMPILER_ID: &str = "rusty-engine-voxels.exploded-kit-flipbook.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FrameFactSource {
    PartPivot {
        part_id: String,
    },
    PartSocket {
        part_id: String,
        socket_id: String,
    },
    PartVoxel {
        part_id: String,
        source_voxel_index: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameAnchorSpec {
    pub id: String,
    pub source: FrameFactSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum HitRegionShape {
    Box { half_extents: [f64; 3] },
    Capsule { radius: f64, half_height: f64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HitRegionSpec {
    pub id: String,
    pub center: FrameFactSource,
    pub shape: HitRegionShape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FlipbookCompileSettings {
    pub asset_id: String,
    pub clip_id: String,
    pub clip_name: String,
    pub source_path: String,
    pub chunk_size: u32,
    pub anchors: Vec<FrameAnchorSpec>,
    pub body_collision: Option<VoxelObjectCollisionPrimitive>,
    pub hit_regions: Vec<HitRegionSpec>,
}

#[derive(Debug, Clone)]
pub struct CompiledFlipbook {
    pub asset: VoxelObjectAsset,
    pub canonical_json: String,
    pub source_sha256: String,
    pub settings_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlipbookPublication {
    pub asset_id: String,
    pub content_hash: String,
    pub path: String,
    pub byte_count: usize,
}

/// Compile a complete fused schedule into one canonical immutable object.
///
/// # Errors
///
/// Returns an error before publication when inputs are incomplete, frame facts
/// cannot resolve against their named source parts, or Engine admission rejects
/// the candidate.
pub fn compile_flipbook(
    context: FusionContext<'_>,
    selected: &[SelectedPose],
    fused: &[FusedFrame],
    settings: &FlipbookCompileSettings,
    source_bytes: &[u8],
) -> Result<CompiledFlipbook, String> {
    if selected.is_empty() || selected.len() != fused.len() {
        return Err(
            "flipbook schedule must contain matching non-empty pose and frame lists".into(),
        );
    }
    if source_bytes.is_empty() {
        return Err("flipbook source bytes must not be empty".into());
    }
    let settings_json = serde_json::to_vec(settings).map_err(|error| error.to_string())?;
    let source_sha256 = sha256(source_bytes);
    let settings_sha256 = sha256(&settings_json);
    let palette = material_palette(context.kit)?;
    let palette_slots = palette.iter().map(|binding| binding.material_slot);
    let material_map = material_map(context.kit);
    let mut animation_frames = Vec::with_capacity(fused.len());

    for (pose, frame) in selected.iter().zip(fused) {
        if pose.time_microseconds != frame.time_microseconds
            || pose.duration_microseconds != frame.duration_microseconds
            || frame.duration_microseconds == 0
        {
            return Err("fused frame timing does not match its selected pose".into());
        }
        let placements = socket_constrained_part_placements(
            context.kit,
            context.rig_map,
            context.model,
            context.clip_index,
            pose.time_microseconds,
        )
        .map_err(|error| error.to_string())?;
        let anchors = settings
            .anchors
            .iter()
            .map(|anchor| {
                Ok(VoxelObjectFrameAnchor {
                    id: anchor.id.clone(),
                    position: resolve_fact_source(context.kit, &placements, &anchor.source)?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let hit_regions = settings
            .hit_regions
            .iter()
            .map(|region| {
                let center = resolve_fact_source(context.kit, &placements, &region.center)?;
                let primitive = match region.shape {
                    HitRegionShape::Box { half_extents } => VoxelObjectCollisionPrimitive::Box {
                        center,
                        half_extents,
                    },
                    HitRegionShape::Capsule {
                        radius,
                        half_height,
                    } => VoxelObjectCollisionPrimitive::Capsule {
                        center,
                        radius,
                        half_height,
                    },
                };
                Ok(VoxelObjectHitRegion {
                    id: region.id.clone(),
                    primitive,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let collision = (settings.body_collision.is_some() || !hit_regions.is_empty()).then(|| {
            VoxelObjectFrameCollision {
                body: settings.body_collision.clone(),
                hit_regions,
            }
        });
        animation_frames.push(VoxelObjectAnimationFrame {
            duration_seconds: Some(frame.duration_microseconds as f64 / 1_000_000.0),
            anchors,
            collision,
            frame: voxel_frame(frame, palette_slots.clone())?,
        });
    }

    finalize_compiled_flipbook(
        settings,
        source_sha256,
        settings_sha256,
        source_bytes
            .len()
            .try_into()
            .map_err(|_| "source byte count does not fit u64")?,
        palette,
        material_map,
        animation_frames,
        context.kit.convention.voxel_size_meters,
        inferred_frame_rate(fused),
    )
}

/// Compile rig-free authored pose frames into one canonical immutable object.
///
/// This is the rig-free sibling of [`compile_flipbook`] for manually authored
/// pose specs (`crate::posed`): the frames are already complete rough
/// assemblies, so no rig, fusion context, or frame-fact resolution is
/// involved. Anchors, hit regions, and body collision are not resolvable
/// without placements and must be empty in `settings`.
///
/// Frames are run-length encoded along +X (posed frames carry no fusion
/// provenance worth preserving in the artifact), which keeps kit-scale
/// characters well inside the Engine's 64 MiB artifact bound.
///
/// The compiled object's cell size is explicit (not taken from the kit) so
/// pose specs can compile at a downsampled lattice rate.
///
/// # Errors
///
/// Returns an error when the frame list is empty, a frame has no duration or
/// no voxels, the cell size is not finite and positive, frame facts were
/// requested, or Engine admission rejects the candidate.
pub fn compile_posed_flipbook(
    kit: &VoxelKit,
    frames: &[RoughFrame],
    settings: &FlipbookCompileSettings,
    source_bytes: &[u8],
    cell_size_meters: f64,
) -> Result<CompiledFlipbook, String> {
    if frames.is_empty() {
        return Err("posed flipbook must contain at least one frame".into());
    }
    if source_bytes.is_empty() {
        return Err("flipbook source bytes must not be empty".into());
    }
    if !cell_size_meters.is_finite() || cell_size_meters <= 0.0 {
        return Err("posed flipbook cell size must be finite and positive".into());
    }
    if !settings.anchors.is_empty()
        || !settings.hit_regions.is_empty()
        || settings.body_collision.is_some()
    {
        return Err(
            "posed flipbook compilation does not resolve frame facts; anchors, hit regions, and body collision must be empty"
                .into(),
        );
    }
    let settings_json = serde_json::to_vec(settings).map_err(|error| error.to_string())?;
    let source_sha256 = sha256(source_bytes);
    let settings_sha256 = sha256(&settings_json);
    let palette = material_palette(kit)?;
    let palette_slots = palette.iter().map(|binding| binding.material_slot);
    let material_map = material_map(kit);
    let mut animation_frames = Vec::with_capacity(frames.len());
    for frame in frames {
        if frame.duration_microseconds == 0 {
            return Err("posed flipbook frames must have non-zero durations".into());
        }
        animation_frames.push(VoxelObjectAnimationFrame {
            duration_seconds: Some(frame.duration_microseconds as f64 / 1_000_000.0),
            anchors: Vec::new(),
            collision: None,
            frame: voxel_frame_runs(frame, palette_slots.clone())?,
        });
    }
    finalize_compiled_flipbook(
        settings,
        source_sha256,
        settings_sha256,
        source_bytes
            .len()
            .try_into()
            .map_err(|_| "source byte count does not fit u64")?,
        palette,
        material_map,
        animation_frames,
        cell_size_meters,
        inferred_frame_rate_from_durations(frames),
    )
}

/// Shared object assembly for both flipbook compilers: union bounds, default
/// frame from the first animation frame, one clip, and authored provenance.
/// The pinned `flipbook_experiment` content hashes prove this produces
/// byte-identical output for the rig-driven path.
#[allow(clippy::too_many_arguments)]
fn finalize_compiled_flipbook(
    settings: &FlipbookCompileSettings,
    source_sha256: String,
    settings_sha256: String,
    source_byte_count: u64,
    palette: Vec<VoxelAssetMaterialBinding>,
    material_map: Vec<VoxelAssetMaterialMapping>,
    animation_frames: Vec<VoxelObjectAnimationFrame>,
    cell_size_meters: f64,
    frames_per_second: f64,
) -> Result<CompiledFlipbook, String> {
    let bounds = animation_frames
        .iter()
        .map(|frame| frame.frame.bounds)
        .reduce(union_bounds)
        .ok_or("compiled flipbook has no bounds")?;
    let default_frame = animation_frames[0].frame.clone();
    let object = VoxelObjectAsset {
        schema_version: VOXEL_OBJECT_SCHEMA_VERSION,
        asset_id: settings.asset_id.clone(),
        grid: VoxelObjectGrid {
            coordinate_system: VoxelCoordinateSystem::RightHandedYUp,
            cell_size: cell_size_meters,
            chunk_size: settings.chunk_size,
            pivot: [0.0, 0.0, 0.0],
        },
        bounds,
        default_frame,
        clips: vec![VoxelObjectClip {
            id: settings.clip_id.clone(),
            name: Some(settings.clip_name.clone()),
            frames_per_second,
            frames: animation_frames,
        }],
        default_clip: Some(settings.clip_id.clone()),
        material_palette: palette,
        material_map,
        provenance: VoxelObjectProvenance {
            kind: VoxelObjectProvenanceKind::Authored,
            source_path: settings.source_path.clone(),
            source_sha256: source_sha256.clone(),
            source_byte_count,
            converter: COMPILER_ID.to_owned(),
            settings_sha256: settings_sha256.clone(),
            license_path: None,
            source_clips: Vec::new(),
        },
        content_hash: String::new(),
    };
    let asset = with_computed_voxel_object_hashes(object).map_err(|error| error.to_string())?;
    let canonical_json = encode_voxel_object(&asset).map_err(|error| error.to_string())?;
    Ok(CompiledFlipbook {
        asset,
        canonical_json,
        source_sha256,
        settings_sha256,
    })
}

/// Publish a compiled object under its content identity.
///
/// Existing byte-identical content is idempotent. A hash-path collision with
/// different bytes rejects without replacement.
pub fn publish_compiled_flipbook(
    root: &Path,
    directory: &str,
    compiled: &CompiledFlipbook,
) -> Result<FlipbookPublication, String> {
    let name = compiled
        .asset
        .asset_id
        .strip_prefix("voxel-object/")
        .ok_or("voxel object identity has no voxel-object/ prefix")?
        .replace('/', "-");
    let hash = compiled
        .asset
        .content_hash
        .strip_prefix("sha256:")
        .ok_or("voxel object content hash is malformed")?;
    let relative = format!("{directory}/{name}-{hash}.voxel-object.json");
    let path = safe_join(root, &relative)?;
    publish_immutable(&path, compiled.canonical_json.as_bytes())?;
    Ok(FlipbookPublication {
        asset_id: compiled.asset.asset_id.clone(),
        content_hash: compiled.asset.content_hash.clone(),
        path: relative,
        byte_count: compiled.canonical_json.len(),
    })
}

fn voxel_frame(
    frame: &FusedFrame,
    material_slots: impl IntoIterator<Item = u16>,
) -> Result<VoxelFrame, String> {
    let (min, max) = frame.bounds().ok_or("fused frame contains no voxels")?;
    let sparse_runs = frame
        .voxels
        .iter()
        .map(|cell| VoxelSparseRun {
            start: cell.coordinate,
            length: 1,
            material_slot: cell.material_slot,
        })
        .collect();
    with_computed_voxel_frame_hash(
        VoxelFrame {
            bounds: VoxelAssetBounds { min, max },
            representation: VoxelRepresentation {
                kind: VoxelRepresentationKind::SparseRuns,
                sparse_runs,
            },
            voxel_data_hash: String::new(),
        },
        material_slots,
    )
    .map_err(|error| error.to_string())
}

/// Build a voxel frame from a posed rough frame, run-length encoding
/// consecutive same-slot cells along +X (the Engine's sparse-run direction).
/// Posed frames carry no fusion provenance worth preserving in the artifact —
/// coordinate + material slot are the only geometry authority here.
fn voxel_frame_runs(
    frame: &RoughFrame,
    material_slots: impl IntoIterator<Item = u16>,
) -> Result<VoxelFrame, String> {
    let (min, max) = frame
        .bounds()
        .ok_or("posed flipbook frame contains no voxels")?;
    let mut cells: Vec<([i64; 3], u16)> = frame
        .voxels
        .iter()
        .map(|voxel| (voxel.coordinate, voxel.material_slot))
        .collect();
    cells.sort_by_key(|(coordinate, _)| (coordinate[1], coordinate[2], coordinate[0]));
    let mut sparse_runs: Vec<VoxelSparseRun> = Vec::new();
    for (coordinate, material_slot) in cells {
        match sparse_runs.last_mut() {
            Some(run)
                if run.material_slot == material_slot
                    && run.start[1] == coordinate[1]
                    && run.start[2] == coordinate[2]
                    && run.start[0] + i64::from(run.length) == coordinate[0] =>
            {
                run.length += 1;
            }
            _ => sparse_runs.push(VoxelSparseRun {
                start: coordinate,
                length: 1,
                material_slot,
            }),
        }
    }
    with_computed_voxel_frame_hash(
        VoxelFrame {
            bounds: VoxelAssetBounds { min, max },
            representation: VoxelRepresentation {
                kind: VoxelRepresentationKind::SparseRuns,
                sparse_runs,
            },
            voxel_data_hash: String::new(),
        },
        material_slots,
    )
    .map_err(|error| error.to_string())
}

fn material_palette(kit: &VoxelKit) -> Result<Vec<VoxelAssetMaterialBinding>, String> {
    let mut bindings = kit
        .palette
        .iter()
        .flat_map(|group| {
            group.slots.iter().map(|slot| VoxelAssetMaterialBinding {
                material_slot: slot.slot,
                material_asset_id: format!("material/{}-{}", kit.id, slot.slot),
                display_name: Some(slot.display_name.clone()),
            })
        })
        .collect::<Vec<_>>();
    bindings.sort_by_key(|binding| binding.material_slot);
    if bindings.is_empty() {
        return Err("kit palette has no material slots".into());
    }
    Ok(bindings)
}

fn material_map(kit: &VoxelKit) -> Vec<VoxelAssetMaterialMapping> {
    let mut mappings = kit
        .palette
        .iter()
        .flat_map(|group| {
            group.slots.iter().map(|slot| VoxelAssetMaterialMapping {
                source_material_slot: u32::from(slot.slot),
                source_material_name: Some(slot.display_name.clone()),
                voxel_material_slot: slot.slot,
            })
        })
        .collect::<Vec<_>>();
    mappings.sort_by_key(|mapping| mapping.source_material_slot);
    mappings
}

fn resolve_fact_source(
    kit: &VoxelKit,
    placements: &std::collections::BTreeMap<String, crate::pose::RigidTransform>,
    source: &FrameFactSource,
) -> Result<[f64; 3], String> {
    let (part_id, local) = match source {
        FrameFactSource::PartPivot { part_id } => {
            let part = kit
                .part(part_id)
                .ok_or_else(|| format!("unknown frame-fact part {part_id}"))?;
            (part_id, part.pivot.map(|value| value as f64))
        }
        FrameFactSource::PartSocket { part_id, socket_id } => {
            let part = kit
                .part(part_id)
                .ok_or_else(|| format!("unknown frame-fact part {part_id}"))?;
            let socket = part
                .socket(socket_id)
                .ok_or_else(|| format!("unknown frame-fact socket {part_id}.{socket_id}"))?;
            (part_id, socket.position)
        }
        FrameFactSource::PartVoxel {
            part_id,
            source_voxel_index,
        } => {
            let part = kit
                .part(part_id)
                .ok_or_else(|| format!("unknown frame-fact part {part_id}"))?;
            let cell = part
                .cells
                .get(*source_voxel_index as usize)
                .ok_or_else(|| {
                    format!("frame-fact voxel {part_id}[{source_voxel_index}] is out of range")
                })?;
            (part_id, cell.coordinate.map(|value| value as f64))
        }
    };
    placements
        .get(part_id)
        .map(|placement| placement.apply(local))
        .ok_or_else(|| format!("frame-fact part {part_id} has no pose placement"))
}

fn inferred_frame_rate(frames: &[FusedFrame]) -> f64 {
    let average = frames
        .iter()
        .map(|frame| frame.duration_microseconds as f64)
        .sum::<f64>()
        / frames.len() as f64;
    (1_000_000.0 / average).clamp(f64::EPSILON, 240.0)
}

fn inferred_frame_rate_from_durations(frames: &[RoughFrame]) -> f64 {
    let average = frames
        .iter()
        .map(|frame| frame.duration_microseconds as f64)
        .sum::<f64>()
        / frames.len() as f64;
    (1_000_000.0 / average).clamp(f64::EPSILON, 240.0)
}

fn union_bounds(left: VoxelAssetBounds, right: VoxelAssetBounds) -> VoxelAssetBounds {
    VoxelAssetBounds {
        min: std::array::from_fn(|axis| left.min[axis].min(right.min[axis])),
        max: std::array::from_fn(|axis| left.max[axis].max(right.max[axis])),
    }
}

fn publish_immutable(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
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
