//! Deterministic joint fusion and structural cleanup for exploded-kit frames.
//!
//! This module turns an M2 [`RoughFrame`] into a reviewable deterministic base.
//! It is deliberately an authoring convenience, not runtime truth: later
//! bounded edits may refine the result before M4 compiles immutable frames.
//! Every synthetic cell records the operation that created it, while canonical
//! cells retain their part/voxel identity.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use voxel_convert::ImportedAnimatedModel;

use crate::assemble::{
    assemble_rough_frame, socket_constrained_part_placements, DiscardedCanonicalVoxel, RoughFrame,
    SelectedPose,
};
use crate::kit::{VoxelKit, MAX_COORDINATE_ABS};
use crate::pose::{RasterSettings, RigMap, RigidTransform};

const FACE_NEIGHBORS: [[i64; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
];

/// Configurable deterministic cleanup policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FusionSettings {
    pub fill_one_voxel_cavities: bool,
    pub remove_isolated_generated_voxels: bool,
    pub bridge_one_voxel_gaps: bool,
    pub enforce_minimum_limb_thickness: bool,
    pub restore_ground_contact: bool,
    pub normalize_weapon_dimensions: bool,
    /// Maximum Manhattan distance admitted for one socket bridge.
    pub max_socket_bridge_length: u32,
    /// Aggregate generated-cell quota, checked before publication.
    pub max_generated_voxels: usize,
}

impl Default for FusionSettings {
    fn default() -> Self {
        Self {
            fill_one_voxel_cavities: true,
            remove_isolated_generated_voxels: true,
            bridge_one_voxel_gaps: true,
            enforce_minimum_limb_thickness: true,
            restore_ground_contact: true,
            normalize_weapon_dimensions: true,
            max_socket_bridge_length: 64,
            max_generated_voxels: 8_192,
        }
    }
}

impl FusionSettings {
    fn validate(self) -> Result<(), FusionError> {
        if self.max_socket_bridge_length == 0 || self.max_socket_bridge_length > 256 {
            return Err(FusionError::new(
                "fusion.invalidSettings",
                format!(
                    "maxSocketBridgeLength must be within 1..=256, got {}",
                    self.max_socket_bridge_length
                ),
            ));
        }
        if self.max_generated_voxels == 0 || self.max_generated_voxels > 1_000_000 {
            return Err(FusionError::new(
                "fusion.invalidSettings",
                format!(
                    "maxGeneratedVoxels must be within 1..=1000000, got {}",
                    self.max_generated_voxels
                ),
            ));
        }
        Ok(())
    }
}

/// Named cleanup operations suitable for diagnostics and later edit bundles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanupOperation {
    FillOneVoxelCavity,
    RemoveIsolatedGeneratedVoxel,
    BridgeOneVoxelGap,
    EnforceMinimumLimbThickness,
    TrimDeepInterpenetration,
    RepairSocketNeighborhood,
    RestoreGroundContact,
    NormalizeWeaponThickness,
}

/// Provenance for a synthetic cell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum GeneratedOperation {
    JointBridge { joint_id: String },
    Cleanup { operation: CleanupOperation },
}

/// Closed provenance for every fused cell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum FusedVoxelOrigin {
    Canonical {
        part_id: u32,
        source_voxel_index: u32,
    },
    Generated {
        operation: GeneratedOperation,
    },
    AuthoredEdit {
        operation_index: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FusedVoxelCell {
    pub coordinate: [i64; 3],
    pub material_slot: u16,
    pub origin: FusedVoxelOrigin,
    /// Cleanup operations that modified this occupied cell after its origin
    /// was established. A global ground correction, for example, remains
    /// canonical provenance plus `RestoreGroundContact`.
    pub operations: Vec<CleanupOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscardedVoxelOrigin {
    pub coordinate: [i64; 3],
    pub part_id: u32,
    pub source_voxel_index: u32,
    pub material_slot: u16,
    pub winner_part_id: u32,
    pub winner_source_voxel_index: u32,
}

impl From<DiscardedCanonicalVoxel> for DiscardedVoxelOrigin {
    fn from(value: DiscardedCanonicalVoxel) -> Self {
        Self {
            coordinate: value.coordinate,
            part_id: value.part_id,
            source_voxel_index: value.source_voxel_index,
            material_slot: value.material_slot,
            winner_part_id: value.winner_part_id,
            winner_source_voxel_index: value.winner_source_voxel_index,
        }
    }
}

/// Structurally valid first-pass frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FusedFrame {
    pub time_microseconds: u64,
    pub duration_microseconds: u64,
    pub voxels: Vec<FusedVoxelCell>,
    pub discarded_origins: Vec<DiscardedVoxelOrigin>,
    pub generated_voxels: usize,
    /// Ordered pass ledger, including no-op validation/enforcement passes.
    pub applied_operations: Vec<CleanupOperation>,
}

impl FusedFrame {
    #[must_use]
    pub fn bounds(&self) -> Option<([i64; 3], [i64; 3])> {
        bounds(self.voxels.iter().map(|cell| cell.coordinate))
    }

    #[must_use]
    pub fn generated_coordinates(&self) -> BTreeSet<[i64; 3]> {
        self.voxels
            .iter()
            .filter_map(|cell| {
                matches!(cell.origin, FusedVoxelOrigin::Generated { .. }).then_some(cell.coordinate)
            })
            .collect()
    }
}

/// Typed, machine-readable fusion or structural-admission failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FusionError {
    code: &'static str,
    message: String,
}

