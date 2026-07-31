//! Pose selection and rough frame assembly for the exploded-kit pipeline (M2).
//!
//! The straight mesh→flipbook path samples every source frame at a fixed rate
//! and re-voxelizes each independently. The baked pipeline instead *selects* a
//! small set of meaningful poses and *assembles* each into a rough voxel frame
//! from rigid canonical parts. This module owns that selection and assembly:
//!
//! - **Pose selection** keeps hard anchors (first, last, and caller-authored
//!   mandatory timestamps) and stages the complete error-bounded schedule —
//!   the minimal subdivisions that bring every retained interval within the
//!   pose-space error budget — failing with a typed impossibility when that
//!   schedule cannot fit under the hard frame cap. Motion-event frames fill
//!   leftover slots best-effort. The result is a stepped schedule with
//!   independent per-frame durations.
//!
//! - **Rough assembly** rasterizes every bound part for a selected pose and
//!   merges the result into one frame, preserving canonical part/voxel
//!   provenance and marking not-yet-fused joint regions for M3.
//!
//! Both are deterministic: same model + same clip + same settings → identical
//! schedule and identical frames.

use std::collections::BTreeMap;

use serde::Serialize;
use voxel_convert::ImportedAnimatedModel;

use crate::kit::VoxelKit;
use crate::pose::{
    evaluate_node_poses, rasterize_part, PoseError, RasterSettings, RigMap, RigidTransform,
};

// ---------------------------------------------------------------------------
// Pose selection
// ---------------------------------------------------------------------------

/// Settings for selecting a stepped pose schedule from a source clip.
#[derive(Debug, Clone, PartialEq)]
pub struct PoseSelectionSettings {
    /// Root/limb translation (in cells) that counts as a mandatory event frame.
    pub event_translation_threshold: f64,
    /// Bone rotation (radians) that counts as a mandatory event frame.
    pub event_rotation_threshold: f64,
    /// Pose-space error budget: the maximum allowed deviation (in cells) when
    /// reducing between mandatory frames.
    pub error_budget: f64,
    /// Hard cap on selected frames (defensive bound). Must be >= 2 so first and
    /// last can both be retained; the cap is never exceeded.
    pub max_frames: usize,
    /// Caller-authored mandatory timestamps (microseconds) that must be
    /// retained regardless of motion delta (contacts, key poses, impacts).
    pub mandatory_timestamps: Vec<u64>,
}

impl PoseSelectionSettings {
    fn validate(&self) -> Result<(), PoseError> {
        for (name, value) in [
            (
                "eventTranslationThreshold",
                self.event_translation_threshold,
            ),
            ("eventRotationThreshold", self.event_rotation_threshold),
            ("errorBudget", self.error_budget),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(PoseError::Validation(format!(
                    "{name} must be finite and non-negative, got {value}"
                )));
            }
        }
        if self.max_frames < 2 {
            return Err(PoseError::Validation(format!(
                "max_frames must be >= 2 so first and last poses fit, got {}",
                self.max_frames
            )));
        }
        for &t in &self.mandatory_timestamps {
            // Checked in select against the clip; only shape here.
            let _ = t;
        }
        Ok(())
    }
}

impl Default for PoseSelectionSettings {
    fn default() -> Self {
        PoseSelectionSettings {
            event_translation_threshold: 2.0,
            event_rotation_threshold: 0.35,
            error_budget: 1.5,
            max_frames: 64,
            mandatory_timestamps: Vec::new(),
        }
    }
}

/// One selected pose with its independent hold duration.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedPose {
    /// Source timestamp in microseconds.
    pub time_microseconds: u64,
    /// How long this pose is held before the next selected pose, in
    /// microseconds. The last pose holds for the remaining clip duration.
    pub duration_microseconds: u64,
    /// Why this frame was selected.
    pub reason: SelectionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionReason {
    First,
    Last,
    Event,
    /// A caller-authored mandatory timestamp (contact, key pose, impact).
    Mandatory,
    ErrorBudget,
}

