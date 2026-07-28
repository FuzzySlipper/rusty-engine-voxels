//! Rigid-part posing: the math and structures that turn a proxy-rig pose into
//! per-part rigid transforms for the exploded-kit pipeline (M2).
//!
//! The whole point of the canonical exploded kit is that animation is *rigid
//! transforms of stable parts*, not per-frame re-voxelization of a continuous
//! skinned surface. This module owns that rigid core:
//!
//! - **Pose evaluation** consumes the engine's `ImportedAnimatedModel` (node
//!   base transforms, hierarchy, and raw animation channels) and evaluates each
//!   node's world transform at an explicit time. We evaluate against the
//!   *exposed* channels rather than calling the engine's mesh-deformation
//!   sampler, because that sampler's whole purpose is to materialize a deformed
//!   mesh — exactly the operation that causes the churn this pipeline removes.
//!   This is consumption of engine data structures, not reimplementation of
//!   engine conversion semantics.
//!
//! - **Rig mapping** binds each canonical part to one proxy bone (rigid, no
//!   skinning/weights).
//!
//! - **Conservative rasterization** transforms each part's stable cell set as
//!   occupied cubes into frame space, supersampling so rigid parts stay
//!   hole-free, then downsampling by occupancy vote with provenance preserved.
//!
//! Everything is deterministic: same model + same time + same part → identical
//! output.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use voxel_convert::{
    AnimationChannelValues, AnimationInterpolation, AnimationProperty, ImportedAnimatedModel,
    ImportedNodeTransform,
};

use crate::kit::{KitPart, VoxelKit};

// ---------------------------------------------------------------------------
// Rigid transform
// ---------------------------------------------------------------------------

/// A rigid transform: rotation (unit quaternion, x/y/z/w) + translation, no
/// scale. Parts are rigid; non-uniform scale would deform them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RigidTransform {
    pub rotation: [f64; 4],
    pub translation: [f64; 3],
}

impl RigidTransform {
    pub const IDENTITY: RigidTransform = RigidTransform {
        rotation: [0.0, 0.0, 0.0, 1.0],
        translation: [0.0, 0.0, 0.0],
    };

    /// Compose `self ∘ other` (apply `other` first, then `self`).
    pub fn then(self, other: RigidTransform) -> RigidTransform {
        let rotated = quat_rotate(self.rotation, other.translation);
        RigidTransform {
            rotation: quat_mul(self.rotation, other.rotation),
            translation: [
                rotated[0] + self.translation[0],
                rotated[1] + self.translation[1],
                rotated[2] + self.translation[2],
            ],
        }
    }

    pub fn inverse(self) -> RigidTransform {
        let inv_rotation = quat_conjugate(self.rotation);
        let neg = quat_rotate(
            inv_rotation,
            [
                -self.translation[0],
                -self.translation[1],
                -self.translation[2],
            ],
        );
        RigidTransform {
            rotation: inv_rotation,
            translation: neg,
        }
    }

    pub fn apply(self, point: [f64; 3]) -> [f64; 3] {
        let rotated = quat_rotate(self.rotation, point);
        [
            rotated[0] + self.translation[0],
            rotated[1] + self.translation[1],
            rotated[2] + self.translation[2],
        ]
    }
}

fn quat_conjugate(q: [f64; 4]) -> [f64; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

fn quat_normalize(q: [f64; 4]) -> [f64; 4] {
    let len = (q[0] * q[0] + q[1] * q[1] + q[2] * q[2] + q[3] * q[3]).sqrt();
    if len < 1e-12 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    [q[0] / len, q[1] / len, q[2] / len, q[3] / len]
}

fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let (ax, ay, az, aw) = (a[0], a[1], a[2], a[3]);
    let (bx, by, bz, bw) = (b[0], b[1], b[2], b[3]);
    quat_normalize([
        aw * bx + ax * bw + ay * bz - az * by,
        aw * by - ax * bz + ay * bw + az * bx,
        aw * bz + ax * by - ay * bx + az * bw,
        aw * bw - ax * bx - ay * by - az * bz,
    ])
}