impl FusionError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for FusionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FusionError {}

/// Immutable source authority required to validate and fuse rough frames.
#[derive(Debug, Clone, Copy)]
pub struct FusionContext<'a> {
    pub kit: &'a VoxelKit,
    pub rig_map: &'a RigMap,
    pub model: &'a ImportedAnimatedModel,
    pub clip_index: usize,
    pub raster_settings: &'a RasterSettings,
}

/// Fuse and validate one rough pose.
///
/// # Errors
///
/// Returns a typed [`FusionError`] when settings, pose evaluation, socket
/// bridging, quotas, or structural hard gates cannot be satisfied.
pub fn fuse_rough_frame(
    context: FusionContext<'_>,
    selected: &SelectedPose,
    rough: &RoughFrame,
    settings: FusionSettings,
) -> Result<FusedFrame, FusionError> {
    settings.validate()?;
    context
        .kit
        .validate()
        .map_err(|error| FusionError::new("fusion.invalidKit", error.to_string()))?;
    context
        .rig_map
        .validate(context.kit, context.model)
        .map_err(|error| FusionError::new("fusion.invalidRigMap", error.to_string()))?;
    if rough.time_microseconds != selected.time_microseconds
        || rough.duration_microseconds != selected.duration_microseconds
    {
        return Err(FusionError::new(
            "fusion.poseMismatch",
            "rough frame time/duration does not match the selected pose",
        ));
    }
    let expected_rough = assemble_rough_frame(
        context.kit,
        context.rig_map,
        context.model,
        context.clip_index,
        selected,
        context.raster_settings,
    )
    .map_err(|error| FusionError::new("fusion.roughFrameValidation", error.to_string()))?;
    if rough.discarded_overlaps != expected_rough.discarded_overlaps {
        return Err(FusionError::new(
            "fusion.overlapLedgerMismatch",
            "discarded-overlap diagnostics do not match authoritative rasterization",
        ));
    }
    let placements_by_id = socket_constrained_part_placements(
        context.kit,
        context.rig_map,
        context.model,
        context.clip_index,
        selected.time_microseconds,
    )
    .map_err(|error| FusionError::new("fusion.poseEvaluation", error.to_string()))?;
    let placements: Vec<RigidTransform> = context
        .kit
        .parts
        .iter()
        .map(|part| placements_by_id[&part.id])
        .collect();

    let mut cells = canonical_cells(context.kit, rough)?;
    let maximum_total_cells = cells
        .len()
        .checked_add(settings.max_generated_voxels)
        .ok_or_else(|| {
            FusionError::new(
                "fusion.generatedVoxelQuotaExceeded",
                "canonical volume plus generated voxel quota overflowed usize",
            )
        })?;
    let seam_coordinates: BTreeSet<[i64; 3]> = rough
        .voxels
        .iter()
        .filter(|cell| cell.needs_fusion)
        .map(|cell| cell.coordinate)
        .collect();

    for joint in socket_joints(context.kit, &placements)? {
        add_socket_bridge(
            &mut cells,
            &joint,
            settings.max_socket_bridge_length,
            maximum_total_cells,
        )?;
    }

    if settings.bridge_one_voxel_gaps {
        bridge_one_voxel_gaps(&mut cells, &seam_coordinates, maximum_total_cells)?;
    }
    if settings.fill_one_voxel_cavities {
        fill_one_voxel_cavities(&mut cells, &seam_coordinates, maximum_total_cells)?;
    }
    if settings.enforce_minimum_limb_thickness {
        enforce_minimum_limb_thickness(context.kit, rough, &mut cells, maximum_total_cells)?;
    }
    if settings.restore_ground_contact {
        restore_ground_contact(context.kit, &mut cells, maximum_total_cells)?;
    }
    if settings.remove_isolated_generated_voxels {
        remove_isolated_generated_voxels(&mut cells);
    }

    let generated_voxels = cells
        .values()
        .filter(|cell| matches!(cell.origin, FusedVoxelOrigin::Generated { .. }))
        .count();
    if generated_voxels > settings.max_generated_voxels {
        return Err(FusionError::new(
            "fusion.generatedVoxelQuotaExceeded",
            format!(
                "generated {generated_voxels} voxels, limit is {}",
                settings.max_generated_voxels
            ),
        ));
    }

    let mut frame = FusedFrame {
        time_microseconds: rough.time_microseconds,
        duration_microseconds: rough.duration_microseconds,
        voxels: cells.into_values().collect(),
        discarded_origins: rough
            .discarded_overlaps
            .iter()
            .copied()
            .map(DiscardedVoxelOrigin::from)
            .collect(),
        generated_voxels,
        applied_operations: applied_operations(settings, !rough.discarded_overlaps.is_empty()),
    };
    frame.voxels.sort_by_key(|cell| cell.coordinate);
    frame.discarded_origins.sort_by_key(|discarded| {
        (
            discarded.coordinate,
            discarded.part_id,
            discarded.source_voxel_index,
        )
    });

    validate_structural_frame(context.kit, &placements, rough, &frame, settings)?;
    Ok(frame)
}

