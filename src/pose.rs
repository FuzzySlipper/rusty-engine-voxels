//! Rigid-part posing: the math and structures that turn a proxy-rig pose into
//! per-part rigid transforms for the exploded-kit pipeline (M2).
//!
//! The whole point of the canonical exploded kit is that animation is *rigid
//! transforms of stable parts*, not per-frame re-voxelization of a continuous
//! skinned surface. This module owns that rigid core:
//!
//! - **Pose evaluation** consumes the Engine-owned `evaluate_clip_node_poses`
//!   seam (rusty-engine #6348), which evaluates per-node affine world transforms
//!   for a clip + explicit time with canonical Step/Linear/CubicSpline, scale,
//!   hierarchy, finite, and duration semantics — *without* materializing
//!   deformed meshes. This module admits those affine poses to rigid placement
//!   under the Engine's explicit `NodePoseRigidScalePolicy` and converts the
//!   admitted matrices into its rigid-transform vocabulary. We do not evaluate
//!   animation channels locally; that is the Engine's authority.
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
use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use voxel_convert::{evaluate_clip_node_poses, ImportedAnimatedModel, NodePoseRigidScalePolicy};

use crate::kit::{KitPart, VoxelKit};

// ---------------------------------------------------------------------------
// Rigid transform
// ---------------------------------------------------------------------------

/// A rigid transform: rotation (unit quaternion, x/y/z/w) + translation, no
/// scale. Parts are rigid; non-uniform scale would deform them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// Compose a quaternion from per-axis euler deltas in degrees, applied X
/// first, then Y, then Z (Rz * Ry * Rx). This is the pose-spec authoring
/// convention for manual pivot rotations (see `crate::posed`).
pub fn euler_degrees_to_quaternion(x_deg: f64, y_deg: f64, z_deg: f64) -> [f64; 4] {
    let qx = axis_angle([1.0, 0.0, 0.0], x_deg);
    let qy = axis_angle([0.0, 1.0, 0.0], y_deg);
    let qz = axis_angle([0.0, 0.0, 1.0], z_deg);
    quat_mul(qz, quat_mul(qy, qx))
}

fn axis_angle(axis: [f64; 3], degrees: f64) -> [f64; 4] {
    let half = degrees.to_radians() * 0.5;
    let s = half.sin();
    [axis[0] * s, axis[1] * s, axis[2] * s, half.cos()]
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

#[cfg(test)]
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
    let clip_name = model
        .clips
        .get(clip_index)
        .map(|clip| clip.name.as_str())
        .ok_or(PoseError::UnknownClip(clip_index))?;
    evaluate_node_poses_by_name(model, clip_name, time_microseconds)
}

/// Evaluate node poses by clip name through the Engine seam, admitting each
/// affine world transform to rigid placement under `policy`.
pub fn evaluate_node_poses_by_name(
    model: &ImportedAnimatedModel,
    clip_name: &str,
    time_microseconds: u64,
) -> Result<NodePoses, PoseError> {
    evaluate_node_poses_with_policy(
        model,
        clip_name,
        time_microseconds,
        NodePoseRigidScalePolicy::AllowUniformScale,
    )
}

/// The explicit admitted-scale policy used when converting Engine affine poses
/// to rigid placement: one positive uniform scale is admitted (the retro-character
/// rig carries benign uniform scale); non-uniform scale, shear, singular axes,
/// and reflections remain Engine typed failures.
pub const RIGID_SCALE_POLICY: NodePoseRigidScalePolicy =
    NodePoseRigidScalePolicy::AllowUniformScale;

/// Evaluate node poses through the Engine `evaluate_clip_node_poses` seam and
/// admit each affine world transform to rigid placement under the
/// [`RIGID_SCALE_POLICY`], then convert the admitted matrices into rigid
/// transforms. Out-of-range times, missing clips, non-finite values, and
/// hierarchy cycles surface as the Engine's typed `ConversionError`, mapped
/// here without loss of meaning.
pub fn evaluate_node_poses_with_policy(
    model: &ImportedAnimatedModel,
    clip_name: &str,
    time_microseconds: u64,
    policy: NodePoseRigidScalePolicy,
) -> Result<NodePoses, PoseError> {
    let receipt = evaluate_clip_node_poses(model, clip_name, time_microseconds)
        .map_err(|error| PoseError::EngineEvaluation(error.to_string()))?;
    let mut poses = BTreeMap::new();
    for node in &receipt.nodes {
        let admitted = admit_node_world_transform(node, policy)?;
        poses.insert(
            node.source_node_index,
            decompose_matrix_with_uniform_scale(
                admitted.affine_world_transform,
                admitted.uniform_scale,
            ),
        );
    }
    Ok(poses)
}

/// Admit one node's affine world transform to rigid placement.
///
/// The Engine's `admit_rigid_world_transform` enforces *strict* per-axis
/// uniformity at an absolute tolerance (`1e-6 * max(1, scale)`), which rejects
/// genuinely-uniform rigs whose axes carry only floating-point jitter at large
/// scale (the retro-character rig is uniformly scaled by ~100 and its axes
/// differ by ~1e-4 relative). That strictness is correct as the Engine's
/// conservative default, but it would exclude this rig. Our admitted policy is
/// therefore: use the Engine admission whenever it accepts, and otherwise
/// re-check *uniform* scale against a **relative** tolerance — the rig must be
/// uniform within `UNIFORM_SCALE_RELATIVE_TOLERANCE` of its mean scale — while
/// still requiring finite values, non-singular axes, orthogonality (no shear),
/// and a proper (non-reflecting) basis. Non-uniform scale, shear, and
/// reflections remain hard failures regardless of tolerance.
pub fn admit_node_world_transform(
    node: &voxel_convert::AnimationNodePose,
    policy: NodePoseRigidScalePolicy,
) -> Result<voxel_convert::AdmittedRigidNodePose, PoseError> {
    match node.admit_rigid_world_transform(policy) {
        Ok(admitted) => Ok(admitted),
        Err(strict_error) => {
            let admitted = admit_uniform_scale_with_relative_tolerance(node, policy)?;
            let _ = strict_error; // the Engine's stricter default is documented above.
            Ok(admitted)
        }
    }
}

