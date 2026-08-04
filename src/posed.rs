//! Authored pose-spec documents: rig-free flipbook frames from a canonical kit.
//!
//! A pose-spec is a small JSON document (schema version 1) that authors one
//! flipbook clip out of per-part euler deltas (degrees, about each part's own
//! pivot) plus single-level attachment chains (a child part inherits its
//! parent's rotation about the parent's pivot, so hands follow arms and
//! weapons follow the arm-hand chain). This is the authored-data form of the
//! manual pivoting experiment (`docs/kit-pivoting.md`): poses are data, not
//! test-code constants.
//!
//! Frames assemble through the rig-free `assemble_placed_frame` and compile
//! into a canonical Engine voxel object through
//! `flipbook::compile_posed_flipbook`. No rig, skinning, or Engine semantics
//! are reproduced here.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::assemble::{assemble_placed_frame, RoughFrame};
use crate::flipbook::{
    compile_posed_flipbook, publish_compiled_flipbook, FlipbookCompileSettings, FlipbookPublication,
};
use crate::kit::{load_kit, neutral_part_transforms, VoxelKit};
use crate::pose::{euler_degrees_to_quaternion, RasterSettings, RigidTransform};
use crate::project::{atomic_write, read_bounded, read_bounded_text, safe_join};

pub const POSE_SPEC_SCHEMA_VERSION: u32 = 1;
const MAX_POSE_SPEC_BYTES: u64 = 256 * 1024;
/// Engine object codec admits at most 4,096 frames per clip.
const MAX_POSE_SPEC_FRAMES: usize = 4_096;
/// Per-frame duration ceiling (one minute), far above any flipbook rate.
const MAX_FRAME_DURATION_MICROSECONDS: u64 = 60_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoseSpecDocument {
    pub schema_version: u32,
    /// Short identity for this pose set (e.g. `knight-walk`); also used to
    /// derive the compiled voxel-object asset id.
    pub id: String,
    /// Project-relative path of the canonical kit JSON.
    pub kit: String,
    /// Optional power-of-two cell downsample applied to every assembled frame
    /// before compilation (the object's cell size grows by the same factor).
    /// Kit-scale characters (100k+ cells) exceed the Engine's 64 MiB artifact
    /// bound at full resolution; 2 keeps a ~300-cell character at retro
    /// high-fidelity scale.
    #[serde(default = "default_cell_downsample")]
    pub cell_downsample: u32,
    pub clip_id: String,
    pub clip_name: String,
    pub frames: Vec<PoseSpecFrame>,
}

const fn default_cell_downsample() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoseSpecFrame {
    pub name: String,
    pub duration_microseconds: u64,
    #[serde(default)]
    pub deltas: Vec<PartDelta>,
    #[serde(default)]
    pub chains: Vec<PartChain>,
}

/// One part's pivot rotation: euler deltas in degrees about the part's own
/// pivot, applied X first, then Y, then Z.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartDelta {
    pub part: String,
    #[serde(default)]
    pub x_deg: f64,
    #[serde(default)]
    pub y_deg: f64,
    #[serde(default)]
    pub z_deg: f64,
}

/// `child` inherits `parent`'s own delta as a rotation about the parent's
/// neutral pivot point. Single-level: a chain parent must not itself be a
/// chain child (composition order would become ambiguous).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartChain {
    pub child: String,
    pub parent: String,
}

/// Load and parse a pose-spec document from a project-relative path.
pub fn load_pose_spec(root: &Path, relative_path: &str) -> Result<PoseSpecDocument, String> {
    let path = safe_join(root, relative_path)?;
    let text = read_bounded_text(&path, MAX_POSE_SPEC_BYTES, "pose spec")?;
    serde_json::from_str(&text).map_err(|error| format!("{relative_path}: {error}"))
}