/// Select a stepped pose schedule for `clip_index` from `model`.
///
/// Walks candidate frames at the clip's native sample ticks and stages the
/// complete error-bounded schedule up front: the first and last poses plus
/// every caller-authored mandatory timestamp are hard anchors, and the minimal
/// greedy subdivisions that bring every retained interval within
/// `error_budget` are required frames. Both the hard cap and the error budget
/// are guarantees, never best-effort (R6336-11): if the staged schedule does
/// not fit under `max_frames`, selection fails with a typed impossibility
/// error naming the required frame count — likewise when mandatory anchors
/// alone overflow the cap, or when the budget is below the clip's maximum
/// indivisible tick-step error. Event frames (motion deltas beyond the event
/// thresholds) fill only leftover slots, and only where the split keeps both
/// halves within budget. Independent durations are the gaps between
/// consecutive selected times (the last holds to the clip end).
pub fn select_pose_schedule(
    model: &ImportedAnimatedModel,
    clip_index: usize,
    settings: &PoseSelectionSettings,
) -> Result<Vec<SelectedPose>, PoseError> {
    settings.validate()?;
    let clip = model
        .clips
        .get(clip_index)
        .ok_or(PoseError::UnknownClip(clip_index))?;
    let duration = clip.duration_microseconds;
    if duration == 0 {
        return Ok(Vec::new());
    }
    // Build the complete bounded native timeline strictly within [0, duration),
    // independent of the output cap, so tight caps never truncate away the true
    // tail (R6336-9). The clip end is the boundary the final selected pose holds
    // to, never itself a selected pose. The timeline is bounded only by the
    // native tick, never by max_frames.
    let tick = 16_667u64.max(duration / 256);
    let mut candidates: Vec<u64> = Vec::new();
    let mut t = 0u64;
    while t < duration {
        candidates.push(t);
        t += tick;
    }
    debug_assert!(!candidates.is_empty());

    // Caller-authored mandatory timestamps are added to the candidate set
    // (clamped into the clip) so they are always evaluated and retained.
    let mut mandatory_times: Vec<u64> = settings
        .mandatory_timestamps
        .iter()
        .map(|&t| t.min(duration.saturating_sub(1)))
        .collect();
    mandatory_times.sort_unstable();
    mandatory_times.dedup();
    for &time in &mandatory_times {
        if !candidates.contains(&time) {
            candidates.push(time);
        }
    }
    candidates.sort_unstable();

    // Mandatory-capacity preflight (R6336-9): first + last + every mandatory
    // anchor must fit under the hard cap. If they cannot, no schedule can honor
    // both the cap and the mandatory anchors, so reject with a typed
    // impossibility error rather than silently dropping a mandatory timestamp.
    // (First and last share no timestamp with a distinct mandatory time here.)
    let distinct_mandatory = mandatory_times
        .iter()
        .filter(|&&t| t != candidates[0] && t != *candidates.last().expect("non-empty"))
        .count();
    let required = 2 + distinct_mandatory; // first + last + distinct mandatories
    if required > settings.max_frames {
        return Err(PoseError::Validation(format!(
            "max_frames {} cannot hold first + last + {distinct_mandatory} mandatory timestamps (need {required}); either raise max_frames or drop mandatory anchors",
            settings.max_frames
        )));
    }

    // Evaluate poses for candidates.
    let poses: Vec<BTreeMap<u32, RigidTransform>> = candidates
        .iter()
        .map(|&time| evaluate_node_poses(model, clip_index, time))
        .collect::<Result<Vec<_>, _>>()?;

    // Error-budget feasibility (R6336-9): a budget below the MAXIMUM adjacent
    // indivisible-step error cannot be honored, because somewhere in the clip
    // two consecutive ticks already exceed it and no subdivision can shrink a
    // single tick step. Reject with a typed impossibility error naming that
    // floor rather than accept a schedule that violates the interval invariant
    // on some step. (Using the minimum, as before, could pass a clip that still
    // has an over-budget adjacent step elsewhere.)
    let max_intertick = poses
        .windows(2)
        .map(|pair| pose_error(&pair[0], &pair[1]))
        .fold(0.0f64, f64::max);
    if settings.error_budget < max_intertick {
        return Err(PoseError::Validation(format!(
            "errorBudget {} is unsatisfiable at tick resolution: the clip's maximum indivisible adjacent-step pose error is {max_intertick}; either raise the budget to at least that floor or sample more finely",
            settings.error_budget
        )));
    }

    // Hard anchors are ONLY first, last, and caller-authored mandatory
    // timestamps (R6336-9). Events are best-effort, not requirements, so a
    // motion-rich clip does not become infeasible.
    let final_index = candidates.len() - 1;

    // Stage the complete error-bounded schedule independent of the output cap
    // (R6336-11): the hard anchors plus the minimal greedy subdivisions that
    // bring every retained interval within the error budget. Both the hard cap
    // and the error budget are guarantees, not best-effort: if the staged
    // schedule does not fit under max_frames, no schedule can honor cap +
    // budget + mandatory anchors at once, and that is a typed impossibility —
    // never a partial schedule that silently exceeds the budget, and never an
    // overflow of the cap.
    let mut staged: Vec<(usize, SelectionReason)> = vec![(0, SelectionReason::First)];
    let mut anchor = 0usize;
    for i in 1..=final_index {
        // Subdivide until the interval anchor→i measures within budget. The
        // feasibility preflight above guarantees every adjacent tick step is
        // within budget, so i-1 always strictly improves on anchor and this
        // loop terminates.
        while pose_error(&poses[anchor], &poses[i]) > settings.error_budget {
            if i - 1 == anchor {
                // Unreachable while the preflight holds; stay honest if the
                // sampling ever changes.
                return Err(PoseError::Validation(format!(
                    "errorBudget {} is unsatisfiable at tick resolution: an indivisible adjacent step before candidate index {i} exceeds it",
                    settings.error_budget
                )));
            }
            staged.push((i - 1, SelectionReason::ErrorBudget));
            anchor = i - 1;
        }
        if i == final_index {
            staged.push((i, SelectionReason::Last));
        } else if mandatory_times.contains(&candidates[i]) {
            staged.push((i, SelectionReason::Mandatory));
            anchor = i;
        }
    }

    // Cap feasibility (R6336-11): the complete error-bounded schedule must fit
    // under the hard cap as a whole; otherwise reject with a typed
    // impossibility naming the required frame count instead of returning a
    // partial schedule that violates either guarantee.
    if staged.len() > settings.max_frames {
        let subdivisions = staged
            .iter()
            .filter(|&&(_, reason)| reason == SelectionReason::ErrorBudget)
            .count();
        return Err(PoseError::Validation(format!(
            "max_frames {} cannot hold the error-bounded schedule: first + last + {distinct_mandatory} mandatory anchors + {subdivisions} required error subdivisions = {} frames; either raise max_frames, relax errorBudget, or drop mandatory anchors",
            settings.max_frames,
            staged.len()
        )));
    }

    // Events are best-effort: they fill only the slots the error-bounded
    // schedule did not need, and an event frame is kept only where splitting
    // its interval keeps BOTH halves within the error budget, so the measured
    // interval invariant holds for every returned frame.
    let mut kept = staged;
    let mut free_slots = settings.max_frames - kept.len();
    let mut i = 1usize;
    while i < final_index && free_slots > 0 {
        let pos = kept.partition_point(|&(idx, _)| idx < i);
        if pos < kept.len() && kept[pos].0 == i {
            i += 1;
            continue; // already kept as an anchor or subdivision
        }
        let prev = kept[pos - 1].0; // kept[0] is always (0, First)
        let next = kept[pos].0; // the final pose is always kept
        if exceeds_event(
            &poses[prev],
            &poses[i],
            settings.event_translation_threshold,
            settings.event_rotation_threshold,
        ) && pose_error(&poses[prev], &poses[i]) <= settings.error_budget
            && pose_error(&poses[i], &poses[next]) <= settings.error_budget
        {
            kept.insert(pos, (i, SelectionReason::Event));
            free_slots -= 1;
        }
        i += 1;
    }

    // Build the schedule with independent durations, deduplicating any
    // consecutive identical timestamps (an event frame landing exactly on the
    // final tick must not produce a zero-duration pose).
    let mut times: Vec<(u64, SelectionReason)> = Vec::new();
    for &(idx, reason) in &kept {
        let time = candidates[idx];
        if times.last().map(|&(t, _)| t) == Some(time) {
            // Keep the more meaningful reason (Last/Event over ErrorBudget).
            times.last_mut().expect("non-empty").1 = reason;
        } else {
            times.push((time, reason));
        }
    }
    let mut schedule = Vec::with_capacity(times.len());
    for (k, &(time, reason)) in times.iter().enumerate() {
        let next_time = times.get(k + 1).map(|&(t, _)| t).unwrap_or(duration);
        schedule.push(SelectedPose {
            time_microseconds: time,
            duration_microseconds: next_time.saturating_sub(time),
            reason,
        });
    }
    Ok(schedule)
}