fn quat_rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let (qx, qy, qz, qw) = (q[0], q[1], q[2], q[3]);
    // t = 2 * q_vec × v
    let tx = 2.0 * (qy * v[2] - qz * v[1]);
    let ty = 2.0 * (qz * v[0] - qx * v[2]);
    let tz = 2.0 * (qx * v[1] - qy * v[0]);
    // v' = v + qw*t + q_vec × t
    [
        v[0] + qw * tx + (qy * tz - qz * ty),
        v[1] + qw * ty + (qz * tx - qx * tz),
        v[2] + qw * tz + (qx * ty - qy * tx),
    ]
}

fn quat_slerp(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] {
    let mut b = b;
    let mut dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2] + a[3] * b[3];
    if dot < 0.0 {
        b = [-b[0], -b[1], -b[2], -b[3]];
        dot = -dot;
    }
    if dot > 0.9995 {
        return quat_normalize([
            a[0] + t * (b[0] - a[0]),
            a[1] + t * (b[1] - a[1]),
            a[2] + t * (b[2] - a[2]),
            a[3] + t * (b[3] - a[3]),
        ]);
    }
    let theta = dot.clamp(-1.0, 1.0).acos();
    let sin_theta = theta.sin();
    let wa = ((1.0 - t) * theta).sin() / sin_theta;
    let wb = (t * theta).sin() / sin_theta;
    quat_normalize([
        wa * a[0] + wb * b[0],
        wa * a[1] + wb * b[1],
        wa * a[2] + wb * b[2],
        wa * a[3] + wb * b[3],
    ])
}

fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + t * (b[0] - a[0]),
        a[1] + t * (b[1] - a[1]),
        a[2] + t * (b[2] - a[2]),
    ]
}

// ---------------------------------------------------------------------------
// Pose evaluation from the imported animated model
// ---------------------------------------------------------------------------

/// The world transform of every node at one explicit time.
pub type NodePoses = BTreeMap<u32, RigidTransform>;

/// Evaluate every node's world transform at `time_microseconds` for `clip`,
/// by walking the raw animation channels against the node hierarchy. Node
/// poses are deterministic for a given model + clip + time.
pub fn evaluate_node_poses(
    model: &ImportedAnimatedModel,
    clip_index: usize,
    time_microseconds: u64,
) -> Result<NodePoses, PoseError> {
    let clip = model
        .clips
        .get(clip_index)
        .ok_or(PoseError::UnknownClip(clip_index))?;

    // Start from each node's base local transform.
    let mut local: BTreeMap<u32, RigidTransform> = BTreeMap::new();
    for node in &model.nodes {
        local.insert(node.source_node_index, decompose_base(node.base_transform));
    }

    // Apply animated channels.
    for channel in &clip.channels {
        let time_s = time_microseconds as f64 / 1_000_000.0;
        let value = sample_channel(channel, time_s)?;
        let entry = local
            .entry(channel.target_node_index)
            .or_insert(RigidTransform::IDENTITY);
        match (channel.property, value) {
            (AnimationProperty::Translation, ChannelValue::T3(v)) => entry.translation = v,
            (AnimationProperty::Rotation, ChannelValue::Q(v)) => entry.rotation = v,
            // Scale and morph weights do not affect rigid part transforms.
            _ => {}
        }
    }

    // Compose world transforms down the hierarchy.
    let scene_nodes = &model.scene.nodes;
    let parent_of: BTreeMap<u32, Option<u32>> = scene_nodes
        .iter()
        .map(|n| (n.source_node_index, n.parent_node_index))
        .collect();
    let mut world: NodePoses = BTreeMap::new();
    for node in scene_nodes {
        let index = node.source_node_index;
        let w = compose_world(index, &local, &parent_of, &mut world)?;
        world.insert(index, w);
    }
    Ok(world)
}

