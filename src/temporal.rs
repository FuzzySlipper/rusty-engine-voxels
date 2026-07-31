//! Temporal validation and human-review artifacts for finished fused clips.
//!
//! Metrics distinguish stable canonical identities from intentional spatial
//! motion. Review renders are observations; they never become frame authority.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use gif::{Encoder, Frame, Repeat};
use serde::{Deserialize, Serialize};

use crate::churn::occupied_coordinate_churn;
use crate::cleanup::EditBounds;
use crate::fusion::{FusedFrame, FusedVoxelOrigin};
use crate::kit::VoxelKit;

const FACE_NEIGHBORS: [[i64; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
];
const CHURN_BANDS: usize = 4;
const REVIEW_SIZE: u16 = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporalSettings {
    pub maximum_part_voxel_delta: usize,
    pub maximum_part_dimension_delta: i64,
    pub maximum_generated_voxels: usize,
    pub maximum_anchor_error_milli_cells: i64,
    pub required_anchors: BTreeSet<String>,
    pub protected_parts: BTreeSet<u32>,
}

impl Default for TemporalSettings {
    fn default() -> Self {
        Self {
            maximum_part_voxel_delta: 32,
            maximum_part_dimension_delta: 2,
            maximum_generated_voxels: 256,
            maximum_anchor_error_milli_cells: 250,
            required_anchors: BTreeSet::new(),
            protected_parts: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalClipEvidence {
    pub frame_count: usize,
    pub frames: Vec<TemporalFrameEvidence>,
    pub transitions: Vec<TemporalTransitionEvidence>,
    pub anchor_trajectories: Vec<AnchorTrajectory>,
    pub warnings: Vec<TemporalWarning>,
    pub average_spatial_churn_millionths: u32,
    pub average_canonical_identity_churn_millionths: u32,
    pub canonical_identity_churn_by_part_millionths: BTreeMap<String, u32>,
    pub churn_by_height_band: [usize; CHURN_BANDS],
    pub generated_voxel_minimum: usize,
    pub generated_voxel_maximum: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalFrameEvidence {
    pub frame_index: usize,
    pub time_microseconds: u64,
    pub regions: Vec<TemporalRegionMetrics>,
    pub anchor_distances_milli_cells: BTreeMap<String, i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalRegionMetrics {
    pub part_id: String,
    pub part_index: u32,
    pub voxel_count: usize,
    pub bounds: Option<EditBounds>,
    pub dimensions: [i64; 3],
    pub centroid_milli_cells: [i64; 3],
    pub principal_axes: [u8; 3],
    pub palette_histogram: BTreeMap<u16, usize>,
    pub exposed_surface_faces: usize,
    pub connected_components: usize,
    pub silhouette_area_front: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalTransitionEvidence {
    pub from_frame: usize,
    pub to_frame: usize,
    pub spatial_churn_millionths: u32,
    pub canonical_identity_churn_millionths: u32,
    pub canonical_identity_churn_by_part_millionths: BTreeMap<u32, u32>,
    pub added_coordinates: usize,
    pub removed_coordinates: usize,
    pub generated_voxel_count: usize,
    pub material_identity_changes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnchorTrajectory {
    pub anchor_id: String,
    pub samples: Vec<AnchorSample>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnchorSample {
    pub frame_index: usize,
    pub position_milli_cells: [i64; 3],
    pub proxy_error_milli_cells: i64,
    pub explicitly_corrected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalWarning {
    pub code: String,
    pub frame_index: usize,
    pub neighbor_frame_index: Option<usize>,
    pub region: Option<String>,
    pub view: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlickerReviewArtifacts {
    pub alternating_gif: Vec<u8>,
    pub onion_skin_svg: String,
    pub difference_heat_map_svg: String,
    pub silhouette_edge_motion_svg: String,
    pub palette_flicker_svg: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemporalError {
    code: &'static str,
    path: String,
    message: String,
}

impl TemporalError {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TemporalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

impl std::error::Error for TemporalError {}

/// Analyze a finished fused schedule without mutating its frames.
///
/// # Errors
///
/// Returns a typed error when the kit or frame/anchor cardinality is invalid,
/// a hard identity invariant fails, required anchor facts disappear, or a
/// bounded metric cannot be represented.
pub fn analyze_temporal_clip(
    kit: &VoxelKit,
    frames: &[FusedFrame],
    observed_anchors: &[BTreeMap<String, [f64; 3]>],
    proxy_anchors: &[BTreeMap<String, [f64; 3]>],
    corrected_anchors: &BTreeSet<(usize, String)>,
    settings: &TemporalSettings,
) -> Result<TemporalClipEvidence, TemporalError> {
    kit.validate()
        .map_err(|error| TemporalError::new("temporal.invalidKit", "kit", error.to_string()))?;
    if frames.len() < 2 {
        return Err(TemporalError::new(
            "temporal.insufficientFrames",
            "frames",
            "at least two frames are required",
        ));
    }
    if observed_anchors.len() != frames.len() || proxy_anchors.len() != frames.len() {
        return Err(TemporalError::new(
            "temporal.anchorFrameMismatch",
            "anchors",
            "observed/proxy anchor lists must match the frame count",
        ));
    }
    validate_hard_identity(kit, frames, settings)?;
    let mut evidence_frames = Vec::with_capacity(frames.len());
    let mut warnings = Vec::new();
    for (frame_index, frame) in frames.iter().enumerate() {
        let regions = kit
            .parts
            .iter()
            .enumerate()
            .map(|(part_index, part)| {
                region_metrics(
                    frame,
                    u32::try_from(part_index).expect("validated part count fits u32"),
                    &part.id,
                )
            })
            .collect();
        evidence_frames.push(TemporalFrameEvidence {
            frame_index,
            time_microseconds: frame.time_microseconds,
            regions,
            anchor_distances_milli_cells: anchor_distances(&observed_anchors[frame_index])?,
        });
    }
    let (y_min, y_max) = clip_y_bounds(frames)?;
    let mut transitions = Vec::new();
    let mut band_churn = [0usize; CHURN_BANDS];
    for frame_index in 1..frames.len() {
        let transition = transition_evidence(
            frame_index - 1,
            &frames[frame_index - 1],
            frame_index,
            &frames[frame_index],
            y_min,
            y_max,
            &mut band_churn,
        )?;
        compare_regions(
            &evidence_frames[frame_index - 1],
            &evidence_frames[frame_index],
            settings,
            &mut warnings,
        );
        if transition.generated_voxel_count > settings.maximum_generated_voxels {
            warnings.push(TemporalWarning {
                code: "temporal.synthetic_seam_large".to_owned(),
                frame_index,
                neighbor_frame_index: Some(frame_index - 1),
                region: Some("generated".to_owned()),
                view: Some("id_pass".to_owned()),
                message: format!(
                    "{} generated seam voxels exceed the {} warning threshold",
                    transition.generated_voxel_count, settings.maximum_generated_voxels
                ),
            });
        }
        if transition.material_identity_changes > 0 {
            warnings.push(TemporalWarning {
                code: "temporal.canonical_material_changed".to_owned(),
                frame_index,
                neighbor_frame_index: Some(frame_index - 1),
                region: None,
                view: Some("palette_flicker".to_owned()),
                message: format!(
                    "{} canonical identities changed material",
                    transition.material_identity_changes
                ),
            });
        }
        transitions.push(transition);
    }
    let anchor_trajectories = analyze_anchors(
        observed_anchors,
        proxy_anchors,
        corrected_anchors,
        settings,
        &mut warnings,
    )?;
    let average_spatial_churn_millionths = average(
        transitions
            .iter()
            .map(|transition| transition.spatial_churn_millionths),
    );
    let average_canonical_identity_churn_millionths = average(
        transitions
            .iter()
            .map(|transition| transition.canonical_identity_churn_millionths),
    );
    let canonical_identity_churn_by_part_millionths = kit
        .parts
        .iter()
        .enumerate()
        .map(|(part_index, part)| {
            let part_index = u32::try_from(part_index).expect("validated part count fits u32");
            (
                part.id.clone(),
                average(transitions.iter().map(|transition| {
                    transition
                        .canonical_identity_churn_by_part_millionths
                        .get(&part_index)
                        .copied()
                        .unwrap_or(0)
                })),
            )
        })
        .collect();
    Ok(TemporalClipEvidence {
        frame_count: frames.len(),
        frames: evidence_frames,
        transitions,
        anchor_trajectories,
        warnings,
        average_spatial_churn_millionths,
        average_canonical_identity_churn_millionths,
        canonical_identity_churn_by_part_millionths,
        churn_by_height_band: band_churn,
        generated_voxel_minimum: frames
            .iter()
            .map(|frame| frame.generated_voxels)
            .min()
            .unwrap_or(0),
        generated_voxel_maximum: frames
            .iter()
            .map(|frame| frame.generated_voxels)
            .max()
            .unwrap_or(0),
    })
}

/// Build deterministic human-review projections for a finished fused schedule.
///
/// # Errors
///
/// Returns a typed error when fewer than two frames are supplied, the clip is
/// empty, or GIF encoding fails.
pub fn generate_flicker_review(
    frames: &[FusedFrame],
) -> Result<FlickerReviewArtifacts, TemporalError> {
    if frames.len() < 2 {
        return Err(TemporalError::new(
            "temporal.insufficientFrames",
            "frames",
            "at least two frames are required",
        ));
    }
    let bounds = clip_bounds(frames)?;
    let mut gif = Vec::new();
    {
        let palette = &[16, 18, 22, 235, 238, 242];
        let mut encoder = Encoder::new(&mut gif, REVIEW_SIZE, REVIEW_SIZE, palette)
            .map_err(|error| TemporalError::new("temporal.gifEncode", "gif", error.to_string()))?;
        encoder
            .set_repeat(Repeat::Infinite)
            .map_err(|error| TemporalError::new("temporal.gifEncode", "gif", error.to_string()))?;
        for frame in frames {
            let pixels = occupancy_pixels(frame, bounds);
            let mut gif_frame =
                Frame::from_palette_pixels(REVIEW_SIZE, REVIEW_SIZE, pixels, palette, None);
            gif_frame.delay =
                u16::try_from((frame.duration_microseconds / 10_000).clamp(1, 500)).unwrap_or(500);
            encoder.write_frame(&gif_frame).map_err(|error| {
                TemporalError::new("temporal.gifEncode", "gif", error.to_string())
            })?;
        }
    }
    let middle = frames.len() / 2;
    let previous = &frames[middle.saturating_sub(1)];
    let current = &frames[middle];
    let next = &frames[(middle + 1).min(frames.len() - 1)];
    Ok(FlickerReviewArtifacts {
        alternating_gif: gif,
        onion_skin_svg: comparison_svg("Three-frame onion skin", previous, current, next, bounds),
        difference_heat_map_svg: difference_svg(
            "Per-pixel temporal difference heat map",
            previous,
            current,
            bounds,
            false,
        ),
        silhouette_edge_motion_svg: difference_svg(
            "Silhouette-edge motion",
            previous,
            current,
            bounds,
            true,
        ),
        palette_flicker_svg: palette_flicker_svg(previous, current, bounds),
    })
}

fn validate_hard_identity(
    kit: &VoxelKit,
    frames: &[FusedFrame],
    settings: &TemporalSettings,
) -> Result<(), TemporalError> {
    for part_id in &settings.protected_parts {
        if usize::try_from(*part_id)
            .ok()
            .and_then(|index| kit.parts.get(index))
            .is_none()
        {
            return Err(TemporalError::new(
                "temporal.invalidProtectedPart",
                "settings.protectedParts",
                format!("protected part index {part_id} is not present in the kit"),
            ));
        }
    }
    for (frame_index, frame) in frames.iter().enumerate() {
        let mut coordinates = BTreeSet::new();
        for cell in &frame.voxels {
            if !coordinates.insert(cell.coordinate) {
                return Err(TemporalError::new(
                    "temporal.duplicateCanonicalIdentity",
                    format!("frames[{frame_index}].voxels"),
                    format!("duplicate occupied coordinate {:?}", cell.coordinate),
                ));
            }
        }
        let identities = canonical_map(frame);
        for &(part_id, source_voxel_index) in identities.keys() {
            let Some(part) = usize::try_from(part_id)
                .ok()
                .and_then(|index| kit.parts.get(index))
            else {
                return Err(TemporalError::new(
                    "temporal.invalidCanonicalIdentity",
                    format!("frames[{frame_index}]"),
                    format!("canonical part index {part_id} is not present in the kit"),
                ));
            };
            if usize::try_from(source_voxel_index)
                .ok()
                .is_none_or(|index| index >= part.cells.len())
            {
                return Err(TemporalError::new(
                    "temporal.invalidCanonicalIdentity",
                    format!("frames[{frame_index}]"),
                    format!(
                        "canonical source index {source_voxel_index} is not present in part {}",
                        part.id
                    ),
                ));
            }
        }
        for &part_id in &settings.protected_parts {
            let part = &kit.parts[usize::try_from(part_id).expect("protected part validated")];
            let expected = (0..part.cells.len())
                .map(|source_voxel_index| {
                    (
                        part_id,
                        u32::try_from(source_voxel_index)
                            .expect("validated kit cell count fits canonical identity"),
                    )
                })
                .collect::<BTreeSet<_>>();
            let actual = identities
                .keys()
                .filter(|(identity_part_id, _)| *identity_part_id == part_id)
                .copied()
                .collect::<BTreeSet<_>>();
            if expected != actual {
                let missing = expected.difference(&actual).next().copied();
                let extra = actual.difference(&expected).next().copied();
                return Err(TemporalError::new(
                    "temporal.protectedIdentityChanged",
                    format!("frames[{frame_index}]"),
                    format!(
                        "protected part {} identity inventory differs: missing {missing:?}, extra {extra:?}",
                        part.id
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn region_metrics(frame: &FusedFrame, part_index: u32, part_id: &str) -> TemporalRegionMetrics {
    let cells = frame
        .voxels
        .iter()
        .filter(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical { part_id, .. } if part_id == part_index
            )
        })
        .collect::<Vec<_>>();
    let coordinates = cells
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let bounds = coordinate_bounds(coordinates.iter().copied());
    let dimensions = bounds.map_or([0; 3], |bounds| {
        std::array::from_fn(|axis| bounds.max[axis] - bounds.min[axis] + 1)
    });
    let centroid_milli_cells = if cells.is_empty() {
        [0; 3]
    } else {
        std::array::from_fn(|axis| {
            cells
                .iter()
                .map(|cell| i128::from(cell.coordinate[axis]) * 1_000)
                .sum::<i128>()
                .checked_div(i128::try_from(cells.len()).unwrap_or(i128::MAX))
                .and_then(|value| i64::try_from(value).ok())
                .unwrap_or(0)
        })
    };
    let mut variances = [0i128; 3];
    for cell in &cells {
        for axis in 0..3 {
            let delta =
                i128::from(cell.coordinate[axis]) * 1_000 - i128::from(centroid_milli_cells[axis]);
            variances[axis] += delta * delta;
        }
    }
    let mut axes = [0u8, 1, 2];
    axes.sort_by_key(|axis| std::cmp::Reverse(variances[usize::from(*axis)]));
    let mut palette_histogram = BTreeMap::new();
    for cell in &cells {
        *palette_histogram.entry(cell.material_slot).or_insert(0) += 1;
    }
    let exposed_surface_faces = coordinates
        .iter()
        .map(|coordinate| {
            neighbors(*coordinate)
                .into_iter()
                .filter(|neighbor| !coordinates.contains(neighbor))
                .count()
        })
        .sum();
    TemporalRegionMetrics {
        part_id: part_id.to_owned(),
        part_index,
        voxel_count: cells.len(),
        bounds,
        dimensions,
        centroid_milli_cells,
        principal_axes: axes,
        palette_histogram,
        exposed_surface_faces,
        connected_components: components(&coordinates).len(),
        silhouette_area_front: coordinates
            .iter()
            .map(|coordinate| [coordinate[0], coordinate[1]])
            .collect::<BTreeSet<_>>()
            .len(),
    }
}

fn transition_evidence(
    from_index: usize,
    from: &FusedFrame,
    to_index: usize,
    to: &FusedFrame,
    y_min: i64,
    y_max: i64,
    band_churn: &mut [usize; CHURN_BANDS],
) -> Result<TemporalTransitionEvidence, TemporalError> {
    let from_coordinates = from
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let to_coordinates = to
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    for coordinate in from_coordinates.symmetric_difference(&to_coordinates) {
        band_churn[height_band(coordinate[1], y_min, y_max)] += 1;
    }
    let coordinate_churn = occupied_coordinate_churn(&from_coordinates, &to_coordinates);
    let added_coordinates = coordinate_churn.added;
    let removed_coordinates = coordinate_churn.removed;
    let union = coordinate_churn.union;
    let from_identities = canonical_map(from);
    let to_identities = canonical_map(to);
    let from_keys = from_identities.keys().copied().collect::<BTreeSet<_>>();
    let to_keys = to_identities.keys().copied().collect::<BTreeSet<_>>();
    let identity_union = from_keys.union(&to_keys).count();
    let identity_changes = from_keys.symmetric_difference(&to_keys).count();
    let material_identity_changes = from_identities
        .iter()
        .filter(|(identity, observations)| {
            to_identities
                .get(identity)
                .is_some_and(|next_observations| *observations != next_observations)
        })
        .count();
    let part_ids = from_keys
        .iter()
        .chain(&to_keys)
        .map(|(part_id, _)| *part_id)
        .collect::<BTreeSet<_>>();
    let canonical_identity_churn_by_part_millionths = part_ids
        .into_iter()
        .map(|part_id| {
            let from_part = from_keys
                .iter()
                .filter(|(candidate, _)| *candidate == part_id)
                .copied()
                .collect::<BTreeSet<_>>();
            let to_part = to_keys
                .iter()
                .filter(|(candidate, _)| *candidate == part_id)
                .copied()
                .collect::<BTreeSet<_>>();
            (
                part_id,
                ratio_millionths(
                    from_part.symmetric_difference(&to_part).count(),
                    from_part.union(&to_part).count(),
                ),
            )
        })
        .collect();
    Ok(TemporalTransitionEvidence {
        from_frame: from_index,
        to_frame: to_index,
        spatial_churn_millionths: ratio_millionths(added_coordinates + removed_coordinates, union),
        canonical_identity_churn_millionths: ratio_millionths(identity_changes, identity_union),
        canonical_identity_churn_by_part_millionths,
        added_coordinates,
        removed_coordinates,
        generated_voxel_count: to.generated_voxels,
        material_identity_changes,
    })
}

fn compare_regions(
    previous: &TemporalFrameEvidence,
    current: &TemporalFrameEvidence,
    settings: &TemporalSettings,
    warnings: &mut Vec<TemporalWarning>,
) {
    for (before, after) in previous.regions.iter().zip(&current.regions) {
        let count_delta = before.voxel_count.abs_diff(after.voxel_count);
        if count_delta > settings.maximum_part_voxel_delta {
            warnings.push(TemporalWarning {
                code: "temporal.region_volume_drift".to_owned(),
                frame_index: current.frame_index,
                neighbor_frame_index: Some(previous.frame_index),
                region: Some(after.part_id.clone()),
                view: Some("id_pass".to_owned()),
                message: format!(
                    "voxel count changed by {count_delta}; threshold is {}",
                    settings.maximum_part_voxel_delta
                ),
            });
        }
        for axis in 0..3 {
            let delta = (before.dimensions[axis] - after.dimensions[axis]).abs();
            if delta > settings.maximum_part_dimension_delta {
                warnings.push(TemporalWarning {
                    code: "temporal.region_dimension_drift".to_owned(),
                    frame_index: current.frame_index,
                    neighbor_frame_index: Some(previous.frame_index),
                    region: Some(after.part_id.clone()),
                    view: Some(
                        match axis {
                            0 => "front",
                            1 => "side",
                            _ => "top",
                        }
                        .to_owned(),
                    ),
                    message: format!(
                        "axis {axis} dimension changed by {delta}; threshold is {}",
                        settings.maximum_part_dimension_delta
                    ),
                });
            }
        }
        if before.connected_components != after.connected_components {
            warnings.push(TemporalWarning {
                code: "temporal.component_count_changed".to_owned(),
                frame_index: current.frame_index,
                neighbor_frame_index: Some(previous.frame_index),
                region: Some(after.part_id.clone()),
                view: Some("id_pass".to_owned()),
                message: format!(
                    "component count changed {} -> {}",
                    before.connected_components, after.connected_components
                ),
            });
        }
    }
}

fn analyze_anchors(
    observed: &[BTreeMap<String, [f64; 3]>],
    proxy: &[BTreeMap<String, [f64; 3]>],
    corrected: &BTreeSet<(usize, String)>,
    settings: &TemporalSettings,
    warnings: &mut Vec<TemporalWarning>,
) -> Result<Vec<AnchorTrajectory>, TemporalError> {
    let ids = observed
        .iter()
        .flat_map(|anchors| anchors.keys().cloned())
        .collect::<BTreeSet<_>>();
    for required in &settings.required_anchors {
        if !ids.contains(required) {
            return Err(TemporalError::new(
                "temporal.requiredAnchorMissing",
                "anchors",
                format!("required anchor {required} is absent"),
            ));
        }
    }
    let mut trajectories = Vec::new();
    for id in ids {
        let mut samples = Vec::with_capacity(observed.len());
        for frame_index in 0..observed.len() {
            let position = observed[frame_index].get(&id).ok_or_else(|| {
                TemporalError::new(
                    "temporal.anchorBlink",
                    format!("anchors[{frame_index}]"),
                    format!("anchor {id} disappeared"),
                )
            })?;
            let expected = proxy[frame_index].get(&id).ok_or_else(|| {
                TemporalError::new(
                    "temporal.proxyAnchorMissing",
                    format!("proxyAnchors[{frame_index}]"),
                    format!("proxy anchor {id} is absent"),
                )
            })?;
            let position_milli_cells = milli_position(*position)?;
            let expected_milli_cells = milli_position(*expected)?;
            let error = (0..3)
                .map(|axis| {
                    i128::from(position_milli_cells[axis] - expected_milli_cells[axis]).pow(2)
                })
                .sum::<i128>();
            let error = integer_sqrt(error).min(i128::from(i64::MAX)) as i64;
            let explicitly_corrected = corrected.contains(&(frame_index, id.clone()));
            if error > settings.maximum_anchor_error_milli_cells && !explicitly_corrected {
                warnings.push(TemporalWarning {
                    code: "temporal.anchor_proxy_drift".to_owned(),
                    frame_index,
                    neighbor_frame_index: None,
                    region: Some(id.clone()),
                    view: Some("anchor_overlay".to_owned()),
                    message: format!(
                        "anchor differs from proxy by {error} milli-cells; threshold is {}",
                        settings.maximum_anchor_error_milli_cells
                    ),
                });
            }
            samples.push(AnchorSample {
                frame_index,
                position_milli_cells,
                proxy_error_milli_cells: error,
                explicitly_corrected,
            });
        }
        trajectories.push(AnchorTrajectory {
            anchor_id: id,
            samples,
        });
    }
    Ok(trajectories)
}

fn canonical_map(frame: &FusedFrame) -> BTreeMap<(u32, u32), BTreeSet<u16>> {
    let mut result = BTreeMap::<(u32, u32), BTreeSet<u16>>::new();
    for cell in &frame.voxels {
        if let FusedVoxelOrigin::Canonical {
            part_id,
            source_voxel_index,
        } = cell.origin
        {
            result
                .entry((part_id, source_voxel_index))
                .or_default()
                .insert(cell.material_slot);
        }
    }
    // A canonical source hidden by deterministic inter-part overlap remains
    // part of the frame's identity inventory. M3 validates this ledger against
    // authoritative rerasterization, so M6 can distinguish visibility from an
    // authored identity actually appearing or disappearing.
    for discarded in &frame.discarded_origins {
        result
            .entry((discarded.part_id, discarded.source_voxel_index))
            .or_default()
            .insert(discarded.material_slot);
    }
    result
}

fn milli_position(position: [f64; 3]) -> Result<[i64; 3], TemporalError> {
    let mut result = [0; 3];
    for axis in 0..3 {
        if !position[axis].is_finite() || position[axis].abs() > i64::MAX as f64 / 1_000.0 {
            return Err(TemporalError::new(
                "temporal.invalidAnchor",
                "anchors",
                "anchor coordinates must be finite and bounded",
            ));
        }
        result[axis] = (position[axis] * 1_000.0).round() as i64;
    }
    Ok(result)
}

fn anchor_distances(
    anchors: &BTreeMap<String, [f64; 3]>,
) -> Result<BTreeMap<String, i64>, TemporalError> {
    let anchors = anchors
        .iter()
        .map(|(id, position)| Ok((id, milli_position(*position)?)))
        .collect::<Result<Vec<_>, TemporalError>>()?;
    let mut distances = BTreeMap::new();
    for left in 0..anchors.len() {
        for right in left + 1..anchors.len() {
            let squared = (0..3)
                .map(|axis| i128::from(anchors[left].1[axis] - anchors[right].1[axis]).pow(2))
                .sum();
            distances.insert(
                format!("{}..{}", anchors[left].0, anchors[right].0),
                integer_sqrt(squared).min(i128::from(i64::MAX)) as i64,
            );
        }
    }
    Ok(distances)
}

fn ratio_millionths(numerator: usize, denominator: usize) -> u32 {
    if denominator == 0 {
        return 0;
    }
    let value = (numerator as u128)
        .saturating_mul(1_000_000)
        .checked_div(denominator as u128)
        .unwrap_or(0);
    u32::try_from(value.min(u128::from(u32::MAX))).unwrap_or(u32::MAX)
}

fn average(values: impl IntoIterator<Item = u32>) -> u32 {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty() {
        0
    } else {
        u32::try_from(
            values.iter().map(|value| u64::from(*value)).sum::<u64>() / values.len() as u64,
        )
        .unwrap_or(u32::MAX)
    }
}

fn clip_y_bounds(frames: &[FusedFrame]) -> Result<(i64, i64), TemporalError> {
    let bounds = clip_bounds(frames)?;
    Ok((bounds.min[1], bounds.max[1]))
}

fn clip_bounds(frames: &[FusedFrame]) -> Result<EditBounds, TemporalError> {
    coordinate_bounds(
        frames
            .iter()
            .flat_map(|frame| frame.voxels.iter().map(|cell| cell.coordinate)),
    )
    .ok_or_else(|| TemporalError::new("temporal.emptyClip", "frames", "clip has no voxels"))
}

fn coordinate_bounds(coordinates: impl IntoIterator<Item = [i64; 3]>) -> Option<EditBounds> {
    let mut coordinates = coordinates.into_iter();
    let first = coordinates.next()?;
    let (min, max) = coordinates.fold((first, first), |(mut min, mut max), coordinate| {
        for axis in 0..3 {
            min[axis] = min[axis].min(coordinate[axis]);
            max[axis] = max[axis].max(coordinate[axis]);
        }
        (min, max)
    });
    Some(EditBounds { min, max })
}

fn height_band(y: i64, y_min: i64, y_max: i64) -> usize {
    let span = usize::try_from((y_max - y_min + 1).max(1)).unwrap_or(1);
    let offset = usize::try_from((y - y_min).max(0)).unwrap_or(0);
    (offset * CHURN_BANDS / span).min(CHURN_BANDS - 1)
}

fn neighbors(coordinate: [i64; 3]) -> [[i64; 3]; 6] {
    FACE_NEIGHBORS
        .map(|offset| std::array::from_fn(|axis| coordinate[axis].saturating_add(offset[axis])))
}

fn components(coordinates: &BTreeSet<[i64; 3]>) -> Vec<BTreeSet<[i64; 3]>> {
    let mut remaining = coordinates.clone();
    let mut result = Vec::new();
    while let Some(start) = remaining.pop_first() {
        let mut component = BTreeSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some(coordinate) = queue.pop_front() {
            for neighbor in neighbors(coordinate) {
                if remaining.remove(&neighbor) {
                    component.insert(neighbor);
                    queue.push_back(neighbor);
                }
            }
        }
        result.push(component);
    }
    result
}

fn integer_sqrt(value: i128) -> i128 {
    if value <= 0 {
        return 0;
    }
    let mut low = 1i128;
    let mut high = value.min(i128::from(i64::MAX));
    while low <= high {
        let middle = low + (high - low) / 2;
        match middle.checked_mul(middle) {
            Some(square) if square == value => return middle,
            Some(square) if square < value => low = middle + 1,
            _ => high = middle - 1,
        }
    }
    high
}

fn occupancy_pixels(frame: &FusedFrame, bounds: EditBounds) -> Vec<u8> {
    let mut pixels = vec![0u8; usize::from(REVIEW_SIZE) * usize::from(REVIEW_SIZE)];
    for cell in &frame.voxels {
        let [x, y] = review_coordinate(cell.coordinate, bounds);
        pixels[usize::from(y) * usize::from(REVIEW_SIZE) + usize::from(x)] = 1;
    }
    pixels
}

fn review_coordinate(coordinate: [i64; 3], bounds: EditBounds) -> [u16; 2] {
    let width = (bounds.max[0] - bounds.min[0]).max(1);
    let height = (bounds.max[1] - bounds.min[1]).max(1);
    let x = (coordinate[0] - bounds.min[0]) * i64::from(REVIEW_SIZE - 1) / width;
    let y = (bounds.max[1] - coordinate[1]) * i64::from(REVIEW_SIZE - 1) / height;
    [
        u16::try_from(x.clamp(0, i64::from(REVIEW_SIZE - 1))).unwrap_or(0),
        u16::try_from(y.clamp(0, i64::from(REVIEW_SIZE - 1))).unwrap_or(0),
    ]
}

fn comparison_svg(
    title: &str,
    previous: &FusedFrame,
    current: &FusedFrame,
    next: &FusedFrame,
    bounds: EditBounds,
) -> String {
    let layers = [
        (previous, "#ff3b30", "0.35"),
        (current, "#f5f7fa", "0.85"),
        (next, "#34c759", "0.35"),
    ];
    let mut body = String::new();
    for (frame, color, opacity) in layers {
        for cell in &frame.voxels {
            let [x, y] = review_coordinate(cell.coordinate, bounds);
            body.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" width=\"2\" height=\"2\" fill=\"{color}\" opacity=\"{opacity}\"/>"
            ));
        }
    }
    svg(title, &body)
}

fn difference_svg(
    title: &str,
    previous: &FusedFrame,
    current: &FusedFrame,
    bounds: EditBounds,
    edges_only: bool,
) -> String {
    let previous = previous
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let current = current
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let mut body = String::new();
    for coordinate in previous.symmetric_difference(&current) {
        if edges_only
            && neighbors(*coordinate)
                .into_iter()
                .all(|neighbor| previous.contains(&neighbor) || current.contains(&neighbor))
        {
            continue;
        }
        let [x, y] = review_coordinate(*coordinate, bounds);
        let color = if current.contains(coordinate) {
            "#34c759"
        } else {
            "#ff3b30"
        };
        body.push_str(&format!(
            "<rect x=\"{x}\" y=\"{y}\" width=\"2\" height=\"2\" fill=\"{color}\"/>"
        ));
    }
    svg(title, &body)
}

fn palette_flicker_svg(previous: &FusedFrame, current: &FusedFrame, bounds: EditBounds) -> String {
    let previous = previous
        .voxels
        .iter()
        .map(|cell| (cell.coordinate, cell.material_slot))
        .collect::<BTreeMap<_, _>>();
    let mut body = String::new();
    for cell in &current.voxels {
        if previous
            .get(&cell.coordinate)
            .is_some_and(|material| *material != cell.material_slot)
        {
            let [x, y] = review_coordinate(cell.coordinate, bounds);
            body.push_str(&format!(
                "<rect x=\"{x}\" y=\"{y}\" width=\"3\" height=\"3\" fill=\"#ffcc00\"/>"
            ));
        }
    }
    svg("Palette flicker", &body)
}

fn svg(title: &str, body: &str) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"128\" height=\"148\" viewBox=\"0 0 128 148\"><rect width=\"128\" height=\"148\" fill=\"#101216\"/><text x=\"4\" y=\"14\" fill=\"#f5f7fa\" font-size=\"7\">{title}</text><g transform=\"translate(0 20)\">{body}</g></svg>\n"
    )
}