/// Pose-space deviation between two poses: the max over nodes of translation
/// distance (cells) plus a rotation-distance term (radians → cells at a 1:1
/// weight for budget comparison).
/// Pose-space deviation between two poses (exposed for error-budget verification).
pub fn pose_error(a: &BTreeMap<u32, RigidTransform>, b: &BTreeMap<u32, RigidTransform>) -> f64 {
    let mut worst = 0.0f64;
    for (node, ta) in a {
        let Some(tb) = b.get(node) else { continue };
        let dt = ((ta.translation[0] - tb.translation[0]).powi(2)
            + (ta.translation[1] - tb.translation[1]).powi(2)
            + (ta.translation[2] - tb.translation[2]).powi(2))
        .sqrt();
        let rotation_distance = quaternion_angle(ta.rotation, tb.rotation);
        worst = worst.max(dt + rotation_distance);
    }
    worst
}

fn exceeds_event(
    a: &BTreeMap<u32, RigidTransform>,
    b: &BTreeMap<u32, RigidTransform>,
    translation_threshold: f64,
    rotation_threshold: f64,
) -> bool {
    for (node, ta) in a {
        let Some(tb) = b.get(node) else { continue };
        let dt = ((ta.translation[0] - tb.translation[0]).powi(2)
            + (ta.translation[1] - tb.translation[1]).powi(2)
            + (ta.translation[2] - tb.translation[2]).powi(2))
        .sqrt();
        if dt >= translation_threshold {
            return true;
        }
        if quaternion_angle(ta.rotation, tb.rotation) >= rotation_threshold {
            return true;
        }
    }
    false
}