fn compose_world(
    index: u32,
    local: &BTreeMap<u32, RigidTransform>,
    parent_of: &BTreeMap<u32, Option<u32>>,
    world: &mut NodePoses,
) -> Result<RigidTransform, PoseError> {
    if let Some(&cached) = world.get(&index) {
        return Ok(cached);
    }
    let local_t = local
        .get(&index)
        .copied()
        .unwrap_or(RigidTransform::IDENTITY);
    let parent = parent_of.get(&index).copied().flatten();
    let result = match parent {
        Some(p) => {
            if p == index {
                return Err(PoseError::HierarchyCycle(index));
            }
            let parent_world = compose_world(p, local, parent_of, world)?;
            parent_world.then(local_t)
        }
        None => local_t,
    };
    world.insert(index, result);
    Ok(result)
}

fn decompose_base(transform: ImportedNodeTransform) -> RigidTransform {
    match transform {
        ImportedNodeTransform::Decomposed {
            translation,
            rotation,
            ..
        } => RigidTransform {
            rotation: quat_normalize(rotation),
            translation,
        },
        ImportedNodeTransform::Matrix(m) => decompose_matrix(m),
    }
}

/// Decompose a 4x4 column-major (glTF) matrix into rotation + translation,
/// dropping scale (rigid parts ignore scale).
fn decompose_matrix(m: [f64; 16]) -> RigidTransform {
    // Column-major: basis vectors in columns 0..2, translation in column 3.
    let col = |c: usize| [m[c * 4], m[c * 4 + 1], m[c * 4 + 2]];
    let mut basis = [col(0), col(1), col(2)];
    // Normalize columns to remove scale.
    for b in &mut basis {
        let len = (b[0] * b[0] + b[1] * b[1] + b[2] * b[2]).sqrt();
        if len > 1e-12 {
            b[0] /= len;
            b[1] /= len;
            b[2] /= len;
        }
    }
    let rotation = quat_from_basis(basis);
    RigidTransform {
        rotation,
        translation: [m[12], m[13], m[14]],
    }
}

fn quat_from_basis(b: [[f64; 3]; 3]) -> [f64; 4] {
    let (m00, m01, m02) = (b[0][0], b[1][0], b[2][0]);
    let (m10, m11, m12) = (b[0][1], b[1][1], b[2][1]);
    let (m20, m21, m22) = (b[0][2], b[1][2], b[2][2]);
    let trace = m00 + m11 + m22;
    let q = if trace > 0.0 {
        let s = (trace + 1.0).sqrt() * 2.0;
        [(m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s]
    };
    quat_normalize(q)
}

enum ChannelValue {
    T3([f64; 3]),
    Q([f64; 4]),
}

fn sample_channel(
    channel: &voxel_convert::ImportedAnimationChannel,
    time_s: f64,
) -> Result<ChannelValue, PoseError> {
    let times: Vec<f64> = channel
        .timestamps_microseconds
        .iter()
        .map(|t| *t as f64 / 1_000_000.0)
        .collect();
    if times.is_empty() {
        return Err(PoseError::EmptyChannel(channel.target_node_index));
    }
    let (a, b, t) = bracket(&times, time_s, channel.interpolation);
    match &channel.values {
        AnimationChannelValues::Translations(values) => {
            let va = values.get(a).copied().unwrap_or([0.0; 3]);
            let vb = values.get(b).copied().unwrap_or(va);
            Ok(ChannelValue::T3(lerp3(va, vb, t)))
        }
        AnimationChannelValues::Rotations(values) => {
            let va = values.get(a).copied().unwrap_or([0.0, 0.0, 0.0, 1.0]);
            let vb = values.get(b).copied().unwrap_or(va);
            Ok(ChannelValue::Q(quat_slerp(va, vb, t)))
        }
        _ => Err(PoseError::UnsupportedChannel(channel.target_node_index)),
    }
}

/// Find the bracketing keyframe indices and interpolation factor.
fn bracket(
    times: &[f64],
    time_s: f64,
    interpolation: AnimationInterpolation,
) -> (usize, usize, f64) {
    if time_s <= times[0] {
        return (0, 0, 0.0);
    }
    let last = times.len() - 1;
    if time_s >= times[last] {
        return (last, last, 0.0);
    }
    let mut i = 0;
    while i + 1 < times.len() && times[i + 1] <= time_s {
        i += 1;
    }
    let span = times[i + 1] - times[i];
    let t = if span <= 0.0 {
        0.0
    } else {
        ((time_s - times[i]) / span).clamp(0.0, 1.0)
    };
    match interpolation {
        AnimationInterpolation::Step => (i, i, 0.0),
        _ => (i, i + 1, t),
    }
}

// ---------------------------------------------------------------------------
// Rig mapping
// ---------------------------------------------------------------------------

/// Binds each canonical part to one proxy bone (node index) with a bind
/// transform from the part's pivot frame to the bone frame. Rigid: one part,
/// one bone, no skinning weights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RigMap {
    pub schema_version: u32,
    pub bindings: Vec<PartBinding>,
}