fn applied_operations(
    settings: FusionSettings,
    resolved_interpenetration: bool,
) -> Vec<CleanupOperation> {
    let mut operations = vec![CleanupOperation::RepairSocketNeighborhood];
    if resolved_interpenetration {
        operations.push(CleanupOperation::TrimDeepInterpenetration);
    }
    if settings.bridge_one_voxel_gaps {
        operations.push(CleanupOperation::BridgeOneVoxelGap);
    }
    if settings.fill_one_voxel_cavities {
        operations.push(CleanupOperation::FillOneVoxelCavity);
    }
    if settings.enforce_minimum_limb_thickness {
        operations.push(CleanupOperation::EnforceMinimumLimbThickness);
    }
    if settings.restore_ground_contact {
        operations.push(CleanupOperation::RestoreGroundContact);
    }
    if settings.remove_isolated_generated_voxels {
        operations.push(CleanupOperation::RemoveIsolatedGeneratedVoxel);
    }
    if settings.normalize_weapon_dimensions {
        operations.push(CleanupOperation::NormalizeWeaponThickness);
    }
    operations
}

/// Fuse a complete selected schedule.
///
/// # Errors
///
/// Returns the first typed fusion or structural failure without publishing a
/// partial schedule.
pub fn fuse_rough_schedule(
    context: FusionContext<'_>,
    selected: &[SelectedPose],
    rough: &[RoughFrame],
    settings: FusionSettings,
) -> Result<Vec<FusedFrame>, FusionError> {
    if selected.len() != rough.len() {
        return Err(FusionError::new(
            "fusion.scheduleLengthMismatch",
            format!(
                "selected schedule has {} poses but rough schedule has {} frames",
                selected.len(),
                rough.len()
            ),
        ));
    }
    selected
        .iter()
        .zip(rough)
        .map(|(pose, frame)| fuse_rough_frame(context, pose, frame, settings))
        .collect()
}

fn canonical_cells(
    kit: &VoxelKit,
    rough: &RoughFrame,
) -> Result<BTreeMap<[i64; 3], FusedVoxelCell>, FusionError> {
    let mut cells = BTreeMap::new();
    for cell in &rough.voxels {
        let part = kit.parts.get(cell.part_id as usize).ok_or_else(|| {
            FusionError::new(
                "fusion.invalidProvenance",
                format!("unknown part index {}", cell.part_id),
            )
        })?;
        let source = part
            .cells
            .get(cell.source_voxel_index as usize)
            .ok_or_else(|| {
                FusionError::new(
                    "fusion.invalidProvenance",
                    format!(
                        "part {} has no source voxel {}",
                        part.id, cell.source_voxel_index
                    ),
                )
            })?;
        if source.material_slot != cell.material_slot {
            return Err(FusionError::new(
                "fusion.invalidProvenance",
                format!(
                    "part {} voxel {} material {} does not match rough material {}",
                    part.id, cell.source_voxel_index, source.material_slot, cell.material_slot
                ),
            ));
        }
        let fused = FusedVoxelCell {
            coordinate: cell.coordinate,
            material_slot: cell.material_slot,
            origin: FusedVoxelOrigin::Canonical {
                part_id: cell.part_id,
                source_voxel_index: cell.source_voxel_index,
            },
            operations: Vec::new(),
        };
        if cells.insert(cell.coordinate, fused).is_some() {
            return Err(FusionError::new(
                "fusion.duplicateCoordinate",
                format!("rough frame repeats coordinate {:?}", cell.coordinate),
            ));
        }
    }
    if cells.is_empty() {
        return Err(FusionError::new(
            "fusion.missingRequiredRegion",
            "rough frame contains no canonical geometry",
        ));
    }
    Ok(cells)
}

struct SocketJoint {
    id: String,
    child_part: u32,
    parent_part: u32,
    child_center: [f64; 3],
    parent_center: [f64; 3],
    radius: f64,
}

fn socket_joints(
    kit: &VoxelKit,
    placements: &[RigidTransform],
) -> Result<Vec<SocketJoint>, FusionError> {
    let part_index: BTreeMap<&str, usize> = kit
        .parts
        .iter()
        .enumerate()
        .map(|(index, part)| (part.id.as_str(), index))
        .collect();
    let mut joints = Vec::new();
    for (child_index, child) in kit.parts.iter().enumerate() {
        for socket in &child.sockets {
            let Some(mate) = socket.mate.as_deref() else {
                continue;
            };
            let (parent_id, parent_socket_id) = mate.split_once('.').ok_or_else(|| {
                FusionError::new(
                    "fusion.invalidSocketMate",
                    format!("socket mate {mate:?} is not <part>.<socket>"),
                )
            })?;
            let parent_index = *part_index.get(parent_id).ok_or_else(|| {
                FusionError::new(
                    "fusion.invalidSocketMate",
                    format!("socket mate part {parent_id} is absent"),
                )
            })?;
            let parent_socket = kit.parts[parent_index]
                .socket(parent_socket_id)
                .ok_or_else(|| {
                    FusionError::new(
                        "fusion.invalidSocketMate",
                        format!("socket mate {mate} is absent"),
                    )
                })?;
            joints.push(SocketJoint {
                id: format!("{}.{}<->{}", child.id, socket.id, mate),
                child_part: child_index as u32,
                parent_part: parent_index as u32,
                child_center: placements[child_index].apply(socket.position),
                parent_center: placements[parent_index].apply(parent_socket.position),
                radius: socket.radius.min(parent_socket.radius).max(1.0),
            });
        }
    }
    joints.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(joints)
}