/// The admitted relative tolerance for uniform scale: the per-axis scales must
/// agree within this fraction of the mean scale. Loose enough to admit a
/// genuinely-uniform 100x rig's floating-point jitter, tight enough that a
/// truly non-uniform rig (a limb stretched on one axis) is still rejected.
const UNIFORM_SCALE_RELATIVE_TOLERANCE: f64 = 1.0e-3;

/// The absolute tolerance for the *other* rigid invariants (orthogonality,
/// determinant, non-singularity). Matches the Engine's rigid tolerance so the
/// fallback only ever relaxes uniform-scale uniformity, never rigidity itself.
const RIGID_INVARIANT_TOLERANCE: f64 = 1.0e-4;

/// Re-validate every Engine rigid invariant the strict check enforces, but
/// with the *uniform-scale uniformity* measured at the relative tolerance the
/// caller admitted. Shear, reflection, singular axes, and non-finite values
/// are hard failures at `RIGID_INVARIANT_TOLERANCE`; only the per-axis scale
/// *uniformity* threshold is relaxed (for the ~100x retro-character rig's fp
/// jitter). This closes the gap where the previous fallback admitted
/// equal-length non-orthogonal (shear) or reflected bases.
fn admit_uniform_scale_with_relative_tolerance(
    node: &voxel_convert::AnimationNodePose,
    policy: NodePoseRigidScalePolicy,
) -> Result<voxel_convert::AdmittedRigidNodePose, PoseError> {
    let m = node.world_transform;
    let reject = |reason: &str| PoseError::NonRigidPose {
        node: node.source_node_index,
        reason: reason.to_owned(),
    };

    // 1. Every affine component (basis + translation) must be finite.
    if !m.iter().all(|component| component.is_finite()) {
        return Err(reject("world transform is not finite affine data"));
    }

    let columns = [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]];
    let length = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let scales = columns.map(length);

    // 2. Non-singular axes.
    if scales
        .iter()
        .any(|scale| *scale <= RIGID_INVARIANT_TOLERANCE)
    {
        return Err(reject("world transform has a singular scale axis"));
    }

    // 3. Orthogonality (no shear): normalized axes must be mutually orthogonal.
    let axes: [[f64; 3]; 3] = std::array::from_fn(|i| {
        let c = columns[i];
        [c[0] / scales[i], c[1] / scales[i], c[2] / scales[i]]
    });
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    if dot(axes[0], axes[1]).abs() > RIGID_INVARIANT_TOLERANCE
        || dot(axes[0], axes[2]).abs() > RIGID_INVARIANT_TOLERANCE
        || dot(axes[1], axes[2]).abs() > RIGID_INVARIANT_TOLERANCE
    {
        return Err(reject("world transform contains shear"));
    }

    // 4. Proper basis (no reflection): determinant of the normalized basis ~ +1.
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let determinant = dot(cross(axes[0], axes[1]), axes[2]);
    if (determinant - 1.0).abs() > RIGID_INVARIANT_TOLERANCE {
        return Err(reject(
            "world transform contains a reflection or a non-rigid basis",
        ));
    }

    // 5. Uniform scale at the admitted *relative* tolerance (the only relaxed
    //    invariant): per-axis scales agree within a fraction of the mean.
    let mean = (scales[0] + scales[1] + scales[2]) / 3.0;
    if scales
        .iter()
        .any(|s| (*s - mean).abs() > UNIFORM_SCALE_RELATIVE_TOLERANCE * mean)
    {
        return Err(reject(
            "world transform has non-uniform scale beyond the admitted relative tolerance",
        ));
    }
    if policy == NodePoseRigidScalePolicy::RequireUnitScale
        && (mean - 1.0).abs() > UNIFORM_SCALE_RELATIVE_TOLERANCE
    {
        return Err(reject(
            "world transform scale is not one under RequireUnitScale",
        ));
    }

    Ok(voxel_convert::AdmittedRigidNodePose {
        source_node_index: node.source_node_index,
        affine_world_transform: m,
        uniform_scale: mean,
    })
}