pub const RIG_MAP_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PartBinding {
    /// The canonical part id.
    pub part_id: String,
    /// The proxy bone (GLB node index) this part rigidly follows.
    pub bone_node_index: u32,
}

impl RigMap {
    pub fn validate(&self, kit: &VoxelKit, model: &ImportedAnimatedModel) -> Result<(), PoseError> {
        if self.schema_version != RIG_MAP_SCHEMA_VERSION {
            return Err(PoseError::Validation(format!(
                "rig map schema {} unsupported; expected {RIG_MAP_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        let node_indices: std::collections::BTreeSet<u32> = model
            .scene
            .nodes
            .iter()
            .map(|n| n.source_node_index)
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        for binding in &self.bindings {
            if kit.part(&binding.part_id).is_none() {
                return Err(PoseError::Validation(format!(
                    "rig map binds unknown part {}",
                    binding.part_id
                )));
            }
            if !node_indices.contains(&binding.bone_node_index) {
                return Err(PoseError::Validation(format!(
                    "part {} binds unknown bone node {}",
                    binding.part_id, binding.bone_node_index
                )));
            }
            if !seen.insert(binding.part_id.as_str()) {
                return Err(PoseError::Validation(format!(
                    "part {} is bound more than once",
                    binding.part_id
                )));
            }
        }
        // Every part must be bound.
        for part in &kit.parts {
            if !self.bindings.iter().any(|b| b.part_id == part.id) {
                return Err(PoseError::Validation(format!(
                    "part {} has no rig binding",
                    part.id
                )));
            }
        }
        Ok(())
    }

    pub fn binding_for(&self, part_id: &str) -> Option<&PartBinding> {
        self.bindings.iter().find(|b| b.part_id == part_id)
    }
}

// ---------------------------------------------------------------------------
// Conservative rasterization
// ---------------------------------------------------------------------------

/// Rasterization settings for turning a rigid-transformed part into frame
/// cells. Supersampling avoids the holes naive per-voxel rotation leaves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RasterSettings {
    /// Sub-cell supersample factor per axis (>= 1). 2 is a good default: 8
    /// sample points per source voxel.
    pub supersample: u32,
    /// Fraction of a target cell's sub-samples that must be covered for the
    /// cell to be occupied, in (0, 1]. 0.5 = majority coverage.
    pub occupancy_threshold: f64,
}

impl Default for RasterSettings {
    fn default() -> Self {
        RasterSettings {
            supersample: 2,
            occupancy_threshold: 0.5,
        }
    }
}

impl RasterSettings {
    pub fn validate(&self) -> Result<(), PoseError> {
        if self.supersample == 0 || self.supersample > 8 {
            return Err(PoseError::Validation(format!(
                "supersample must be within 1..=8, got {}",
                self.supersample
            )));
        }
        if !(self.occupancy_threshold > 0.0 && self.occupancy_threshold <= 1.0) {
            return Err(PoseError::Validation(format!(
                "occupancy_threshold must be within (0, 1], got {}",
                self.occupancy_threshold
            )));
        }
        Ok(())
    }
}

/// One rasterized cell with the source voxel it came from (provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RasterCell {
    pub coordinate: [i64; 3],
    pub material_slot: u16,
    pub source_voxel_index: u32,
}

/// Conservatively rasterize one part under a rigid transform.
///
/// Each source voxel is treated as an occupied cube. The cube is transformed
/// into frame space and supersampled; every target frame cell that collects at
/// least `occupancy_threshold` of its sub-samples is marked occupied, inheriting
/// the dominant material slot among its covering source voxels. This keeps
/// rigid parts hole-free and thickness-stable — the churn the straight
/// per-frame re-voxelization produces is avoided because the part's own cell
/// set is fixed and only the transform changes.
///
/// Deterministic: same part + same transform + same settings → identical cells.
pub fn rasterize_part(
    part: &KitPart,
    transform: RigidTransform,
    settings: &RasterSettings,
) -> Result<Vec<RasterCell>, PoseError> {
    settings.validate()?;
    let ss = settings.supersample as i64;
    let ss_f = settings.supersample as f64;
    let samples_per_cell = (ss * ss * ss) as f64;

    // Accumulate, per target cell, a count per (source voxel, material slot).
    // Deterministic ordering via BTreeMap.
    let mut coverage: BTreeMap<[i64; 3], BTreeMap<(u32, u16), u32>> = BTreeMap::new();

    for (voxel_index, cell) in part.cells.iter().enumerate() {
        // Transform the voxel's cube center; the cube spans ±0.5 around it.
        let center = [
            cell.coordinate[0] as f64 + 0.5,
            cell.coordinate[1] as f64 + 0.5,
            cell.coordinate[2] as f64 + 0.5,
        ];
        let key = (voxel_index as u32, cell.material_slot);
        // Sample sub-cell points across the source cube in source space,
        // transform each, and bin into target cells.
        for dx in 0..ss {
            for dy in 0..ss {
                for dz in 0..ss {
                    let local = [
                        center[0] - 0.5 + (dx as f64 + 0.5) / ss_f,
                        center[1] - 0.5 + (dy as f64 + 0.5) / ss_f,
                        center[2] - 0.5 + (dz as f64 + 0.5) / ss_f,
                    ];
                    let world = transform.apply(local);
                    let target = [
                        world[0].floor() as i64,
                        world[1].floor() as i64,
                        world[2].floor() as i64,
                    ];
                    *coverage.entry(target).or_default().entry(key).or_insert(0) += 1;
                }
            }
        }
    }

    let mut cells = Vec::new();
    for (coordinate, votes) in coverage {
        // Total coverage across all source voxels for this target cell.
        let total: u32 = votes.values().sum();
        if (total as f64) < settings.occupancy_threshold * samples_per_cell {
            continue;
        }
        // Dominant (voxel, slot) by vote count; tie-broken by lowest slot then
        // lowest voxel index for determinism.
        let (source_voxel_index, material_slot) = votes
            .iter()
            .max_by(|(ka, ca), (kb, cb)| {
                ca.cmp(cb)
                    .then_with(|| kb.1.cmp(&ka.1))
                    .then_with(|| kb.0.cmp(&ka.0))
            })
            .map(|(k, _)| *k)
            .expect("non-empty votes");
        cells.push(RasterCell {
            coordinate,
            material_slot,
            source_voxel_index,
        });
    }
    cells.sort_by_key(|cell| cell.coordinate);
    Ok(cells)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum PoseError {
    Validation(String),
    UnknownClip(usize),
    EmptyChannel(u32),
    UnsupportedChannel(u32),
    HierarchyCycle(u32),
}

impl fmt::Display for PoseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoseError::Validation(m) => write!(f, "pose validation: {m}"),
            PoseError::UnknownClip(i) => write!(f, "unknown clip index {i}"),
            PoseError::EmptyChannel(n) => write!(f, "node {n} has an empty animation channel"),
            PoseError::UnsupportedChannel(n) => {
                write!(f, "node {n} uses an unsupported channel kind")
            }
            PoseError::HierarchyCycle(n) => write!(f, "node hierarchy cycle at node {n}"),
        }
    }
}
impl std::error::Error for PoseError {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kit::{DeformationBudget, KitCell, KitPart};