fn add_socket_bridge(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    joint: &SocketJoint,
    max_length: u32,
    maximum_total_cells: usize,
) -> Result<(), FusionError> {
    let child =
        nearest_part_cell(cells, joint.child_part, joint.child_center).ok_or_else(|| {
            FusionError::new(
                "fusion.missingRequiredRegion",
                format!("joint {} child part has no canonical cells", joint.id),
            )
        })?;
    let parent =
        nearest_part_cell(cells, joint.parent_part, joint.parent_center).ok_or_else(|| {
            FusionError::new(
                "fusion.missingRequiredRegion",
                format!("joint {} parent part has no canonical cells", joint.id),
            )
        })?;
    let distance = manhattan(child, parent);
    if distance > u64::from(max_length) {
        return Err(FusionError::new(
            "fusion.socketBridgeTooLong",
            format!(
                "joint {} needs bridge length {distance}, limit is {max_length}",
                joint.id
            ),
        ));
    }
    let material_slot = nearest_material(cells, joint.child_center, joint.parent_center)
        .ok_or_else(|| FusionError::new("fusion.invalidPalette", "frame has no material"))?;
    let path = manhattan_path(child, parent);
    let thickness = if joint.radius >= 2.0 { 1i64 } else { 0i64 };
    let mut candidates = BTreeSet::new();
    for coordinate in path {
        candidates.insert(coordinate);
        if thickness > 0 {
            for delta in FACE_NEIGHBORS {
                let expanded = add(coordinate, delta)?;
                if point_segment_distance(
                    cell_center(expanded),
                    joint.child_center,
                    joint.parent_center,
                ) <= joint.radius.min(2.0)
                {
                    candidates.insert(expanded);
                }
            }
        }
    }
    for coordinate in candidates {
        insert_generated(
            cells,
            coordinate,
            material_slot,
            GeneratedOperation::JointBridge {
                joint_id: joint.id.clone(),
            },
            maximum_total_cells,
        )?;
    }
    Ok(())
}

fn nearest_part_cell(
    cells: &BTreeMap<[i64; 3], FusedVoxelCell>,
    part_id: u32,
    center: [f64; 3],
) -> Option<[i64; 3]> {
    cells
        .values()
        .filter(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical {
                    part_id: candidate,
                    ..
                } if candidate == part_id
            )
        })
        .min_by(|left, right| {
            squared_distance(cell_center(left.coordinate), center)
                .total_cmp(&squared_distance(cell_center(right.coordinate), center))
                .then_with(|| left.coordinate.cmp(&right.coordinate))
        })
        .map(|cell| cell.coordinate)
}

fn nearest_material(
    cells: &BTreeMap<[i64; 3], FusedVoxelCell>,
    left: [f64; 3],
    right: [f64; 3],
) -> Option<u16> {
    let midpoint = [
        (left[0] + right[0]) * 0.5,
        (left[1] + right[1]) * 0.5,
        (left[2] + right[2]) * 0.5,
    ];
    cells
        .values()
        .filter(|cell| matches!(cell.origin, FusedVoxelOrigin::Canonical { .. }))
        .min_by(|a, b| {
            squared_distance(cell_center(a.coordinate), midpoint)
                .total_cmp(&squared_distance(cell_center(b.coordinate), midpoint))
                .then_with(|| a.coordinate.cmp(&b.coordinate))
        })
        .map(|cell| cell.material_slot)
}

fn bridge_one_voxel_gaps(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    seam_coordinates: &BTreeSet<[i64; 3]>,
    maximum_total_cells: usize,
) -> Result<(), FusionError> {
    let occupied: BTreeSet<[i64; 3]> = cells.keys().copied().collect();
    let mut additions = BTreeSet::new();
    for coordinate in &occupied {
        for axis in 0..3 {
            let mut far = *coordinate;
            far[axis] = far[axis].checked_add(2).ok_or_else(|| {
                FusionError::new(
                    "fusion.coordinateOverflow",
                    "gap bridge coordinate overflow",
                )
            })?;
            let mut middle = *coordinate;
            middle[axis] = middle[axis].checked_add(1).ok_or_else(|| {
                FusionError::new(
                    "fusion.coordinateOverflow",
                    "gap bridge coordinate overflow",
                )
            })?;
            if occupied.contains(&far)
                && !occupied.contains(&middle)
                && near_seam(middle, seam_coordinates, 2)
            {
                additions.insert(middle);
            }
        }
    }
    for coordinate in additions {
        let material = neighboring_material(cells, coordinate).unwrap_or(1);
        insert_generated(
            cells,
            coordinate,
            material,
            GeneratedOperation::Cleanup {
                operation: CleanupOperation::BridgeOneVoxelGap,
            },
            maximum_total_cells,
        )?;
    }
    Ok(())
}