fn quaternion_angle(a: [f64; 4], b: [f64; 4]) -> f64 {
    let dot = (a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3])
        .abs()
        .clamp(-1.0, 1.0);
    2.0 * dot.acos()
}

// ---------------------------------------------------------------------------
// Rough frame assembly
// ---------------------------------------------------------------------------

/// One voxel in a rough assembled frame, with its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssembledVoxelCell {
    pub coordinate: [i64; 3],
    pub material_slot: u16,
    /// The canonical part this voxel came from.
    pub part_id: u32,
    /// The source voxel index within that part.
    pub source_voxel_index: u32,
    /// Whether this voxel is in a not-yet-fused joint region (near a socket
    /// boundary between two parts) and therefore needs M3 fusion.
    pub needs_fusion: bool,
}

/// A rough assembled frame: the union of all bound parts at one pose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoughFrame {
    pub time_microseconds: u64,
    pub duration_microseconds: u64,
    pub voxels: Vec<AssembledVoxelCell>,
}

impl RoughFrame {
    pub fn len(&self) -> usize {
        self.voxels.len()
    }
    pub fn is_empty(&self) -> bool {
        self.voxels.is_empty()
    }
    pub fn bounds(&self) -> Option<([i64; 3], [i64; 3])> {
        let mut iter = self.voxels.iter().map(|v| v.coordinate);
        let first = iter.next()?;
        let mut lo = first;
        let mut hi = first;
        for c in iter {
            for axis in 0..3 {
                lo[axis] = lo[axis].min(c[axis]);
                hi[axis] = hi[axis].max(c[axis]);
            }
        }
        Some((lo, hi))
    }
    /// Voxels flagged as needing M3 joint fusion.
    pub fn fusion_candidates(&self) -> usize {
        self.voxels.iter().filter(|v| v.needs_fusion).count()
    }
}