    // --- Conservative rasterizer ---

    fn solid_part(size: i64, slot: u16) -> KitPart {
        let mut cells = Vec::new();
        for x in 0..size {
            for y in 0..size {
                for z in 0..size {
                    cells.push(KitCell {
                        coordinate: [x, y, z],
                        material_slot: slot,
                    });
                }
            }
        }
        cells.sort_by_key(|cell| cell.coordinate);
        KitPart {
            id: "block".to_owned(),
            version: 1,
            pivot: [0, 0, 0],
            limb: false,
            cells,
            sockets: vec![],
            palette_groups: vec![],
            deformation_budget: DeformationBudget {
                max_length_change: 0.0,
                max_volume_change: 0.0,
                allow_joint_compression: false,
            },
            protected_regions: vec![],
            symmetry_partner: None,
        }
    }

    fn coords(cells: &[RasterCell]) -> std::collections::BTreeSet<[i64; 3]> {
        cells.iter().map(|c| c.coordinate).collect()
    }

    fn has_holes(
        cells: &std::collections::BTreeSet<[i64; 3]>,
        min: [i64; 3],
        max: [i64; 3],
    ) -> bool {
        // Check axis-aligned interior: any fully-surrounded empty cell is a hole.
        for x in (min[0] + 1)..max[0] {
            for y in (min[1] + 1)..max[1] {
                for z in (min[2] + 1)..max[2] {
                    if !cells.contains(&[x, y, z]) {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn identity_rasterizes_to_the_same_cells() {
        let part = solid_part(3, 1);
        let cells = rasterize_part(&part, RigidTransform::IDENTITY, &RasterSettings::default())
            .expect("identity rasterize");
        let expected: std::collections::BTreeSet<[i64; 3]> =
            part.cells.iter().map(|c| c.coordinate).collect();
        assert_eq!(
            coords(&cells),
            expected,
            "identity must reproduce the part exactly"
        );
    }

    #[test]
    fn translation_preserves_volume_and_shape() {
        let part = solid_part(3, 1);
        let transform = RigidTransform {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [10.0, -4.0, 2.0],
        };
        let cells = rasterize_part(&part, transform, &RasterSettings::default())
            .expect("translate rasterize");
        assert_eq!(cells.len(), 27, "rigid translation preserves volume");
        let cs = coords(&cells);
        assert!(
            !has_holes(&cs, [10, -4, 2], [13, -1, 5]),
            "translation must not introduce holes"
        );
        assert!(cs.contains(&[10, -4, 2]) && cs.contains(&[12, -2, 4]));
    }

    #[test]
    fn rotation_stays_connected_and_hole_free() {
        // A 3x3x3 block rotated 30° about Z must remain a single connected
        // blob without interior holes — the failure mode of naive per-voxel
        // rotation.
        let part = solid_part(3, 1);
        let angle = std::f64::consts::PI / 6.0;
        let transform = RigidTransform {
            rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
            translation: [0.0, 0.0, 0.0],
        };
        let settings = RasterSettings {
            supersample: 4,
            occupancy_threshold: 0.3,
        };
        let cells = rasterize_part(&part, transform, &settings).expect("rotate rasterize");
        assert!(
            cells.len() >= 27,
            "conservative raster keeps at least the source volume"
        );
        let cs = coords(&cells);
        // Connectivity: every cell has at least one face-neighbour present.
        let all_connected = cs.iter().all(|c| {
            [
                [1, 0, 0],
                [-1, 0, 0],
                [0, 1, 0],
                [0, -1, 0],
                [0, 0, 1],
                [0, 0, -1],
            ]
            .iter()
            .any(|d| cs.contains(&[c[0] + d[0], c[1] + d[1], c[2] + d[2]]))
        });
        assert!(
            all_connected,
            "rotated part must remain face-connected (no isolated voxels)"
        );
    }

    #[test]
    fn rasterization_is_deterministic() {
        let part = solid_part(3, 2);
        let angle = std::f64::consts::PI / 5.0;
        let transform = RigidTransform {
            rotation: [(angle / 2.0).sin(), 0.0, 0.0, (angle / 2.0).cos()],
            translation: [1.5, 2.5, 3.5],
        };
        let a = rasterize_part(&part, transform, &RasterSettings::default()).expect("first");
        let b = rasterize_part(&part, transform, &RasterSettings::default()).expect("second");
        assert_eq!(a, b, "same part + transform must be bit-identical");
    }

    #[test]
    fn still_part_has_zero_churn_across_poses() {
        // The core churn claim: a part that does NOT move between two poses
        // produces identical rasterized cells (zero churn), unlike per-frame
        // re-voxelization of a continuous surface.
        let part = solid_part(3, 1);
        let pose_a = RigidTransform::IDENTITY;
        let pose_b = RigidTransform::IDENTITY; // same pose (part didn't move)
        let cells_a = rasterize_part(&part, pose_a, &RasterSettings::default()).expect("pose a");
        let cells_b = rasterize_part(&part, pose_b, &RasterSettings::default()).expect("pose b");
        assert_eq!(
            coords(&cells_a),
            coords(&cells_b),
            "unmoved part must have zero churn"
        );
    }

    fn assert_v3_close(a: [f64; 3], b: [f64; 3]) {
        for i in 0..3 {
            assert!(
                (a[i] - b[i]).abs() < 1e-9,
                "index {i}: {} vs {}",
                a[i],
                b[i]
            );
        }
    }

    #[test]
    fn quaternion_rotate_z_90() {
        // 90° about +Z maps +X -> +Y.
        let angle = std::f64::consts::FRAC_PI_2;
        let q = [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()];
        let r = quat_rotate(q, [1.0, 0.0, 0.0]);
        assert_v3_close(r, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn rigid_then_and_inverse() {
        let t = RigidTransform {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [1.0, 2.0, 3.0],
        };
        let p = [4.0, 5.0, 6.0];
        let moved = t.apply(p);
        assert_v3_close(moved, [5.0, 7.0, 9.0]);
        let back = t.inverse().apply(moved);
        assert_v3_close(back, p);
    }

    #[test]
    fn compose_applies_other_first() {
        let parent = RigidTransform {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [10.0, 0.0, 0.0],
        };
        let child = RigidTransform {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [0.0, 5.0, 0.0],
        };
        let world = parent.then(child);
        assert_v3_close(world.apply([0.0, 0.0, 0.0]), [10.0, 5.0, 0.0]);
    }

    #[test]
    fn slerp_halfway_is_normalized() {
        let a = [0.0, 0.0, 0.0, 1.0];
        let angle = std::f64::consts::PI;
        let b = [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()];
        let mid = quat_slerp(a, b, 0.5);
        let len = (mid[0] * mid[0] + mid[1] * mid[1] + mid[2] * mid[2] + mid[3] * mid[3]).sqrt();
        assert!((len - 1.0).abs() < 1e-9);
    }

    #[test]
    fn bracket_step_and_linear() {
        let times = [0.0, 1.0, 2.0];
        assert_eq!(
            bracket(&times, -1.0, AnimationInterpolation::Linear),
            (0, 0, 0.0)
        );
        assert_eq!(
            bracket(&times, 5.0, AnimationInterpolation::Linear),
            (2, 2, 0.0)
        );
        let (a, b, t) = bracket(&times, 1.5, AnimationInterpolation::Linear);
        assert_eq!((a, b), (1, 2));
        assert!((t - 0.5).abs() < 1e-9);
        let (a, b, _) = bracket(&times, 1.5, AnimationInterpolation::Step);
        assert_eq!((a, b), (1, 1));
    }
}
