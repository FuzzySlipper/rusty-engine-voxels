//! Joint-localized temporal churn measurement for voxel flipbook clips.
//!
//! The straight mesh→flipbook pipeline resamples the continuous skinned surface
//! independently every pose, which turns sub-cell bone motion into widespread
//! binary cell flips. That aliasing concentrates where surfaces move fastest —
//! limbs and joints — while rigid or slow regions (head, feet) stay stable.
//!
//! This module measures that directly from the admitted runtime object: for
//! each clip it walks consecutive stored frames, computes the symmetric
//! difference of occupied cell coordinates, and buckets the churned cells by
//! height band. The result is the baseline that a canonical-parts ("exploded
//! kit") pipeline must beat, because rigid parts should confine churn to joint
//! seams instead of spreading it across the whole body.
//!
//! All data is derived from the checked canonical object via the strict runtime
//! admission path; nothing here mutates project or object state.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::conversion::ENGINE_REVISION;
use crate::runtime::load_runtime_project;

/// Number of equal-height bands the object's Y extent is split into. Regions
/// are labelled `region/0` (lowest, feet) through `region/N` (highest, head).
const CHURN_REGIONS: usize = 4;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameTransitionChurn {
    pub from_frame: u32,
    pub to_frame: u32,
    pub from_voxels: usize,
    pub to_voxels: usize,
    pub shared_voxels: usize,
    pub added_voxels: usize,
    pub removed_voxels: usize,
    /// Fraction of the union that changed (added + removed) / |A ∪ B|.
    pub churn_fraction: f64,
    /// Churned cells per height region, index 0 = lowest (feet).
    pub churn_by_region: Vec<usize>,
    /// Share of total churn in each region; parallel to `churn_by_region`.
    pub churn_fraction_by_region: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipChurnEvidence {
    pub clip_id: String,
    pub transitions: Vec<FrameTransitionChurn>,
    pub average_churn_fraction: f64,
    /// Which height region (0 = feet) carries the most churn on average.
    pub dominant_region: usize,
    /// Average share of churn in the dominant region across transitions.
    pub dominant_region_fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChurnStudyEvidence {
    pub engine_revision: String,
    pub project_file: String,
    pub asset_id: String,
    pub content_hash: String,
    pub region_count: usize,
    pub grid_y_min: i64,
    pub grid_y_max: i64,
    pub clips: Vec<ClipChurnEvidence>,
    pub interpretation_limits: Vec<String>,
}

pub fn run_churn_study(root: &Path, relative_project: &str) -> Result<ChurnStudyEvidence, String> {
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

    let (y_min, y_max) = object
        .frames()
        .iter()
        .flat_map(|frame| frame.cells.iter().map(|cell| cell.coordinate[1]))
        .fold((i64::MAX, i64::MIN), |(lo, hi), y| (lo.min(y), hi.max(y)));
    if y_min > y_max {
        return Err("admitted object has no occupied cells".to_owned());
    }

    let mut clips = Vec::new();
    for clip in object.clips() {
        let mut transitions = Vec::new();
        for pair in clip.frame_indices.windows(2) {
            let from = object
                .frame(pair[0])
                .ok_or_else(|| format!("clip {} references missing frame {}", clip.id, pair[0]))?;
            let to = object
                .frame(pair[1])
                .ok_or_else(|| format!("clip {} references missing frame {}", clip.id, pair[1]))?;
            transitions.push(measure_transition(
                from.index,
                &from.cells,
                to.index,
                &to.cells,
                y_min,
                y_max,
            ));
        }
        if transitions.is_empty() {
            continue;
        }
        let average_churn_fraction = transitions
            .iter()
            .map(|transition| transition.churn_fraction)
            .sum::<f64>()
            / transitions.len() as f64;
        let (dominant_region, dominant_region_fraction) = dominant_region(&transitions);
        clips.push(ClipChurnEvidence {
            clip_id: clip.id.clone(),
            transitions,
            average_churn_fraction,
            dominant_region,
            dominant_region_fraction,
        });
    }

    Ok(ChurnStudyEvidence {
        engine_revision: ENGINE_REVISION.to_owned(),
        project_file: relative_project.to_owned(),
        asset_id: object.asset_id().to_owned(),
        content_hash: object.content_hash().to_owned(),
        region_count: CHURN_REGIONS,
        grid_y_min: y_min,
        grid_y_max: y_max,
        clips,
        interpretation_limits: vec![
            "regions are equal-height bands over the object's occupied Y extent; region 0 is \
             the lowest band (feet), the highest is the head. They are geometric bands, not \
             semantic body parts — a band is a proxy for 'limb/joint vs core/head' churn."
                .to_owned(),
            "churn is the symmetric difference of occupied cell coordinates between \
             consecutive stored frames; it measures resampling noise plus true motion and \
             cannot by itself separate the two."
                .to_owned(),
            "material slots are ignored; a cell is compared by coordinate occupancy only."
                .to_owned(),
            "this is the straight mesh→flipbook baseline. A canonical-parts (exploded-kit) \
             pipeline should confine churn to joint-seam regions and drive core/head churn \
             toward zero; this measurement is the gate it must beat."
                .to_owned(),
        ],
    })
}

fn measure_transition(
    from_index: u32,
    from_cells: &[voxel_asset::VoxelFrameCell],
    to_index: u32,
    to_cells: &[voxel_asset::VoxelFrameCell],
    y_min: i64,
    y_max: i64,
) -> FrameTransitionChurn {
    let from: BTreeSet<[i64; 3]> = from_cells.iter().map(|cell| cell.coordinate).collect();
    let to: BTreeSet<[i64; 3]> = to_cells.iter().map(|cell| cell.coordinate).collect();

    let shared = from.intersection(&to).count();
    let added = to.difference(&from).count();
    let removed = from.difference(&to).count();
    let union = from.union(&to).count();
    let churned = added + removed;
    let churn_fraction = if union == 0 {
        0.0
    } else {
        churned as f64 / union as f64
    };

    let mut churn_by_region = vec![0usize; CHURN_REGIONS];
    for cell in from.symmetric_difference(&to) {
        churn_by_region[region_of(cell[1], y_min, y_max)] += 1;
    }
    let churn_fraction_by_region = churn_by_region
        .iter()
        .map(|count| {
            if churned == 0 {
                0.0
            } else {
                *count as f64 / churned as f64
            }
        })
        .collect();

    FrameTransitionChurn {
        from_frame: from_index,
        to_frame: to_index,
        from_voxels: from.len(),
        to_voxels: to.len(),
        shared_voxels: shared,
        added_voxels: added,
        removed_voxels: removed,
        churn_fraction,
        churn_by_region,
        churn_fraction_by_region,
    }
}

fn region_of(y: i64, y_min: i64, y_max: i64) -> usize {
    let span = (y_max - y_min + 1).max(1) as usize;
    let offset = (y - y_min).max(0) as usize;
    (offset * CHURN_REGIONS / span).min(CHURN_REGIONS - 1)
}

fn dominant_region(transitions: &[FrameTransitionChurn]) -> (usize, f64) {
    let mut totals = [0f64; CHURN_REGIONS];
    for transition in transitions {
        for (index, fraction) in transition.churn_fraction_by_region.iter().enumerate() {
            totals[index] += fraction;
        }
    }
    let count = transitions.len().max(1) as f64;
    let mut best = 0usize;
    for (index, total) in totals.iter().enumerate() {
        if *total > totals[best] {
            best = index;
        }
    }
    (best, totals[best] / count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxel_asset::VoxelFrameCell;

    fn cell(x: i64, y: i64, z: i64) -> VoxelFrameCell {
        VoxelFrameCell {
            coordinate: [x, y, z],
            material_slot: 1,
        }
    }

    #[test]
    fn bands_split_the_y_extent_evenly() {
        // y in 0..=99 across 4 bands of 25 each.
        assert_eq!(region_of(0, 0, 99), 0);
        assert_eq!(region_of(24, 0, 99), 0);
        assert_eq!(region_of(25, 0, 99), 1);
        assert_eq!(region_of(99, 0, 99), 3);
        // Degenerate single-cell span stays in region 0.
        assert_eq!(region_of(5, 5, 5), 0);
    }

    #[test]
    fn churn_tracks_region_and_overlap() {
        // Base occupies a vertical column y 0..=7; next keeps the low half,
        // moves the top half one cell over (all churn in the upper bands).
        let from: Vec<_> = (0..=7).map(|y| cell(0, y, 0)).collect();
        let to: Vec<_> = (0..=3)
            .map(|y| cell(0, y, 0))
            .chain((4..=7).map(|y| cell(1, y, 0)))
            .collect();
        let transition = measure_transition(0, &from, 1, &to, 0, 7);
        assert_eq!(transition.shared_voxels, 4);
        assert_eq!(transition.added_voxels, 4);
        assert_eq!(transition.removed_voxels, 4);
        // 8 changed of a 12-cell union (8 base + 4 added).
        assert!((transition.churn_fraction - 8.0 / 12.0).abs() < 1e-9);
        // All churn is in y 4..=7 -> bands 2 and 3, none in bands 0 and 1.
        assert_eq!(transition.churn_by_region[0], 0);
        assert_eq!(transition.churn_by_region[1], 0);
        assert_eq!(
            transition.churn_by_region[2] + transition.churn_by_region[3],
            8
        );
    }

    #[test]
    fn identical_frames_have_zero_churn() {
        let frame: Vec<_> = (0..=3).map(|y| cell(0, y, 0)).collect();
        let transition = measure_transition(0, &frame, 1, &frame, 0, 3);
        assert_eq!(transition.churn_fraction, 0.0);
        assert!(transition.churn_by_region.iter().all(|count| *count == 0));
    }
}