/// Decompose a 4x4 column-major (glTF) matrix into rotation + translation by
/// dividing out the *admitted* uniform scale. Scale is never silently dropped:
/// the caller supplies the uniform scale it already admitted, and the
/// translation is divided by that scale so the rigid placement lands in the
/// part's own unit frame (a ~100x rig's bones move in centimeter-ish units;
/// dividing by the uniform scale returns them to cell units).
fn decompose_matrix_with_uniform_scale(m: [f64; 16], uniform_scale: f64) -> RigidTransform {
    let scale = if uniform_scale.is_finite() && uniform_scale > f64::EPSILON {
        uniform_scale
    } else {
        1.0
    };
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
        translation: [m[12] / scale, m[13] / scale, m[14] / scale],
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

// ---------------------------------------------------------------------------
// Rig mapping
// ---------------------------------------------------------------------------

/// Derive bind transforms that reproduce the M1 grounded neutral assembly at
/// bind pose (R6336-6), per the planner-confirmed convention:
/// `bindTransform = inverse(bone_bind_world) * neutral_part_transform`.
///
/// At bind pose `bone_pose == bone_bind_world`, so the placement
/// `bone_bind_world ∘ inverse(bone_bind_world) ∘ neutral_part_transform`
/// equals `neutral_part_transform` exactly — the part lands in the same
/// grounded canonical frame as `assemble_neutral`. Later poses apply bone
/// deltas from that baseline; we do NOT silently re-ground each pose (that
/// would introduce frame-dependent global motion/churn).
pub fn derive_bind_transform(
    bone_bind_world: RigidTransform,
    neutral_part_transform: RigidTransform,
) -> RigidTransform {
    bone_bind_world.inverse().then(neutral_part_transform)
}

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
    /// The transform from the canonical part's pivot frame into the proxy
    /// bone's bind frame. Rasterization composes `bone_pose ∘ bind_transform ∘
    /// part_local`, so the part lands spatially aligned on its bone rather than
    /// in an unrelated source frame (R6336-2).
    pub bind_transform: RigidTransform,
}

impl PartBinding {
    /// The composed transform placing this part for a given bone world pose:
    /// `bone_pose ∘ bind_transform`. Apply this to part-local coordinates so the
    /// part lands spatially aligned on its bone (R6336-2).
    pub fn placement(&self, bone_pose: RigidTransform) -> RigidTransform {
        bone_pose.then(self.bind_transform)
    }
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
            validate_rigid_transform(&binding.bind_transform, &binding.part_id)?;
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

/// A stored bind transform must be finite with a (near-)unit quaternion.
fn validate_rigid_transform(transform: &RigidTransform, part_id: &str) -> Result<(), PoseError> {
    for component in transform.translation {
        if !component.is_finite() {
            return Err(PoseError::Validation(format!(
                "part {part_id}: bind translation must be finite"
            )));
        }
    }
    // Reject non-finite rotation components explicitly: a NaN norm would
    // otherwise bypass the unit-quaternion check (NaN comparisons are false).
    if !transform.rotation.iter().all(|c| c.is_finite()) {
        return Err(PoseError::Validation(format!(
            "part {part_id}: bind rotation components must be finite"
        )));
    }
    let norm = (transform.rotation[0].powi(2)
        + transform.rotation[1].powi(2)
        + transform.rotation[2].powi(2)
        + transform.rotation[3].powi(2))
    .sqrt();
    if (norm - 1.0).abs() >= 1e-3 {
        return Err(PoseError::Validation(format!(
            "part {part_id}: bind rotation must be a unit quaternion (norm {norm})"
        )));
    }
    Ok(())
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
        // Supersample must be at least 2: at supersample 1 each source voxel
        // contributes a single sample, so a rotated thin part (e.g. a 2-cell
        // bar at ~30-45°) scatters into diagonally-touching or lost cells with
        // no intermediate samples to bridge them — topology and volume cannot
        // be preserved (R6336-12). Supersample 1 is only safe for axis-aligned,
        // non-rotated geometry, which is not this pipeline's case, so it is
        // outside the conservative contract.
        if self.supersample < 2 || self.supersample > 8 {
            return Err(PoseError::Validation(format!(
                "supersample must be within 2..=8 to preserve topology for rotated parts, got {}",
                self.supersample
            )));
        }
        // The admitted contract is *conservative and thickness-stable*, and it
        // is only honest at majority-or-better coverage (R6336-10). Requiring
        // more than half of a cell's sub-samples (threshold > 0.5) is
        // anti-conservative at low supersample: rotated geometry rarely covers
        // a supermajority of any cell, so volume collapses below any useful
        // floor. We therefore admit only thresholds up to majority coverage
        // (0.5), where the volume-floor + connectivity repairs provably keep a
        // rigid part's full volume (or a conservative dilation) and thickness,
        // and reject supermajority thresholds as outside the contract.
        if !(self.occupancy_threshold > 0.0 && self.occupancy_threshold <= 0.5) {
            return Err(PoseError::Validation(format!(
                "occupancy_threshold must be within (0, 0.5] (majority coverage) to preserve volume/thickness, got {}",
                self.occupancy_threshold
            )));
        }
        Ok(())
    }
}

/// Round to the nearest integer, halves away from zero (so ±0.5 never biases
/// toward one side), for dual-grid binning.
fn round_half_away_from_zero(value: f64) -> i64 {
    if value >= 0.0 {
        (value + 0.5).floor() as i64
    } else {
        (value - 0.5).ceil() as i64
    }
}

/// Face-neighbour offsets (6-connectivity).
const FACE_NEIGHBORS: [[i64; 3]; 6] = [
    [1, 0, 0],
    [-1, 0, 0],
    [0, 1, 0],
    [0, -1, 0],
    [0, 0, 1],
    [0, 0, -1],
];

/// Count face-connected components of a cell set (BFS). Used by the raster
/// tests; the repair loop tracks components incrementally with a union-find.
#[cfg(test)]
fn connected_components(cells: &std::collections::BTreeSet<[i64; 3]>) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    let mut components = 0;
    for &start in cells {
        if seen.contains(&start) {
            continue;
        }
        components += 1;
        let mut stack = vec![start];
        while let Some(c) = stack.pop() {
            if !seen.insert(c) {
                continue;
            }
            for d in FACE_NEIGHBORS {
                let n = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                if cells.contains(&n) && !seen.contains(&n) {
                    stack.push(n);
                }
            }
        }
    }
    components
}