fn fill_one_voxel_cavities(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    seam_coordinates: &BTreeSet<[i64; 3]>,
    maximum_total_cells: usize,
) -> Result<(), FusionError> {
    let Some((lo, hi)) = bounds(cells.keys().copied()) else {
        return Ok(());
    };
    let mut candidates = BTreeSet::new();
    for cell in cells.keys() {
        for delta in FACE_NEIGHBORS {
            let candidate = add(*cell, delta)?;
            if candidate[0] < lo[0]
                || candidate[1] < lo[1]
                || candidate[2] < lo[2]
                || candidate[0] > hi[0]
                || candidate[1] > hi[1]
                || candidate[2] > hi[2]
                || cells.contains_key(&candidate)
                || !near_seam(candidate, seam_coordinates, 2)
            {
                continue;
            }
            if FACE_NEIGHBORS
                .iter()
                .all(|neighbor| add(candidate, *neighbor).is_ok_and(|n| cells.contains_key(&n)))
            {
                candidates.insert(candidate);
            }
        }
    }
    for coordinate in candidates {
        let material = neighboring_material(cells, coordinate).unwrap_or(1);
        insert_generated(
            cells,
            coordinate,
            material,
            GeneratedOperation::Cleanup {
                operation: CleanupOperation::FillOneVoxelCavity,
            },
            maximum_total_cells,
        )?;
    }
    Ok(())
}

fn enforce_minimum_limb_thickness(
    kit: &VoxelKit,
    rough: &RoughFrame,
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    maximum_total_cells: usize,
) -> Result<(), FusionError> {
    let minimum = i64::from(kit.invariants.min_limb_thickness);
    for (part_index, part) in kit.parts.iter().enumerate().filter(|(_, part)| part.limb) {
        let coordinates: Vec<[i64; 3]> = cells
            .values()
            .filter_map(|cell| {
                matches!(
                    cell.origin,
                    FusedVoxelOrigin::Canonical {
                        part_id,
                        ..
                    } if part_id == part_index as u32
                )
                .then_some(cell.coordinate)
            })
            .collect();
        let Some((lo, hi)) = bounds(coordinates.iter().copied()) else {
            continue;
        };
        let extents = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
        let main_axis = (0..3).max_by_key(|axis| extents[*axis]).unwrap_or(1);
        for axis in 0..3 {
            if axis == main_axis || extents[axis] >= minimum {
                continue;
            }
            let seam_cells: Vec<[i64; 3]> = rough
                .voxels
                .iter()
                .filter(|cell| cell.part_id == part_index as u32 && cell.needs_fusion)
                .map(|cell| cell.coordinate)
                .collect();
            for coordinate in seam_cells {
                let mut expanded = coordinate;
                expanded[axis] = expanded[axis].checked_add(1).ok_or_else(|| {
                    FusionError::new(
                        "fusion.coordinateOverflow",
                        format!("part {} thickness expansion overflow", part.id),
                    )
                })?;
                let material = neighboring_material(cells, expanded)
                    .unwrap_or_else(|| part.cells.first().map_or(1, |source| source.material_slot));
                insert_generated(
                    cells,
                    expanded,
                    material,
                    GeneratedOperation::Cleanup {
                        operation: CleanupOperation::EnforceMinimumLimbThickness,
                    },
                    maximum_total_cells,
                )?;
            }
        }
    }
    Ok(())
}

fn restore_ground_contact(
    kit: &VoxelKit,
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    _maximum_total_cells: usize,
) -> Result<(), FusionError> {
    let minimum_y = cells
        .keys()
        .map(|coordinate| coordinate[1])
        .min()
        .ok_or_else(|| FusionError::new("fusion.missingRequiredRegion", "empty frame"))?;
    let shift = kit
        .convention
        .ground_y
        .checked_sub(minimum_y)
        .ok_or_else(|| {
            FusionError::new(
                "fusion.coordinateOverflow",
                "ground correction distance overflow",
            )
        })?;
    if shift == 0 {
        return Ok(());
    }
    if shift.abs() > 4 {
        return Err(FusionError::new(
            "fusion.invalidGroundPlane",
            format!(
                "ground correction {shift} exceeds the four-cell repair policy (min y {minimum_y}, ground y {})",
                kit.convention.ground_y,
            ),
        ));
    }
    let mut grounded = BTreeMap::new();
    for (_, mut cell) in std::mem::take(cells) {
        cell.coordinate[1] = cell.coordinate[1].checked_add(shift).ok_or_else(|| {
            FusionError::new("fusion.coordinateOverflow", "ground correction overflow")
        })?;
        cell.operations.push(CleanupOperation::RestoreGroundContact);
        grounded.insert(cell.coordinate, cell);
    }
    *cells = grounded;
    Ok(())
}

fn remove_isolated_generated_voxels(cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>) {
    let occupied: BTreeSet<[i64; 3]> = cells.keys().copied().collect();
    let removals: Vec<[i64; 3]> = cells
        .values()
        .filter(|cell| matches!(cell.origin, FusedVoxelOrigin::Generated { .. }))
        .filter(|cell| {
            FACE_NEIGHBORS.iter().all(|delta| {
                add(cell.coordinate, *delta).map_or(true, |neighbor| !occupied.contains(&neighbor))
            })
        })
        .map(|cell| cell.coordinate)
        .collect();
    for coordinate in removals {
        cells.remove(&coordinate);
    }
}

