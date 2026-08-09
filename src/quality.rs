use std::collections::{BTreeMap, BTreeSet};

use rusty_engine::{voxel_asset, voxel_convert};
use serde::Serialize;
use voxel_asset::VoxelFrameCell;
use voxel_convert::{
    sample_animation_clip_range, AnimationAnchorPolicy, AnimationEndPolicy,
    AnimationSampleRangeRequest, ImportedAnimatedMeshSource, ImportedStaticMesh,
    VoxelObjectConversionReceipt,
};

const SILHOUETTE_RESOLUTION: u32 = 32;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoxelQualityEvidence {
    pub silhouette_resolution: u32,
    pub cell_size: f64,
    pub pivot: [f64; 3],
    pub palette_slots: Vec<u16>,
    pub palette_stable: bool,
    pub clips: Vec<ClipQualityEvidence>,
    pub interpretation_limits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipQualityEvidence {
    pub clip_id: String,
    pub source_clip: String,
    pub endpoint_policy: String,
    pub duration_microseconds: u64,
    pub sampled_frames: usize,
    pub stored_frames: usize,
    pub source_foot_y_range: [f64; 2],
    pub voxel_foot_y_range: [f64; 2],
    pub minimum_source_pose_continuity: f64,
    pub minimum_voxel_silhouette_continuity: f64,
    pub loop_seam_source_continuity: f64,
    pub loop_seam_voxel_continuity: f64,
    pub palette_stable: bool,
    pub frames: Vec<FrameQualityEvidence>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameQualityEvidence {
    pub stored_frame_index: u32,
    pub source_timestamps_microseconds: Vec<u64>,
    pub representative_source_timestamp_microseconds: u64,
    pub voxel_data_hash: String,
    pub voxel_count: usize,
    pub material_slots: Vec<u16>,
    pub source: GeometryReadout,
    pub voxel: GeometryReadout,
    pub normalized_extent_error: f64,
    pub normalized_foot_anchor_error: f64,
    pub source_voxel_silhouette_jaccard: f64,
    pub source_pose_continuity_from_previous: Option<f64>,
    pub voxel_silhouette_continuity_from_previous: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GeometryReadout {
    pub bounds_min: [f64; 3],
    pub bounds_max: [f64; 3],
    pub centroid: [f64; 3],
    pub foot_center_xz: [f64; 2],
    pub foot_y: f64,
    pub sample_count: usize,
    pub silhouette_cells: usize,
}

pub fn analyze_prepared_quality(
    source: &ImportedAnimatedMeshSource,
    candidate: &VoxelObjectConversionReceipt,
    anchor_policy: &AnimationAnchorPolicy,
) -> Result<VoxelQualityEvidence, String> {
    let palette_slots = candidate
        .asset
        .material_palette
        .iter()
        .map(|binding| binding.material_slot)
        .collect::<Vec<_>>();
    let palette_set = palette_slots.iter().copied().collect::<BTreeSet<_>>();
    let mut clips = Vec::with_capacity(candidate.clips.len());
    for converted in &candidate.clips {
        let sampled = sample_animation_clip_range(
            &source.model,
            &AnimationSampleRangeRequest {
                expected_source_sha256: candidate.source_sha256.clone(),
                clip_name: converted.source_clip_name.clone(),
                sample_rate_hz: converted.sample_rate_hz,
                start_microseconds: converted.start_microseconds,
                end_microseconds: converted.end_microseconds,
                end_policy: converted.end_policy,
                anchor_policy: *anchor_policy,
            },
        )
        .map_err(|error| error.to_string())?;
        let snapshots = sampled
            .snapshots
            .iter()
            .map(|snapshot| (snapshot.timestamp_microseconds, &snapshot.mesh))
            .collect::<BTreeMap<_, _>>();
        let source_union = source_union(&sampled.snapshots)?;
        let mut resolved_frames = Vec::with_capacity(converted.frames.len());
        for frame_index in 0..converted.frames.len() {
            resolved_frames.push(
                candidate
                    .asset
                    .resolve_clip_frame(&converted.output_clip_id, frame_index)
                    .map_err(|error| error.to_string())?,
            );
        }
        let voxel_union = voxel_union(
            &resolved_frames,
            candidate.asset.grid.cell_size,
            candidate.asset.grid.pivot,
        )?;
        let mut frames = Vec::with_capacity(converted.frames.len());
        let mut previous_source = None;
        let mut previous_voxel_silhouette = None;
        let mut source_masks = Vec::with_capacity(converted.frames.len());
        let mut voxel_masks = Vec::with_capacity(converted.frames.len());
        let mut palette_stable = true;

        for (index, (readout, cells)) in converted.frames.iter().zip(&resolved_frames).enumerate() {
            let timestamp = *readout
                .source_timestamps_microseconds
                .first()
                .ok_or_else(|| {
                    format!(
                        "{} frame {index} has no source timestamp",
                        converted.output_clip_id
                    )
                })?;
            let mesh = snapshots.get(&timestamp).copied().ok_or_else(|| {
                format!(
                    "{} frame {index} references unsampled source timestamp {timestamp}",
                    converted.output_clip_id
                )
            })?;
            for timestamp in &readout.source_timestamps_microseconds {
                if !snapshots.contains_key(timestamp) {
                    return Err(format!(
                        "{} frame {index} references unsampled source timestamp {timestamp}",
                        converted.output_clip_id
                    ));
                }
            }
            let source_geometry = source_geometry(mesh, source_union)?;
            let voxel_geometry = voxel_geometry(
                cells,
                candidate.asset.grid.cell_size,
                candidate.asset.grid.pivot,
                voxel_union,
            )?;
            let source_mask = rasterize_source_silhouette(mesh, source_union);
            let voxel_mask = rasterize_voxel_silhouette(
                cells,
                candidate.asset.grid.cell_size,
                candidate.asset.grid.pivot,
                voxel_union,
            );
            let material_slots = cells
                .iter()
                .map(|cell| cell.material_slot)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            palette_stable &= material_slots.iter().all(|slot| palette_set.contains(slot));
            let source_pose_continuity_from_previous = previous_source
                .map(|previous| source_pose_continuity(previous, mesh, source_union));
            let voxel_silhouette_continuity_from_previous = previous_voxel_silhouette
                .as_ref()
                .map(|previous| jaccard(previous, &voxel_mask));
            frames.push(FrameQualityEvidence {
                stored_frame_index: readout.stored_frame_index,
                source_timestamps_microseconds: readout.source_timestamps_microseconds.clone(),
                representative_source_timestamp_microseconds: timestamp,
                voxel_data_hash: readout.voxel_data_hash.clone(),
                voxel_count: cells.len(),
                material_slots,
                normalized_extent_error: normalized_extent_error(
                    bounds_of_positions(&mesh.positions)?,
                    source_union,
                    bounds_of_voxels(
                        cells,
                        candidate.asset.grid.cell_size,
                        candidate.asset.grid.pivot,
                    )?,
                    voxel_union,
                ),
                normalized_foot_anchor_error: normalized_foot_error(
                    &source_geometry,
                    source_union,
                    &voxel_geometry,
                    voxel_union,
                ),
                source_voxel_silhouette_jaccard: jaccard(&source_mask, &voxel_mask),
                source_pose_continuity_from_previous,
                voxel_silhouette_continuity_from_previous,
                source: source_geometry,
                voxel: voxel_geometry,
            });
            source_masks.push(source_mask);
            voxel_masks.push(voxel_mask.clone());
            previous_source = Some(mesh);
            previous_voxel_silhouette = Some(voxel_mask);
        }
        let source_foot_y_range = value_range(frames.iter().map(|frame| frame.source.foot_y))?;
        let voxel_foot_y_range = value_range(frames.iter().map(|frame| frame.voxel.foot_y))?;
        let minimum_source_pose_continuity = minimum(
            frames
                .iter()
                .filter_map(|frame| frame.source_pose_continuity_from_previous),
        )
        .unwrap_or(1.0);
        let minimum_voxel_silhouette_continuity = minimum(
            frames
                .iter()
                .filter_map(|frame| frame.voxel_silhouette_continuity_from_previous),
        )
        .unwrap_or(1.0);
        let loop_seam_source_continuity =
            match (sampled.snapshots.first(), sampled.snapshots.last()) {
                (Some(first), Some(last)) => {
                    source_pose_continuity(&last.mesh, &first.mesh, source_union)
                }
                _ => 1.0,
            };
        let loop_seam_voxel_continuity = match (voxel_masks.first(), voxel_masks.last()) {
            (Some(first), Some(last)) => jaccard(last, first),
            _ => 1.0,
        };
        clips.push(ClipQualityEvidence {
            clip_id: converted.output_clip_id.clone(),
            source_clip: converted.source_clip_name.clone(),
            endpoint_policy: endpoint_policy(converted.end_policy).to_owned(),
            duration_microseconds: converted.duration_microseconds,
            sampled_frames: converted.sampled_frame_count,
            stored_frames: converted.stored_frame_count,
            source_foot_y_range: round_pair(source_foot_y_range),
            voxel_foot_y_range: round_pair(voxel_foot_y_range),
            minimum_source_pose_continuity: round(minimum_source_pose_continuity),
            minimum_voxel_silhouette_continuity: round(minimum_voxel_silhouette_continuity),
            loop_seam_source_continuity: round(loop_seam_source_continuity),
            loop_seam_voxel_continuity: round(loop_seam_voxel_continuity),
            palette_stable,
            frames,
        });
    }
    let palette_stable = clips.iter().all(|clip| clip.palette_stable);
    Ok(VoxelQualityEvidence {
        silhouette_resolution: SILHOUETTE_RESOLUTION,
        cell_size: candidate.asset.grid.cell_size,
        pivot: candidate.asset.grid.pivot,
        palette_slots,
        palette_stable,
        clips,
        interpretation_limits: vec![
            "Silhouette scores use a deterministic 32x32 front projection; they are structural evidence, not an art-quality score.".to_owned(),
            "Surface voxelization intentionally thickens sub-cell features to conservative occupied cells.".to_owned(),
            "Schema 1 stores complete poses and performs frame swaps; it does not interpolate voxel positions between poses.".to_owned(),
            "Material evidence measures stable palette-slot identity, not texture or color perceptual fidelity.".to_owned(),
        ],
    })
}

fn source_geometry(mesh: &ImportedStaticMesh, union: Bounds) -> Result<GeometryReadout, String> {
    let bounds = bounds_of_positions(&mesh.positions)?;
    let center = centroid(mesh.positions.iter().copied())?;
    let foot_y = bounds.min[1];
    let foot_band = (bounds.max[1] - bounds.min[1]).abs() * 0.02;
    let feet = mesh
        .positions
        .iter()
        .copied()
        .filter(|position| position[1] <= foot_y + foot_band.max(f64::EPSILON))
        .collect::<Vec<_>>();
    let foot = centroid(feet.iter().copied())?;
    let silhouette = rasterize_source_silhouette(mesh, union);
    Ok(GeometryReadout {
        bounds_min: round_vector(bounds.min),
        bounds_max: round_vector(bounds.max),
        centroid: round_vector(center),
        foot_center_xz: [round(foot[0]), round(foot[2])],
        foot_y: round(foot_y),
        sample_count: mesh.positions.len(),
        silhouette_cells: silhouette.len(),
    })
}

fn voxel_geometry(
    cells: &[VoxelFrameCell],
    cell_size: f64,
    pivot: [f64; 3],
    union: Bounds,
) -> Result<GeometryReadout, String> {
    let positions = cells
        .iter()
        .map(|cell| voxel_center(cell, cell_size, pivot))
        .collect::<Vec<_>>();
    let bounds = bounds_of_positions(&positions)?;
    let center = centroid(positions.iter().copied())?;
    let foot_y = bounds.min[1];
    let feet = positions
        .iter()
        .copied()
        .filter(|position| (position[1] - foot_y).abs() <= f64::EPSILON)
        .collect::<Vec<_>>();
    let foot = centroid(feet.iter().copied())?;
    let silhouette = rasterize_voxel_silhouette(cells, cell_size, pivot, union);
    Ok(GeometryReadout {
        bounds_min: round_vector(bounds.min),
        bounds_max: round_vector(bounds.max),
        centroid: round_vector(center),
        foot_center_xz: [round(foot[0]), round(foot[2])],
        foot_y: round(foot_y),
        sample_count: cells.len(),
        silhouette_cells: silhouette.len(),
    })
}

#[derive(Debug, Clone, Copy)]
struct Bounds {
    min: [f64; 3],
    max: [f64; 3],
}

impl Bounds {
    fn empty() -> Self {
        Self {
            min: [f64::INFINITY; 3],
            max: [f64::NEG_INFINITY; 3],
        }
    }

    fn include(&mut self, point: [f64; 3]) {
        for (axis, value) in point.into_iter().enumerate() {
            self.min[axis] = self.min[axis].min(value);
            self.max[axis] = self.max[axis].max(value);
        }
    }

    fn extent(self, axis: usize) -> f64 {
        (self.max[axis] - self.min[axis]).max(f64::EPSILON)
    }
}

fn source_union(snapshots: &[voxel_convert::AnimationMeshSnapshot]) -> Result<Bounds, String> {
    let mut bounds = Bounds::empty();
    let mut count = 0usize;
    for snapshot in snapshots {
        for position in &snapshot.mesh.positions {
            bounds.include(*position);
            count += 1;
        }
    }
    if count == 0 {
        Err("sampled source clip has no positions".to_owned())
    } else {
        Ok(bounds)
    }
}

fn voxel_union(
    frames: &[Vec<VoxelFrameCell>],
    cell_size: f64,
    pivot: [f64; 3],
) -> Result<Bounds, String> {
    let mut bounds = Bounds::empty();
    let mut count = 0usize;
    for frame in frames {
        for cell in frame {
            bounds.include(voxel_center(cell, cell_size, pivot));
            count += 1;
        }
    }
    if count == 0 {
        Err("converted clip has no voxels".to_owned())
    } else {
        Ok(bounds)
    }
}

fn bounds_of_positions(positions: &[[f64; 3]]) -> Result<Bounds, String> {
    let mut bounds = Bounds::empty();
    for position in positions {
        bounds.include(*position);
    }
    if positions.is_empty() {
        Err("geometry has no positions".to_owned())
    } else {
        Ok(bounds)
    }
}

fn bounds_of_voxels(
    cells: &[VoxelFrameCell],
    cell_size: f64,
    pivot: [f64; 3],
) -> Result<Bounds, String> {
    let positions = cells
        .iter()
        .map(|cell| voxel_center(cell, cell_size, pivot))
        .collect::<Vec<_>>();
    bounds_of_positions(&positions)
}

fn centroid(points: impl IntoIterator<Item = [f64; 3]>) -> Result<[f64; 3], String> {
    let mut sum = [0.0; 3];
    let mut count = 0usize;
    for point in points {
        for axis in 0..3 {
            sum[axis] += point[axis];
        }
        count += 1;
    }
    if count == 0 {
        return Err("geometry statistic has no samples".to_owned());
    }
    Ok(sum.map(|value| value / count as f64))
}

fn voxel_center(cell: &VoxelFrameCell, cell_size: f64, pivot: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|axis| (cell.coordinate[axis] as f64 + 0.5 - pivot[axis]) * cell_size)
}

fn rasterize_source_silhouette(mesh: &ImportedStaticMesh, union: Bounds) -> BTreeSet<(u32, u32)> {
    let mut cells = BTreeSet::new();
    for triangle in &mesh.triangles {
        let points = triangle.indices.map(|index| {
            let point = mesh.positions[index as usize];
            project(point, union)
        });
        rasterize_triangle(points, &mut cells);
    }
    cells
}

fn rasterize_voxel_silhouette(
    voxels: &[VoxelFrameCell],
    cell_size: f64,
    pivot: [f64; 3],
    union: Bounds,
) -> BTreeSet<(u32, u32)> {
    voxels
        .iter()
        .map(|cell| project_cell(voxel_center(cell, cell_size, pivot), union))
        .collect()
}

fn project(point: [f64; 3], bounds: Bounds) -> [f64; 2] {
    [
        (point[0] - bounds.min[0]) / bounds.extent(0) * f64::from(SILHOUETTE_RESOLUTION),
        (point[1] - bounds.min[1]) / bounds.extent(1) * f64::from(SILHOUETTE_RESOLUTION),
    ]
}

fn project_cell(point: [f64; 3], bounds: Bounds) -> (u32, u32) {
    let projected = project(point, bounds);
    (clamp_cell(projected[0]), clamp_cell(projected[1]))
}

fn clamp_cell(value: f64) -> u32 {
    value
        .floor()
        .clamp(0.0, f64::from(SILHOUETTE_RESOLUTION - 1)) as u32
}

fn rasterize_triangle(points: [[f64; 2]; 3], output: &mut BTreeSet<(u32, u32)>) {
    let min_x = clamp_cell(
        points
            .iter()
            .map(|point| point[0])
            .fold(f64::INFINITY, f64::min),
    );
    let max_x = clamp_cell(
        points
            .iter()
            .map(|point| point[0])
            .fold(f64::NEG_INFINITY, f64::max),
    );
    let min_y = clamp_cell(
        points
            .iter()
            .map(|point| point[1])
            .fold(f64::INFINITY, f64::min),
    );
    let max_y = clamp_cell(
        points
            .iter()
            .map(|point| point[1])
            .fold(f64::NEG_INFINITY, f64::max),
    );
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if point_in_triangle([f64::from(x) + 0.5, f64::from(y) + 0.5], points) {
                output.insert((x, y));
            }
        }
    }
    for point in points {
        output.insert((clamp_cell(point[0]), clamp_cell(point[1])));
    }
}