/// Assemble one rough frame for `selected` from the rig-mapped parts.
///
/// Each bound part is rasterized under `bone_pose ∘ bind_transform ∘ part_local`
/// and merged into a single frame. Overlaps between parts resolve by part
/// declaration order (earlier part wins) and are marked as needing fusion; a
/// voxel within `fusion_margin` cells of another part's occupied cell is also
/// flagged, so M3 sees exactly the joint regions that need fusion rather than
/// the whole frame.
pub fn assemble_rough_frame(
    kit: &VoxelKit,
    rig_map: &RigMap,
    model: &ImportedAnimatedModel,
    clip_index: usize,
    selected: &SelectedPose,
    settings: &RasterSettings,
) -> Result<RoughFrame, PoseError> {
    let poses = evaluate_node_poses(model, clip_index, selected.time_microseconds)?;
    let part_indices: BTreeMap<&str, u32> = kit
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i as u32))
        .collect();

    let fusion_margin = 2i64;
    let mut voxels: Vec<AssembledVoxelCell> = Vec::new();
    let mut occupied: BTreeMap<[i64; 3], usize> = BTreeMap::new();

    for binding in &rig_map.bindings {
        let part = kit
            .part(&binding.part_id)
            .ok_or_else(|| PoseError::Validation(format!("unknown part {}", binding.part_id)))?;
        let part_index = part_indices[binding.part_id.as_str()];
        let bone = poses
            .get(&binding.bone_node_index)
            .copied()
            .unwrap_or(RigidTransform::IDENTITY);
        let placement = binding.placement(bone);
        for cell in rasterize_part(part, placement, settings)? {
            let coordinate = cell.coordinate;
            // Overlap: an earlier part already owns this cell.
            if occupied.contains_key(&coordinate) {
                // Mark the existing owner as needing fusion too.
                if let Some(&owner) = occupied.get(&coordinate) {
                    voxels[owner].needs_fusion = true;
                }
                continue;
            }
            let index = voxels.len();
            occupied.insert(coordinate, index);
            voxels.push(AssembledVoxelCell {
                coordinate,
                material_slot: cell.material_slot,
                part_id: part_index,
                source_voxel_index: cell.source_voxel_index,
                needs_fusion: false,
            });
        }
    }

    // Mark voxels near another part's cells as fusion candidates.
    let part_of: Vec<u32> = voxels.iter().map(|v| v.part_id).collect();
    let coords: Vec<[i64; 3]> = voxels.iter().map(|v| v.coordinate).collect();
    let cell_set: std::collections::BTreeSet<[i64; 3]> = coords.iter().copied().collect();
    for i in 0..voxels.len() {
        if voxels[i].needs_fusion {
            continue;
        }
        let c = coords[i];
        'neighbors: for dx in -fusion_margin..=fusion_margin {
            for dy in -fusion_margin..=fusion_margin {
                for dz in -fusion_margin..=fusion_margin {
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    let n = [c[0] + dx, c[1] + dy, c[2] + dz];
                    if !cell_set.contains(&n) {
                        continue;
                    }
                    // Find the owner's part for this neighbor.
                    if let Some(&owner_idx) = occupied.get(&n) {
                        if part_of[owner_idx] != part_of[i] {
                            voxels[i].needs_fusion = true;
                            break 'neighbors;
                        }
                    }
                }
            }
        }
    }

    voxels.sort_by_key(|v| v.coordinate);
    Ok(RoughFrame {
        time_microseconds: selected.time_microseconds,
        duration_microseconds: selected.duration_microseconds,
        voxels,
    })
}

/// Assemble rough frames for a whole selected schedule.
pub fn assemble_rough_schedule(
    kit: &VoxelKit,
    rig_map: &RigMap,
    model: &ImportedAnimatedModel,
    clip_index: usize,
    schedule: &[SelectedPose],
    settings: &RasterSettings,
) -> Result<Vec<RoughFrame>, PoseError> {
    schedule
        .iter()
        .map(|pose| assemble_rough_frame(kit, rig_map, model, clip_index, pose, settings))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn tf(x: f64, y: f64, z: f64) -> RigidTransform {
        RigidTransform {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [x, y, z],
        }
    }

    #[test]
    fn pose_error_tracks_translation() {
        let a: BTreeMap<u32, RigidTransform> = [(1, tf(0.0, 0.0, 0.0))].into_iter().collect();
        let b: BTreeMap<u32, RigidTransform> = [(1, tf(3.0, 4.0, 0.0))].into_iter().collect();
        assert!((pose_error(&a, &b) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn quaternion_angle_identity_and_opposite() {
        let i = [0.0, 0.0, 0.0, 1.0];
        assert!(quaternion_angle(i, i) < 1e-9);
        let half_turn = [0.0, 0.0, 1.0, 0.0];
        let angle = quaternion_angle(i, half_turn);
        assert!((angle - std::f64::consts::PI).abs() < 1e-6);
    }

    #[test]
    fn event_detection_threshold() {
        let a: BTreeMap<u32, RigidTransform> = [(1, tf(0.0, 0.0, 0.0))].into_iter().collect();
        let b: BTreeMap<u32, RigidTransform> = [(1, tf(3.0, 0.0, 0.0))].into_iter().collect();
        assert!(exceeds_event(&a, &b, 2.0, 0.35));
        let c: BTreeMap<u32, RigidTransform> = [(1, tf(0.5, 0.0, 0.0))].into_iter().collect();
        assert!(!exceeds_event(&a, &c, 2.0, 0.35));
    }
}