fn validate_structural_frame(
    kit: &VoxelKit,
    placements: &[RigidTransform],
    rough: &RoughFrame,
    frame: &FusedFrame,
    settings: FusionSettings,
) -> Result<(), FusionError> {
    let palette: BTreeSet<u16> = kit
        .palette
        .iter()
        .flat_map(|group| group.slots.iter().map(|slot| slot.slot))
        .collect();
    for cell in &frame.voxels {
        if !palette.contains(&cell.material_slot) {
            return Err(FusionError::new(
                "fusion.invalidPalette",
                format!(
                    "cell {:?} uses material slot {} outside the kit palette",
                    cell.coordinate, cell.material_slot
                ),
            ));
        }
        if cell
            .coordinate
            .iter()
            .any(|component| !(-MAX_COORDINATE_ABS..=MAX_COORDINATE_ABS).contains(component))
        {
            return Err(FusionError::new(
                "fusion.runtimeBoundsExceeded",
                format!(
                    "cell {:?} exceeds runtime coordinate bounds",
                    cell.coordinate
                ),
            ));
        }
    }

    for (part_index, part) in kit.parts.iter().enumerate() {
        let part_cells: Vec<&FusedVoxelCell> = frame
            .voxels
            .iter()
            .filter(|cell| {
                matches!(
                    cell.origin,
                    FusedVoxelOrigin::Canonical {
                        part_id,
                        ..
                    } if part_id == part_index as u32
                )
            })
            .collect();
        if part_cells.is_empty() {
            return Err(FusionError::new(
                "fusion.missingRequiredRegion",
                format!("part {} has no canonical geometry in the frame", part.id),
            ));
        }
        if kit.invariants.protected_parts.contains(&part.id) && part_cells.len() < part.cells.len()
        {
            return Err(FusionError::new(
                "fusion.protectedRegionRemoved",
                format!(
                    "protected part {} retained {} cells from {} canonical cells",
                    part.id,
                    part_cells.len(),
                    part.cells.len()
                ),
            ));
        }
        for region in &part.protected_regions {
            let protected_indices: BTreeSet<u32> = part
                .cells
                .iter()
                .enumerate()
                .filter(|(_, source)| {
                    (0..3).all(|axis| {
                        source.coordinate[axis] >= region.min[axis]
                            && source.coordinate[axis] <= region.max[axis]
                    })
                })
                .map(|(index, _)| index as u32)
                .collect();
            let retained = part_cells
                .iter()
                .filter(|cell| {
                    matches!(
                        cell.origin,
                        FusedVoxelOrigin::Canonical {
                            source_voxel_index,
                            ..
                        } if protected_indices.contains(&source_voxel_index)
                    )
                })
                .count();
            if retained < protected_indices.len() {
                return Err(FusionError::new(
                    "fusion.protectedRegionRemoved",
                    format!(
                        "part {} protected region {:?}..{:?} retained {retained} cells from {} canonical cells",
                        part.id,
                        region.min,
                        region.max,
                        protected_indices.len()
                    ),
                ));
            }
        }
    }

    let occupied: BTreeSet<[i64; 3]> = frame.voxels.iter().map(|cell| cell.coordinate).collect();
    if connected_components(&occupied) != 1 {
        return Err(FusionError::new(
            "fusion.requiredGeometryDisconnected",
            format!(
                "fused frame has {} face-connected components",
                connected_components(&occupied)
            ),
        ));
    }
    let min_y = occupied
        .iter()
        .map(|coordinate| coordinate[1])
        .min()
        .unwrap_or(i64::MAX);
    if min_y != kit.convention.ground_y {
        return Err(FusionError::new(
            "fusion.invalidGroundPlane",
            format!(
                "fused frame min y {min_y} does not equal ground y {}",
                kit.convention.ground_y
            ),
        ));
    }

    validate_required_sockets(kit, placements, &occupied)?;
    validate_volume(kit, frame.voxels.len())?;
    if settings.enforce_minimum_limb_thickness {
        validate_limb_thickness(kit, frame)?;
    }
    if settings.normalize_weapon_dimensions {
        validate_fixed_part_dimensions(kit, frame)?;
    }

    let seam_coordinates: Vec<[i64; 3]> = rough
        .voxels
        .iter()
        .filter(|cell| cell.needs_fusion)
        .map(|cell| cell.coordinate)
        .collect();
    for generated in frame
        .voxels
        .iter()
        .filter(|cell| matches!(cell.origin, FusedVoxelOrigin::Generated { .. }))
    {
        let local = seam_coordinates
            .iter()
            .any(|seam| chebyshev(*seam, generated.coordinate) <= 4);
        if !local {
            return Err(FusionError::new(
                "fusion.nonLocalCleanup",
                format!(
                    "generated cell {:?} from {:?} is outside the four-cell joint seam envelope",
                    generated.coordinate, generated.origin
                ),
            ));
        }
    }
    Ok(())
}