/// Nearest cell to `origin` not in `occupied`, searching shells of increasing
/// max-norm distance; ties within a shell break by squared distance then
/// coordinate, so the result is deterministic. Every shell cell face-touches a
/// strictly inner shell cell, so the first free cell found always face-touches
/// the already-occupied set — displacement can never disconnect the body.
fn nearest_free_cell(origin: [i64; 3], occupied: &BTreeSet<[i64; 3]>) -> [i64; 3] {
    let mut radius = 1i64;
    loop {
        let mut shell: Vec<[i64; 3]> = Vec::new();
        for dx in -radius..=radius {
            for dy in -radius..=radius {
                for dz in -radius..=radius {
                    if dx.abs().max(dy.abs()).max(dz.abs()) != radius {
                        continue;
                    }
                    shell.push([dx, dy, dz]);
                }
            }
        }
        shell.sort_by_key(|offset| {
            (
                offset[0] * offset[0] + offset[1] * offset[1] + offset[2] * offset[2],
                *offset,
            )
        });
        for offset in shell {
            let candidate = [
                origin[0] + offset[0],
                origin[1] + offset[1],
                origin[2] + offset[2],
            ];
            if !occupied.contains(&candidate) {
                return candidate;
            }
        }
        radius += 1;
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
/// the dominant material slot among its covering source voxels. On top of that
/// vote, an injective per-voxel placement guarantees every source voxel owns
/// one distinct output cell (displacing bin collisions to the nearest free,
/// face-adjacent cell), and a connectivity repair keeps the body a single
/// face-connected component — so a rigid part keeps its full source volume (or
/// a conservative dilation) and stays hole-free and thickness-stable at every
/// admitted setting. The churn the straight per-frame re-voxelization produces
/// is avoided because the part's own cell set is fixed and only the transform
/// changes.
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
                    // Dual-grid binning: assign each sample to its nearest cell
                    // center (round half away from zero), the topology-preserving
                    // convention for rotating a voxel grid. floor() binning
                    // splits thin features (e.g. a 2-cell bar at ~45°) into
                    // diagonally-touching, face-disconnected cells (R6336-4).
                    let target = [
                        round_half_away_from_zero(world[0] - 0.5),
                        round_half_away_from_zero(world[1] - 0.5),
                        round_half_away_from_zero(world[2] - 0.5),
                    ];
                    *coverage.entry(target).or_default().entry(key).or_insert(0) += 1;
                }
            }
        }
    }

    // Emit threshold-passing cells first (the high-confidence body), then add
    // sub-threshold cells that are required to hold the part's connectivity.
    // A rigid part is one connected body; the occupancy threshold must not be
    // allowed to slice it into pieces, so we lower the bar for cells that are
    // needed to keep the body connected rather than drop them (R6336-4).
    let mut cells: Vec<RasterCell> = Vec::new();
    let mut sub_threshold: Vec<RasterCell> = Vec::new();
    for (coordinate, votes) in coverage {
        let total: u32 = votes.values().sum();
        let (source_voxel_index, material_slot) = votes
            .iter()
            .max_by(|(ka, ca), (kb, cb)| {
                ca.cmp(cb)
                    .then_with(|| kb.1.cmp(&ka.1))
                    .then_with(|| kb.0.cmp(&ka.0))
            })
            .map(|(k, _)| *k)
            .expect("non-empty votes");
        let cell = RasterCell {
            coordinate,
            material_slot,
            source_voxel_index,
        };
        if (total as f64) >= settings.occupancy_threshold * samples_per_cell {
            cells.push(cell);
        } else if total > 0 {
            sub_threshold.push(cell);
        }
    }
    cells.sort_by_key(|cell| cell.coordinate);
    sub_threshold.sort_by_key(|cell| cell.coordinate);

    // Injective per-voxel identity placement (R6336-12/R6447): every source
    // voxel is represented by at least one distinct output cell, so a rigid
    // part's full source identity inventory is preserved by construction at
    // every admitted setting — no fractional floor, and no dependence on
    // supersample density. High-confidence coverage may already represent an
    // identity; otherwise that voxel claims the target cell its transformed
    // center bins to, in deterministic source-coordinate order.
    // When two distinct voxels bin to the same target cell (e.g. a thin bar
    // rotated onto a cell diagonal so both centers quantize together), the
    // later voxel is displaced to the nearest unclaimed cell; the shell search
    // guarantees the displaced cell face-touches the already-occupied set, so
    // displacement never disconnects the body. This replaces the old
    // ceil(source_volume/2) floor, which still accepted losing half a part —
    // and accepted a two-voxel bar collapsing to one cell outright.
    let mut occupied: BTreeSet<[i64; 3]> = cells.iter().map(|cell| cell.coordinate).collect();
    let mut represented: BTreeSet<u32> = cells.iter().map(|cell| cell.source_voxel_index).collect();
    let mut voxel_order: Vec<usize> = (0..part.cells.len()).collect();
    voxel_order.sort_by_key(|&index| part.cells[index].coordinate);
    let mut placed: Vec<RasterCell> = Vec::new();
    for index in voxel_order {
        let source_voxel_index = u32::try_from(index).expect("validated part voxel count fits u32");
        if represented.contains(&source_voxel_index) {
            continue;
        }
        let voxel = &part.cells[index];
        let center = [
            voxel.coordinate[0] as f64 + 0.5,
            voxel.coordinate[1] as f64 + 0.5,
            voxel.coordinate[2] as f64 + 0.5,
        ];
        let world = transform.apply(center);
        let primary = [
            round_half_away_from_zero(world[0] - 0.5),
            round_half_away_from_zero(world[1] - 0.5),
            round_half_away_from_zero(world[2] - 0.5),
        ];
        let target = if !occupied.contains(&primary) {
            primary
        } else {
            nearest_free_cell(primary, &occupied)
        };
        let inserted = occupied.insert(target);
        debug_assert!(inserted, "nearest-free placement must be unoccupied");
        represented.insert(source_voxel_index);
        placed.push(RasterCell {
            coordinate: target,
            material_slot: voxel.material_slot,
            source_voxel_index,
        });
    }
    if !placed.is_empty() {
        cells.extend(placed);
        cells.sort_by_key(|cell| cell.coordinate);
        // Placed cells may coincide with sub-threshold candidates; those cells
        // are occupied now and must not be re-added by connectivity repair.
        sub_threshold.retain(|cell| !occupied.contains(&cell.coordinate));
    }

    // Add sub-threshold cells one at a time (most-covered first, deterministic
    // by coordinate) until the part is a single connected component or no
    // candidates remain. Because every candidate face-touches existing cells
    // (it was binned from a neighbouring source cube), this converges to one
    // component for a rigid part.
    //
    // Scaling note: at kit scale (100k+ cells) the original stop condition — a
    // full set rebuild plus a full BFS per added cell — and the per-iteration
    // full-candidate rescan are quadratic. This loop keeps the *identical* pick
    // sequence (and so bit-identical output) while making both incremental: one
    // BFS labeling seeds a union-find, and a (touches, coordinate) ordered set
    // replaces the rescan, updated only for candidates adjacent to each added
    // cell. The pick rule is unchanged: most face-touches with existing cells,
    // tie-broken by lowest coordinate.
    fn find(
        parent: &mut std::collections::BTreeMap<[i64; 3], [i64; 3]>,
        cell: [i64; 3],
    ) -> [i64; 3] {
        let mut root = cell;
        while parent[&root] != root {
            root = parent[&root];
        }
        let mut step = cell;
        while parent[&step] != step {
            let next = parent[&step];
            parent.insert(step, root);
            step = next;
        }
        root
    }
    let mut candidates: BTreeMap<[i64; 3], RasterCell> = sub_threshold
        .into_iter()
        .map(|c| (c.coordinate, c))
        .collect();
    let mut live_set: std::collections::BTreeSet<[i64; 3]> =
        cells.iter().map(|cell| cell.coordinate).collect();
    // One BFS labeling pass over the initial cells: `parent` maps every cell to
    // its component root (the BFS start cell), giving both the union-find's
    // initial state and the component count.
    let mut parent: std::collections::BTreeMap<[i64; 3], [i64; 3]> =
        std::collections::BTreeMap::new();
    let mut components = 0usize;
    for start in live_set.iter().copied().collect::<Vec<_>>() {
        if parent.contains_key(&start) {
            continue;
        }
        components += 1;
        parent.insert(start, start);
        let mut stack = vec![start];
        while let Some(c) = stack.pop() {
            for d in FACE_NEIGHBORS {
                let n = [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
                if live_set.contains(&n) && !parent.contains_key(&n) {
                    parent.insert(n, start);
                    stack.push(n);
                }
            }
        }
    }
    // (touches, coordinate) ordered candidates; only candidates adjacent to an
    // added cell ever change score, so only those are updated per iteration.
    // The set orders ascending, so `next_back` is the highest-touch candidate,
    // and among equal touches the *highest* coordinate — the final pick below
    // applies the lowest-coordinate tie-break explicitly, so ties are resolved
    // by scanning only the tied front.
    let mut touches: std::collections::BTreeMap<[i64; 3], usize> = candidates
        .keys()
        .map(|coordinate| {
            let count = FACE_NEIGHBORS
                .iter()
                .filter(|d| {
                    live_set.contains(&[
                        coordinate[0] + d[0],
                        coordinate[1] + d[1],
                        coordinate[2] + d[2],
                    ])
                })
                .count();
            (*coordinate, count)
        })
        .collect();
    let mut heap: std::collections::BTreeSet<(usize, [i64; 3])> = touches
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(coordinate, count)| (*count, *coordinate))
        .collect();
    for _ in 0..4096 {
        if live_set.is_empty() {
            // Nothing passed the threshold at all; seed with the best candidate.
            if let Some((_, cell)) = candidates.iter().next().map(|(k, v)| (*k, *v)) {
                candidates.remove(&cell.coordinate);
                cells.push(cell);
                parent.insert(cell.coordinate, cell.coordinate);
                live_set.insert(cell.coordinate);
                touches.remove(&cell.coordinate);
                components = 1;
                for d in FACE_NEIGHBORS {
                    let neighbor = [
                        cell.coordinate[0] + d[0],
                        cell.coordinate[1] + d[1],
                        cell.coordinate[2] + d[2],
                    ];
                    if candidates.contains_key(&neighbor) {
                        let old = touches[&neighbor];
                        heap.remove(&(old, neighbor));
                        touches.insert(neighbor, old + 1);
                        heap.insert((old + 1, neighbor));
                    }
                }
            } else {
                break;
            }
            continue;
        }
        if components <= 1 {
            break;
        }
        // The best candidate is at the back of the heap; tied lowest coordinate
        // wins, so walk the tied back-to-front run and pick the smallest.
        let best = heap.iter().next_back().and_then(|(top_count, _)| {
            heap.iter()
                .rev()
                .take_while(|(count, _)| count == top_count)
                .map(|(_, coordinate)| *coordinate)
                .min()
        });
        match best {
            Some(coordinate) => {
                let cell = candidates[&coordinate];
                candidates.remove(&coordinate);
                let score = touches.remove(&coordinate).expect("touched candidate");
                heap.remove(&(score, coordinate));
                cells.push(cell);
                cells.sort_by_key(|c| c.coordinate);
                // The new cell starts as its own component (+1), then merges
                // with each distinct component it face-touches (-1 each).
                parent.insert(cell.coordinate, cell.coordinate);
                components += 1;
                let mut merged_root = cell.coordinate;
                for d in FACE_NEIGHBORS {
                    let neighbor = [
                        cell.coordinate[0] + d[0],
                        cell.coordinate[1] + d[1],
                        cell.coordinate[2] + d[2],
                    ];
                    if live_set.contains(&neighbor) {
                        let root_neighbor = find(&mut parent, neighbor);
                        let root_merged = find(&mut parent, merged_root);
                        if root_neighbor != root_merged {
                            parent.insert(root_merged, root_neighbor);
                            merged_root = root_neighbor;
                            components -= 1;
                        }
                    }
                }
                live_set.insert(cell.coordinate);
                // Update scores only for candidates adjacent to the added cell.
                for d in FACE_NEIGHBORS {
                    let neighbor = [
                        cell.coordinate[0] + d[0],
                        cell.coordinate[1] + d[1],
                        cell.coordinate[2] + d[2],
                    ];
                    if candidates.contains_key(&neighbor) {
                        let old = touches[&neighbor];
                        heap.remove(&(old, neighbor));
                        touches.insert(neighbor, old + 1);
                        heap.insert((old + 1, neighbor));
                    }
                }
            }
            None => break,
        }
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
    /// The Engine `evaluate_clip_node_poses` seam returned a typed error
    /// (missing clip, out-of-range time, non-finite value, hierarchy cycle).
    EngineEvaluation(String),
    /// A node's affine world transform failed rigid admission under the
    /// selected scale policy.
    NonRigidPose {
        node: u32,
        reason: String,
    },
}