impl PoseSpecDocument {
    /// Validate the document against the kit it poses: schema version,
    /// identity shape, frame shape and durations, and every part reference —
    /// with errors naming the offending frame and part.
    pub fn validate(&self, kit: &VoxelKit) -> Result<(), String> {
        if self.schema_version != POSE_SPEC_SCHEMA_VERSION {
            return Err(format!(
                "pose spec schema {} is unsupported; expected {POSE_SPEC_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        require_identity(&self.id, "id")?;
        require_identity(&self.kit, "kit")?;
        if !matches!(self.cell_downsample, 1 | 2 | 4 | 8) {
            return Err(format!(
                "cellDownsample must be one of 1, 2, 4, 8, got {}",
                self.cell_downsample
            ));
        }
        if !self.clip_id.starts_with("clip/") {
            return Err(format!(
                "clip id {} must be a clip/... identity",
                self.clip_id
            ));
        }
        require_identity(&self.clip_name, "clipName")?;
        if self.frames.is_empty() || self.frames.len() > MAX_POSE_SPEC_FRAMES {
            return Err(format!(
                "pose spec must contain 1..={MAX_POSE_SPEC_FRAMES} frames"
            ));
        }
        let mut frame_names = std::collections::BTreeSet::new();
        for frame in &self.frames {
            require_identity(&frame.name, "frames.name")?;
            if !frame_names.insert(frame.name.as_str()) {
                return Err(format!("frame {} is named more than once", frame.name));
            }
            if frame.duration_microseconds == 0
                || frame.duration_microseconds > MAX_FRAME_DURATION_MICROSECONDS
            {
                return Err(format!(
                    "frame {} duration must be within 1..={MAX_FRAME_DURATION_MICROSECONDS} microseconds",
                    frame.name
                ));
            }
            let mut delta_parts = std::collections::BTreeSet::new();
            for delta in &frame.deltas {
                if kit.part(&delta.part).is_none() {
                    return Err(format!(
                        "frame {} delta names unknown part {}",
                        frame.name, delta.part
                    ));
                }
                if !delta_parts.insert(delta.part.as_str()) {
                    return Err(format!(
                        "frame {} deltas part {} more than once",
                        frame.name, delta.part
                    ));
                }
                if ![delta.x_deg, delta.y_deg, delta.z_deg]
                    .iter()
                    .all(|value| value.is_finite())
                {
                    return Err(format!(
                        "frame {} delta for part {} must be finite degrees",
                        frame.name, delta.part
                    ));
                }
            }
            let mut chain_children = std::collections::BTreeSet::new();
            for chain in &frame.chains {
                if kit.part(&chain.child).is_none() {
                    return Err(format!(
                        "frame {} chain names unknown child part {}",
                        frame.name, chain.child
                    ));
                }
                if kit.part(&chain.parent).is_none() {
                    return Err(format!(
                        "frame {} chain names unknown parent part {}",
                        frame.name, chain.parent
                    ));
                }
                if chain.child == chain.parent {
                    return Err(format!(
                        "frame {} chains part {} to itself",
                        frame.name, chain.child
                    ));
                }
                if !chain_children.insert(chain.child.as_str()) {
                    return Err(format!(
                        "frame {} chains part {} more than once",
                        frame.name, chain.child
                    ));
                }
                if frame.chains.iter().any(|other| other.child == chain.parent) {
                    return Err(format!(
                        "frame {} chain parent {} is itself chained; chains are single-level",
                        frame.name, chain.parent
                    ));
                }
            }
        }
        Ok(())
    }
}

fn require_identity(value: &str, path: &str) -> Result<(), String> {
    if value.trim().is_empty() || value != value.trim() {
        Err(format!("{path} must be non-empty canonical text"))
    } else {
        Ok(())
    }
}

/// Compute one rigid placement per part for one pose frame, from the kit's
/// neutral transforms. Parts rotate about their own pivot (the pivot stays at
/// the neutral translation); chained children additionally inherit their
/// parent's own delta as a rotation about the parent's neutral pivot point.
///
/// This is the same placement rule the manual pivoting experiment verified
/// (`tests/kit_pivot_experiment.rs`), now driven by authored data.
pub fn pose_spec_placements(
    kit: &VoxelKit,
    frame: &PoseSpecFrame,
) -> Result<BTreeMap<String, RigidTransform>, String> {
    let neutral = neutral_part_transforms(kit).map_err(|error| error.to_string())?;
    let deltas: BTreeMap<&str, &PartDelta> = frame
        .deltas
        .iter()
        .map(|delta| (delta.part.as_str(), delta))
        .collect();
    let chains: BTreeMap<&str, &str> = frame
        .chains
        .iter()
        .map(|chain| (chain.child.as_str(), chain.parent.as_str()))
        .collect();
    let mut placements = BTreeMap::new();
    for part in &kit.parts {
        let (_, base_translation) = neutral
            .get(&part.id)
            .ok_or_else(|| format!("part {} has no neutral transform", part.id))?;
        let own_rotation = deltas
            .get(part.id.as_str())
            .map_or([0.0, 0.0, 0.0, 1.0], |delta| {
                euler_degrees_to_quaternion(delta.x_deg, delta.y_deg, delta.z_deg)
            });
        let mut placement = RigidTransform {
            rotation: own_rotation,
            translation: [
                base_translation[0] as f64,
                base_translation[1] as f64,
                base_translation[2] as f64,
            ],
        };
        if let Some(parent_id) = chains.get(part.id.as_str()) {
            let parent_rotation = deltas.get(parent_id).map_or([0.0, 0.0, 0.0, 1.0], |delta| {
                euler_degrees_to_quaternion(delta.x_deg, delta.y_deg, delta.z_deg)
            });
            let (_, parent_translation) = neutral
                .get(*parent_id)
                .ok_or_else(|| format!("chain parent {parent_id} has no neutral transform"))?;
            let pivot = [
                parent_translation[0] as f64,
                parent_translation[1] as f64,
                parent_translation[2] as f64,
            ];
            let rotated_pivot = crate::pose::RigidTransform {
                rotation: parent_rotation,
                translation: [0.0, 0.0, 0.0],
            }
            .apply(pivot);
            // about_parent = { rotation: parent_q, translation: pivot - parent_q*pivot }
            let about_parent = RigidTransform {
                rotation: parent_rotation,
                translation: [
                    pivot[0] - rotated_pivot[0],
                    pivot[1] - rotated_pivot[1],
                    pivot[2] - rotated_pivot[2],
                ],
            };
            placement = about_parent.then(placement);
        }
        placements.insert(part.id.clone(), placement);
    }
    Ok(placements)
}

/// Assemble every frame of a validated pose spec into rough frames with
/// cumulative explicit times (`frames[i].time = sum of prior durations`).
pub fn assemble_pose_spec(
    kit: &VoxelKit,
    spec: &PoseSpecDocument,
    settings: &RasterSettings,
) -> Result<Vec<RoughFrame>, String> {
    spec.validate(kit)?;
    let mut frames = Vec::with_capacity(spec.frames.len());
    let mut time_microseconds = 0_u64;
    for frame_spec in &spec.frames {
        let placements = pose_spec_placements(kit, frame_spec)?;
        let frame = assemble_placed_frame(
            kit,
            &placements,
            time_microseconds,
            frame_spec.duration_microseconds,
            settings,
        )
        .map_err(|error| format!("frame {} assembles: {error}", frame_spec.name))?;
        time_microseconds = time_microseconds
            .checked_add(frame_spec.duration_microseconds)
            .ok_or("pose spec cumulative time overflowed")?;
        frames.push(frame);
    }
    Ok(frames)
}

/// ASCII orthographic review render of a rough frame (same convention as the
/// pivot experiment's evidence renders): project along the axis pair with
/// `outer_axis` horizontal and +Y vertical, one character per material slot.
pub fn render_frame_ascii(
    frame: &RoughFrame,
    outer_axis: usize,
    width: usize,
    height: usize,
) -> Vec<String> {
    let Some((min, max)) = frame.bounds() else {
        return Vec::new();
    };
    let span_w = (max[outer_axis] - min[outer_axis] + 1) as f64;
    let span_h = (max[1] - min[1] + 1) as f64;
    let scale = (width as f64 / span_w).min(height as f64 / span_h);
    let out_w = (span_w * scale).ceil() as usize;
    let out_h = (span_h * scale).ceil() as usize;
    let mut grid = vec![vec![' '; out_w]; out_h];
    for voxel in &frame.voxels {
        let gx = ((voxel.coordinate[outer_axis] - min[outer_axis]) as f64 * scale) as usize;
        let gy = ((max[1] - voxel.coordinate[1]) as f64 * scale) as usize;
        if gx < out_w && gy < out_h {
            grid[gy][gx] = match voxel.material_slot {
                1 => '#',
                2 => 'H',
                3 => 'S',
                4 => 'h',
                5 => 'W',
                6 => 'C',
                _ => '+',
            };
        }
    }
    grid.into_iter()
        .map(|row| row.into_iter().collect())
        .collect()
}

/// Downsample a rough frame by a power-of-two cell factor: cells bin into the
/// global target lattice (`div_euclid`, so the ground plane stays consistent
/// across frames) and each target cell takes the majority material slot of
/// its source cells (ties break to the lowest slot). Deterministic.
/// Per-part provenance and fusion flags do not survive downsampling and are
/// dropped.
#[must_use]
pub fn downsample_rough_frame(frame: &RoughFrame, factor: u32) -> RoughFrame {
    if factor <= 1 {
        return frame.clone();
    }
    let factor = i64::from(factor);
    let mut votes: BTreeMap<[i64; 3], BTreeMap<u16, u32>> = BTreeMap::new();
    for voxel in &frame.voxels {
        let target = [
            voxel.coordinate[0].div_euclid(factor),
            voxel.coordinate[1].div_euclid(factor),
            voxel.coordinate[2].div_euclid(factor),
        ];
        *votes
            .entry(target)
            .or_default()
            .entry(voxel.material_slot)
            .or_insert(0) += 1;
    }
    let voxels = votes
        .into_iter()
        .map(|(coordinate, slots)| {
            let (material_slot, _) = slots
                .iter()
                .max_by(|(slot_a, count_a), (slot_b, count_b)| {
                    count_a.cmp(count_b).then_with(|| slot_b.cmp(slot_a))
                })
                .expect("non-empty slot votes");
            crate::assemble::AssembledVoxelCell {
                coordinate,
                material_slot: *material_slot,
                part_id: 0,
                source_voxel_index: 0,
                needs_fusion: false,
            }
        })
        .collect();
    RoughFrame {
        time_microseconds: frame.time_microseconds,
        duration_microseconds: frame.duration_microseconds,
        voxels,
        discarded_overlaps: Vec::new(),
    }
}

/// The result of a posed-flipbook lab run: review evidence plus the
/// content-addressed publication of the compiled voxel object.
pub struct PosedFlipbookRun {
    pub evidence: serde_json::Value,
    pub publication: FlipbookPublication,
    pub compiled: crate::flipbook::CompiledFlipbook,
}

/// Assemble a pose spec, compile it into a canonical voxel object, and publish
/// the immutable artifact under `object_directory` — the `voxel-kit-lab poses`
/// workflow. Assembly determinism is a hard gate: the first frame is
/// re-assembled and must be voxel-identical.
pub fn run_posed_flipbook(
    root: &Path,
    spec_path: &str,
    object_directory: &str,
) -> Result<PosedFlipbookRun, String> {
    let spec = load_pose_spec(root, spec_path)?;
    let kit = load_kit(root, &spec.kit).map_err(|error| error.to_string())?;
    spec.validate(&kit)?;
    let settings = RasterSettings::default();
    let frames = assemble_pose_spec(&kit, &spec, &settings)?;

    let first_placements = pose_spec_placements(&kit, &spec.frames[0])?;
    let first_again = assemble_placed_frame(
        &kit,
        &first_placements,
        frames[0].time_microseconds,
        frames[0].duration_microseconds,
        &settings,
    )
    .map_err(|error| format!("frame {} re-assembles: {error}", spec.frames[0].name))?;
    if first_again.voxels != frames[0].voxels {
        return Err(format!(
            "frame {} assembly is not deterministic",
            spec.frames[0].name
        ));
    }

    let spec_bytes = read_bounded(
        &safe_join(root, spec_path)?,
        MAX_POSE_SPEC_BYTES,
        "pose spec",
    )?;
    let frames = frames
        .iter()
        .map(|frame| downsample_rough_frame(frame, spec.cell_downsample))
        .collect::<Vec<_>>();
    let cell_size_meters = kit.convention.voxel_size_meters * f64::from(spec.cell_downsample);
    let compile_settings = FlipbookCompileSettings {
        asset_id: format!("voxel-object/posed-{}", spec.id),
        clip_id: spec.clip_id.clone(),
        clip_name: spec.clip_name.clone(),
        source_path: spec_path.to_owned(),
        chunk_size: 16,
        anchors: Vec::new(),
        body_collision: None,
        hit_regions: Vec::new(),
    };
    let compiled = compile_posed_flipbook(
        &kit,
        &frames,
        &compile_settings,
        &spec_bytes,
        cell_size_meters,
    )?;
    let publication = publish_compiled_flipbook(root, object_directory, &compiled)?;

    let clip = &compiled.asset.clips[0];
    let frame_evidence = frames
        .iter()
        .zip(&spec.frames)
        .zip(&clip.frames)
        .map(|((frame, frame_spec), animation_frame)| {
            json!({
                "name": frame_spec.name,
                "timeMicroseconds": frame.time_microseconds,
                "durationMicroseconds": frame.duration_microseconds,
                "voxels": frame.len(),
                "sparseRuns": animation_frame.frame.representation.sparse_runs.len(),
                "front": render_frame_ascii(frame, 0, 72, 34),
                "side": render_frame_ascii(frame, 2, 40, 34),
            })
        })
        .collect::<Vec<_>>();
    let evidence = json!({
        "spec": spec_path,
        "kit": spec.kit,
        "clipId": spec.clip_id,
        "cellDownsample": spec.cell_downsample,
        "cellSizeMeters": cell_size_meters,
        "framesPerSecond": clip.frames_per_second,
        "determinism": {
            "frame": spec.frames[0].name,
            "identical": true,
        },
        "frames": frame_evidence,
        "publication": {
            "assetId": publication.asset_id,
            "contentHash": publication.content_hash,
            "path": publication.path,
            "byteCount": publication.byte_count,
        },
        "engineCaps": {
            "artifactBytes": publication.byte_count,
            "maxArtifactBytes": voxel_asset::MAX_VOXEL_OBJECT_ARTIFACT_BYTES,
            "maxVoxelsPerFrame": voxel_asset::MAX_REPRESENTED_VOXELS,
            "maxTotalVoxels": voxel_asset::MAX_VOXEL_OBJECT_TOTAL_VOXELS,
            "peakFrameVoxels": frames.iter().map(RoughFrame::len).max().unwrap_or(0),
            "totalVoxels": frames.iter().map(RoughFrame::len).sum::<usize>(),
        },
    });
    Ok(PosedFlipbookRun {
        evidence,
        publication,
        compiled,
    })
}

/// Write the run's evidence report to a project-relative path.
pub fn write_posed_flipbook_report(
    root: &Path,
    relative_path: &str,
    run: &PosedFlipbookRun,
) -> Result<(), String> {
    let pretty = serde_json::to_string_pretty(&run.evidence).map_err(|error| error.to_string())?;
    atomic_write(&safe_join(root, relative_path)?, pretty.as_bytes())
}