fn validate_required_sockets(
    kit: &VoxelKit,
    placements: &[RigidTransform],
    occupied: &BTreeSet<[i64; 3]>,
) -> Result<(), FusionError> {
    for required in &kit.invariants.required_sockets {
        let (part_id, socket_id) = required.split_once('.').ok_or_else(|| {
            FusionError::new(
                "fusion.missingAnchor",
                format!("required socket {required} is malformed"),
            )
        })?;
        let part_index = kit
            .parts
            .iter()
            .position(|part| part.id == part_id)
            .ok_or_else(|| {
                FusionError::new(
                    "fusion.missingAnchor",
                    format!("required socket part {part_id} is absent"),
                )
            })?;
        let socket = kit.parts[part_index].socket(socket_id).ok_or_else(|| {
            FusionError::new(
                "fusion.missingAnchor",
                format!("required socket {required} is absent"),
            )
        })?;
        let center = placements[part_index].apply(socket.position);
        let radius = socket.radius.ceil() + 1.0;
        if !occupied
            .iter()
            .any(|cell| squared_distance(cell_center(*cell), center) <= radius * radius)
        {
            return Err(FusionError::new(
                "fusion.missingAnchor",
                format!("required socket {required} has no occupied neighborhood"),
            ));
        }
    }
    Ok(())
}

fn validate_volume(kit: &VoxelKit, volume: usize) -> Result<(), FusionError> {
    if let Some([minimum, maximum]) = kit.invariants.volume_range {
        let volume = volume as u64;
        if volume < minimum || volume > maximum {
            return Err(FusionError::new(
                "fusion.volumeOutOfRange",
                format!("fused volume {volume} is outside kit range [{minimum}, {maximum}]"),
            ));
        }
    }
    Ok(())
}

fn validate_limb_thickness(kit: &VoxelKit, frame: &FusedFrame) -> Result<(), FusionError> {
    let minimum = i64::from(kit.invariants.min_limb_thickness);
    for (part_index, part) in kit.parts.iter().enumerate().filter(|(_, part)| part.limb) {
        let coordinates = frame.voxels.iter().filter_map(|cell| {
            matches!(
                cell.origin,
                FusedVoxelOrigin::Canonical {
                    part_id,
                    ..
                } if part_id == part_index as u32
            )
            .then_some(cell.coordinate)
        });
        let Some((lo, hi)) = bounds(coordinates) else {
            continue;
        };
        let mut extents = [hi[0] - lo[0] + 1, hi[1] - lo[1] + 1, hi[2] - lo[2] + 1];
        extents.sort();
        if extents[0] < minimum || extents[1] < minimum {
            return Err(FusionError::new(
                "fusion.minimumLimbThickness",
                format!(
                    "limb {} cross-section {:?} is below minimum {minimum}",
                    part.id,
                    &extents[..2]
                ),
            ));
        }
    }
    Ok(())
}

fn validate_fixed_part_dimensions(kit: &VoxelKit, frame: &FusedFrame) -> Result<(), FusionError> {
    for dimension in &kit.invariants.fixed_dimensions {
        if dimension.subject == "character" {
            continue;
        }
        let Some(part_index) = kit
            .parts
            .iter()
            .position(|part| part.id == dimension.subject)
        else {
            continue;
        };
        let coordinates = frame.voxels.iter().filter_map(|cell| match cell.origin {
            FusedVoxelOrigin::Canonical {
                part_id,
                source_voxel_index,
            } if part_id == part_index as u32 => kit.parts[part_index]
                .cells
                .get(source_voxel_index as usize)
                .map(|source| source.coordinate),
            _ => None,
        });
        let Some((lo, hi)) = bounds(coordinates) else {
            continue;
        };
        let axis = match dimension.axis.as_str() {
            "width" => 0,
            "height" => 1,
            "depth" => 2,
            _ => continue,
        };
        let extent = hi[axis] - lo[axis] + 1;
        if extent < dimension.range[0] || extent > dimension.range[1] {
            return Err(FusionError::new(
                "fusion.invalidWeaponDimension",
                format!(
                    "{} {} extent {extent} is outside [{}, {}]",
                    dimension.subject, dimension.axis, dimension.range[0], dimension.range[1]
                ),
            ));
        }
    }
    Ok(())
}

fn insert_generated(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    coordinate: [i64; 3],
    material_slot: u16,
    operation: GeneratedOperation,
    maximum_total_cells: usize,
) -> Result<(), FusionError> {
    if cells.contains_key(&coordinate) {
        return Ok(());
    }
    if cells.len() >= maximum_total_cells {
        return Err(FusionError::new(
            "fusion.generatedVoxelQuotaExceeded",
            format!("candidate cell cap {maximum_total_cells} would be exceeded"),
        ));
    }
    let operations = match &operation {
        GeneratedOperation::JointBridge { .. } => {
            vec![CleanupOperation::RepairSocketNeighborhood]
        }
        GeneratedOperation::Cleanup { operation } => vec![*operation],
    };
    cells.insert(
        coordinate,
        FusedVoxelCell {
            coordinate,
            material_slot,
            origin: FusedVoxelOrigin::Generated { operation },
            operations,
        },
    );
    Ok(())
}