fn point_in_triangle(point: [f64; 2], triangle: [[f64; 2]; 3]) -> bool {
    let signs = [
        cross_2d(triangle[0], triangle[1], point),
        cross_2d(triangle[1], triangle[2], point),
        cross_2d(triangle[2], triangle[0], point),
    ];
    let has_negative = signs.iter().any(|value| *value < -1.0e-9);
    let has_positive = signs.iter().any(|value| *value > 1.0e-9);
    !(has_negative && has_positive)
}

fn cross_2d(a: [f64; 2], b: [f64; 2], point: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (point[1] - a[1]) - (b[1] - a[1]) * (point[0] - a[0])
}

fn source_pose_continuity(
    previous: &ImportedStaticMesh,
    current: &ImportedStaticMesh,
    union: Bounds,
) -> f64 {
    if previous.positions.len() != current.positions.len() || previous.positions.is_empty() {
        return 0.0;
    }
    let squared = previous
        .positions
        .iter()
        .zip(&current.positions)
        .map(|(left, right)| {
            (0..3)
                .map(|axis| (left[axis] - right[axis]).powi(2))
                .sum::<f64>()
        })
        .sum::<f64>()
        / previous.positions.len() as f64;
    let diagonal = (0..3)
        .map(|axis| union.extent(axis).powi(2))
        .sum::<f64>()
        .sqrt();
    (1.0 - squared.sqrt() / diagonal).clamp(0.0, 1.0)
}