impl fmt::Display for PoseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PoseError::Validation(m) => write!(f, "pose validation: {m}"),
            PoseError::UnknownClip(i) => write!(f, "unknown clip index {i}"),
            PoseError::EngineEvaluation(m) => write!(f, "engine pose evaluation: {m}"),
            PoseError::NonRigidPose { node, reason } => {
                write!(f, "node {node} failed rigid admission: {reason}")
            }
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

    fn two_cell_bar() -> KitPart {
        let mut cells = vec![
            crate::kit::KitCell {
                coordinate: [0, 0, 0],
                material_slot: 1,
            },
            crate::kit::KitCell {
                coordinate: [1, 0, 0],
                material_slot: 1,
            },
        ];
        cells.sort_by_key(|c| c.coordinate);
        KitPart {
            id: "bar".to_owned(),
            version: 1,
            pivot: [0, 0, 0],
            limb: false,
            cells,
            sockets: vec![],
            palette_groups: vec![],
            deformation_budget: crate::kit::DeformationBudget {
                max_length_change: 0.0,
                max_volume_change: 0.0,
                allow_joint_compression: false,
            },
            protected_regions: vec![],
            symmetry_partner: None,
        }
    }

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
    fn cavity_preservation_is_a_documented_limitation_not_an_invariant() {
        // R6336-10: conservative rasterization keeps volume and connectivity,
        // but conservative dilation can fill a small interior cavity when a
        // hollow shell is rotated. This is a documented limitation of the
        // conservative approach, not a guaranteed invariant — the contract
        // guarantees volume/connectivity/thickness, not small-cavity survival.
        let mut cells = Vec::new();
        for x in 0..5i64 {
            for y in 0..5i64 {
                for z in 0..5i64 {
                    if x == 0 || x == 4 || y == 0 || y == 4 || z == 0 || z == 4 {
                        cells.push(crate::kit::KitCell {
                            coordinate: [x, y, z],
                            material_slot: 1,
                        });
                    }
                }
            }
        }
        cells.sort_by_key(|c| c.coordinate);
        let part = crate::kit::KitPart {
            id: "shell".to_owned(),
            version: 1,
            pivot: [0, 0, 0],
            limb: false,
            cells,
            sockets: vec![],
            palette_groups: vec![],
            deformation_budget: crate::kit::DeformationBudget {
                max_length_change: 0.0,
                max_volume_change: 0.0,
                allow_joint_compression: false,
            },
            protected_regions: vec![],
            symmetry_partner: None,
        };
        let source_volume = part.cells.len();
        let angle = std::f64::consts::PI / 9.0;
        let transform = RigidTransform {
            rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
            translation: [0.0, 0.0, 0.0],
        };
        let out =
            rasterize_part(&part, transform, &RasterSettings::default()).expect("shell rasterize");
        // The contract DOES hold on volume: the shell never loses material.
        assert!(
            out.len() >= source_volume,
            "the shell keeps at least its full source volume, got {} of {}",
            out.len(),
            source_volume
        );
        let set: std::collections::BTreeSet<[i64; 3]> = out.iter().map(|c| c.coordinate).collect();
        assert_eq!(
            connected_components(&set),
            1,
            "the shell stays one connected body"
        );
        // Whether the 3x3x3 interior cavity survives is NOT asserted: it is the
        // documented small-cavity limitation, recorded here as the honest edge.
    }

    #[test]
    fn supersample_one_is_outside_the_conservative_contract() {
        // R6336-12: at supersample 1 each source voxel contributes a single
        // sample, so a rotated thin part (a 2-cell bar at ~30-45°) scatters
        // into diagonally-touching or lost cells with no intermediate samples
        // to bridge them — topology and volume cannot be preserved. It is only
        // safe for axis-aligned, non-rotated geometry (not this pipeline), so
        // it is rejected as outside the contract.
        let part = two_cell_bar();
        let angle = std::f64::consts::PI / 4.0;
        let transform = RigidTransform {
            rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
            translation: [0.0, 0.0, 0.0],
        };
        let settings = RasterSettings {
            supersample: 1,
            occupancy_threshold: 0.5,
        };
        assert!(
            rasterize_part(&part, transform, &settings).is_err(),
            "supersample 1 must be rejected as outside the conservative contract"
        );

        // At the admitted supersample the same bar stays both present and
        // face-connected across the 30-45° range that broke at supersample 1.
        let good = RasterSettings {
            supersample: 2,
            occupancy_threshold: 0.5,
        };
        for deg in [30.0_f64, 45.0, 45.25] {
            let angle = deg * std::f64::consts::PI / 180.0;
            let t = RigidTransform {
                rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
                translation: [0.0, 0.0, 0.0],
            };
            let out = rasterize_part(&part, t, &good).expect("bar rasterize");
            assert!(
                out.len() >= 2,
                "the bar must not collapse below its source volume at {deg}°"
            );
            let set: std::collections::BTreeSet<[i64; 3]> =
                out.iter().map(|c| c.coordinate).collect();
            assert_eq!(
                connected_components(&set),
                1,
                "the bar must stay one connected body at {deg}°"
            );
        }
    }

    #[test]
    fn diagonal_bin_collision_preserves_every_source_voxel() {
        // The exact R6336-12 probe: a valid rigid transform mapping the source
        // X axis onto the normalized [1,1,1] diagonal, translated so BOTH bar
        // voxel centers quantize to target cell [0,0,0] (bin offsets -0.3 and
        // +0.277, the reviewer's values). At supersample 1 this collapsed two
        // voxels into one output cell with every observed target passing the
        // threshold, so no repair candidate existed; supersample 1 is now
        // outside the contract. At admitted settings the injective per-voxel
        // placement must still give each voxel its own distinct output cell.
        let part = two_cell_bar();
        let axis = [0.0, -1.0 / 2.0_f64.sqrt(), 1.0 / 2.0_f64.sqrt()];
        let theta = (1.0 / 3.0_f64.sqrt()).acos();
        let (s, c) = (theta / 2.0).sin_cos();
        let rotation = [axis[0] * s, axis[1] * s, axis[2] * s, c];
        // Choose the translation so voxel A's center lands at (0.2, 0.2, 0.2):
        // bin offset -0.3 per axis, exactly the reviewer's collision.
        let unshifted = RigidTransform {
            rotation,
            translation: [0.0, 0.0, 0.0],
        };
        let applied = unshifted.apply([0.5, 0.5, 0.5]);
        let translation = [0.2 - applied[0], 0.2 - applied[1], 0.2 - applied[2]];
        let transform = RigidTransform {
            rotation,
            translation,
        };
        // Premise check: both distinct source voxel centers bin to [0, 0, 0].
        for center in [[0.5, 0.5, 0.5], [1.5, 0.5, 0.5]] {
            let world = transform.apply(center);
            let bin = [
                round_half_away_from_zero(world[0] - 0.5),
                round_half_away_from_zero(world[1] - 0.5),
                round_half_away_from_zero(world[2] - 0.5),
            ];
            assert_eq!(bin, [0, 0, 0], "the probe must collide both voxel centers");
        }

        for settings in [
            RasterSettings {
                supersample: 2,
                occupancy_threshold: 0.5,
            },
            RasterSettings {
                supersample: 2,
                occupancy_threshold: 0.01,
            },
            RasterSettings {
                supersample: 8,
                occupancy_threshold: 0.5,
            },
        ] {
            let out = rasterize_part(&part, transform, &settings).expect("bar rasterize");
            assert!(
                out.len() >= part.cells.len(),
                "every source voxel must be preserved under the collision probe at {settings:?}: got {} of {}",
                out.len(),
                part.cells.len()
            );
            let set: std::collections::BTreeSet<[i64; 3]> =
                out.iter().map(|cell| cell.coordinate).collect();
            assert_eq!(
                set.len(),
                out.len(),
                "output cells must be distinct at {settings:?}"
            );
            assert_eq!(
                connected_components(&set),
                1,
                "the bar must stay one connected body at {settings:?}"
            );
        }
    }

    #[test]
    fn admitted_boundary_settings_hold_volume_and_connectivity() {
        // R6336-12 boundary regression: across the full admitted settings
        // range (lowest/highest supersample, lowest/highest threshold), thin
        // bars and solid blocks under hostile rotations keep full source
        // volume (or a conservative dilation), distinct cells, and one
        // connected component.
        let boundaries = [
            RasterSettings {
                supersample: 2,
                occupancy_threshold: 0.5,
            },
            RasterSettings {
                supersample: 2,
                occupancy_threshold: 0.01,
            },
            RasterSettings {
                supersample: 8,
                occupancy_threshold: 0.5,
            },
            RasterSettings {
                supersample: 8,
                occupancy_threshold: 0.01,
            },
        ];
        let bar = two_cell_bar();
        let solid = solid_part(3, 1);
        let mut transforms: Vec<RigidTransform> = Vec::new();
        // Z-rotation sweep across the range that thins and scatters bars.
        for deg in 0..=90 {
            let angle = (deg as f64) * std::f64::consts::PI / 180.0;
            transforms.push(RigidTransform {
                rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
                translation: [0.0, 0.0, 0.0],
            });
        }
        // Diagonal-axis rotations (the collision class) at several tilts.
        let axis = [0.0, -1.0 / 2.0_f64.sqrt(), 1.0 / 2.0_f64.sqrt()];
        for deg in [30.0_f64, 45.0, 54.7356, 70.0] {
            let theta = deg * std::f64::consts::PI / 180.0;
            let (s, c) = (theta / 2.0).sin_cos();
            transforms.push(RigidTransform {
                rotation: [axis[0] * s, axis[1] * s, axis[2] * s, c],
                translation: [0.13, -0.27, 0.41],
            });
        }
        for (name, part) in [("bar", &bar), ("solid", &solid)] {
            for settings in &boundaries {
                for transform in &transforms {
                    let out = rasterize_part(part, *transform, settings).expect("rasterize");
                    assert!(
                        out.len() >= part.cells.len(),
                        "{name} must keep full source volume at {settings:?}: got {} of {}",
                        out.len(),
                        part.cells.len()
                    );
                    let set: std::collections::BTreeSet<[i64; 3]> =
                        out.iter().map(|cell| cell.coordinate).collect();
                    assert_eq!(
                        set.len(),
                        out.len(),
                        "{name} cells distinct at {settings:?}"
                    );
                    let represented = out
                        .iter()
                        .map(|cell| cell.source_voxel_index)
                        .collect::<BTreeSet<_>>();
                    assert_eq!(
                        represented.len(),
                        part.cells.len(),
                        "{name} must retain every source identity at {settings:?} and {transform:?}"
                    );
                    assert_eq!(
                        connected_components(&set),
                        1,
                        "{name} must stay one connected body at {settings:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn supermajority_threshold_is_outside_the_conservative_contract() {
        // R6336-10: a threshold above majority coverage (0.5) is
        // anti-conservative at low supersample — it collapses volume below any
        // useful floor — so it is rejected as outside the contract rather than
        // silently repaired into a thin part.
        let part = solid_part(3, 1);
        let transform = RigidTransform::IDENTITY;
        let bad = RasterSettings {
            supersample: 2,
            occupancy_threshold: 1.0,
        };
        assert!(
            rasterize_part(&part, transform, &bad).is_err(),
            "a supermajority threshold must be rejected as outside the conservative contract"
        );

        // At the admitted majority boundary the same solid keeps its full
        // volume (conservative dilation allowed, never a loss) and stays
        // connected and thick.
        let angle = std::f64::consts::PI / 6.0;
        let rotated = RigidTransform {
            rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
            translation: [0.0, 0.0, 0.0],
        };
        let good = RasterSettings {
            supersample: 2,
            occupancy_threshold: 0.5,
        };
        let cells = rasterize_part(&part, rotated, &good).expect("majority-threshold rasterize");
        assert!(
            cells.len() >= part.cells.len(),
            "majority coverage must keep at least the full source volume (dilation allowed), got {} of {}",
            cells.len(),
            part.cells.len()
        );
        let set: std::collections::BTreeSet<[i64; 3]> =
            cells.iter().map(|c| c.coordinate).collect();
        assert_eq!(
            connected_components(&set),
            1,
            "the solid stays one connected body"
        );
    }

    #[test]
    fn two_cell_bar_stays_connected_under_rotation() {
        // Reviewer's exact probe: a face-connected 2-cell bar rotated 45.25°.
        let mut cells = vec![
            crate::kit::KitCell {
                coordinate: [0, 0, 0],
                material_slot: 1,
            },
            crate::kit::KitCell {
                coordinate: [1, 0, 0],
                material_slot: 1,
            },
        ];
        cells.sort_by_key(|c| c.coordinate);
        let part = crate::kit::KitPart {
            id: "bar".to_owned(),
            version: 1,
            pivot: [0, 0, 0],
            limb: false,
            cells,
            sockets: vec![],
            palette_groups: vec![],
            deformation_budget: crate::kit::DeformationBudget {
                max_length_change: 0.0,
                max_volume_change: 0.0,
                allow_joint_compression: false,
            },
            protected_regions: vec![],
            symmetry_partner: None,
        };
        let angle = 45.25f64.to_radians();
        let transform = RigidTransform {
            rotation: [0.0, 0.0, (angle / 2.0).sin(), (angle / 2.0).cos()],
            translation: [0.0, 0.0, 0.0],
        };
        let out = rasterize_part(&part, transform, &RasterSettings::default()).expect("rasterize");
        let coords: std::collections::BTreeSet<_> = out.iter().map(|c| c.coordinate).collect();
        // A rigid 2-cell bar must remain a single face-connected component
        // after rotation (R6336-4): no diagonally-touching split.
        let components = connected_components(&coords);
        assert_eq!(
            components, 1,
            "rotated 2-cell bar must stay face-connected, got {components} components: {coords:?}"
        );
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
}