fn neighboring_material(
    cells: &BTreeMap<[i64; 3], FusedVoxelCell>,
    coordinate: [i64; 3],
) -> Option<u16> {
    FACE_NEIGHBORS
        .iter()
        .filter_map(|delta| add(coordinate, *delta).ok())
        .filter_map(|neighbor| cells.get(&neighbor))
        .min_by_key(|cell| (cell.coordinate, cell.material_slot))
        .map(|cell| cell.material_slot)
}

fn connected_components(cells: &BTreeSet<[i64; 3]>) -> usize {
    let mut unseen = cells.clone();
    let mut components = 0;
    while let Some(start) = unseen.pop_first() {
        components += 1;
        let mut pending = vec![start];
        while let Some(coordinate) = pending.pop() {
            for delta in FACE_NEIGHBORS {
                if let Ok(neighbor) = add(coordinate, delta) {
                    if unseen.remove(&neighbor) {
                        pending.push(neighbor);
                    }
                }
            }
        }
    }
    components
}

fn manhattan_path(mut current: [i64; 3], target: [i64; 3]) -> Vec<[i64; 3]> {
    let mut path = vec![current];
    while current != target {
        let axis = (0..3)
            .max_by_key(|axis| (target[*axis] - current[*axis]).abs())
            .unwrap_or(0);
        current[axis] += (target[axis] - current[axis]).signum();
        path.push(current);
    }
    path
}

fn manhattan(left: [i64; 3], right: [i64; 3]) -> u64 {
    (0..3).map(|axis| left[axis].abs_diff(right[axis])).sum()
}

fn chebyshev(left: [i64; 3], right: [i64; 3]) -> u64 {
    (0..3)
        .map(|axis| left[axis].abs_diff(right[axis]))
        .max()
        .unwrap_or(0)
}

fn near_seam(coordinate: [i64; 3], seam_coordinates: &BTreeSet<[i64; 3]>, radius: u64) -> bool {
    seam_coordinates
        .iter()
        .any(|seam| chebyshev(*seam, coordinate) <= radius)
}

fn add(left: [i64; 3], right: [i64; 3]) -> Result<[i64; 3], FusionError> {
    Ok([
        left[0].checked_add(right[0]).ok_or_else(|| {
            FusionError::new("fusion.coordinateOverflow", "X coordinate overflow")
        })?,
        left[1].checked_add(right[1]).ok_or_else(|| {
            FusionError::new("fusion.coordinateOverflow", "Y coordinate overflow")
        })?,
        left[2].checked_add(right[2]).ok_or_else(|| {
            FusionError::new("fusion.coordinateOverflow", "Z coordinate overflow")
        })?,
    ])
}

fn cell_center(coordinate: [i64; 3]) -> [f64; 3] {
    [
        coordinate[0] as f64 + 0.5,
        coordinate[1] as f64 + 0.5,
        coordinate[2] as f64 + 0.5,
    ]
}

fn squared_distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    (left[0] - right[0]).powi(2) + (left[1] - right[1]).powi(2) + (left[2] - right[2]).powi(2)
}

fn point_segment_distance(point: [f64; 3], start: [f64; 3], end: [f64; 3]) -> f64 {
    let delta = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let length_squared = delta[0].powi(2) + delta[1].powi(2) + delta[2].powi(2);
    if length_squared <= f64::EPSILON {
        return squared_distance(point, start).sqrt();
    }
    let relative = [
        point[0] - start[0],
        point[1] - start[1],
        point[2] - start[2],
    ];
    let t = ((relative[0] * delta[0] + relative[1] * delta[1] + relative[2] * delta[2])
        / length_squared)
        .clamp(0.0, 1.0);
    let nearest = [
        start[0] + t * delta[0],
        start[1] + t * delta[1],
        start[2] + t * delta[2],
    ];
    squared_distance(point, nearest).sqrt()
}

fn bounds(coordinates: impl IntoIterator<Item = [i64; 3]>) -> Option<([i64; 3], [i64; 3])> {
    let mut iter = coordinates.into_iter();
    let first = iter.next()?;
    let mut lo = first;
    let mut hi = first;
    for coordinate in iter {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(coordinate[axis]);
            hi[axis] = hi[axis].max(coordinate[axis]);
        }
    }
    Some((lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manhattan_path_is_deterministic_and_face_connected() {
        let path = manhattan_path([0, 0, 0], [2, -1, 1]);
        assert_eq!(path.first(), Some(&[0, 0, 0]));
        assert_eq!(path.last(), Some(&[2, -1, 1]));
        assert_eq!(path.len(), 5);
        assert!(path.windows(2).all(|pair| manhattan(pair[0], pair[1]) == 1));
        assert_eq!(path, manhattan_path([0, 0, 0], [2, -1, 1]));
    }

    #[test]
    fn settings_reject_zero_or_unbounded_quotas() {
        assert_eq!(
            FusionSettings {
                max_generated_voxels: 0,
                ..FusionSettings::default()
            }
            .validate()
            .expect_err("zero quota")
            .code(),
            "fusion.invalidSettings"
        );
        assert_eq!(
            FusionSettings {
                max_socket_bridge_length: 257,
                ..FusionSettings::default()
            }
            .validate()
            .expect_err("unbounded bridge")
            .code(),
            "fusion.invalidSettings"
        );
    }
}