fn normalized_extent_error(
    source: Bounds,
    source_union: Bounds,
    voxel: Bounds,
    voxel_union: Bounds,
) -> f64 {
    let squared = (0..3)
        .map(|axis| {
            let source_ratio = source.extent(axis) / source_union.extent(axis);
            let voxel_ratio = voxel.extent(axis) / voxel_union.extent(axis);
            (source_ratio - voxel_ratio).powi(2)
        })
        .sum::<f64>();
    round((squared / 3.0).sqrt())
}

fn normalized_foot_error(
    source: &GeometryReadout,
    source_union: Bounds,
    voxel: &GeometryReadout,
    voxel_union: Bounds,
) -> f64 {
    let source_point = [
        (source.foot_center_xz[0] - source_union.min[0]) / source_union.extent(0),
        (source.foot_y - source_union.min[1]) / source_union.extent(1),
        (source.foot_center_xz[1] - source_union.min[2]) / source_union.extent(2),
    ];
    let voxel_point = [
        (voxel.foot_center_xz[0] - voxel_union.min[0]) / voxel_union.extent(0),
        (voxel.foot_y - voxel_union.min[1]) / voxel_union.extent(1),
        (voxel.foot_center_xz[1] - voxel_union.min[2]) / voxel_union.extent(2),
    ];
    round(
        source_point
            .iter()
            .zip(voxel_point)
            .map(|(left, right)| (left - right).powi(2))
            .sum::<f64>()
            .sqrt(),
    )
}

fn jaccard(left: &BTreeSet<(u32, u32)>, right: &BTreeSet<(u32, u32)>) -> f64 {
    let union = left.union(right).count();
    if union == 0 {
        1.0
    } else {
        round(left.intersection(right).count() as f64 / union as f64)
    }
}

fn value_range(values: impl Iterator<Item = f64>) -> Result<[f64; 2], String> {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut count = 0usize;
    for value in values {
        min = min.min(value);
        max = max.max(value);
        count += 1;
    }
    if count == 0 {
        Err("quality clip has no frames".to_owned())
    } else {
        Ok([min, max])
    }
}

fn minimum(values: impl Iterator<Item = f64>) -> Option<f64> {
    values.reduce(f64::min)
}

fn endpoint_policy(policy: AnimationEndPolicy) -> &'static str {
    match policy {
        AnimationEndPolicy::IncludeClipEnd => "includeClipEnd",
        AnimationEndPolicy::ExcludeLoopSeam => "excludeLoopSeam",
    }
}

fn round(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn round_pair(value: [f64; 2]) -> [f64; 2] {
    value.map(round)
}

fn round_vector(value: [f64; 3]) -> [f64; 3] {
    value.map(round)
}
