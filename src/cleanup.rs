//! Bounded, replayable cleanup edits for deterministic fused frames.
//!
//! This is an authoring boundary, not a runtime callback or scheduler. Every
//! result regenerates from an immutable fused base plus an ordered diff list.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::fusion::{FusedFrame, FusedVoxelCell, FusedVoxelOrigin};
use crate::kit::{VoxelKit, MAX_COORDINATE_ABS};
use crate::project::sha256;

const EDIT_DIFF_SCHEMA_VERSION: u32 = 1;
const FACE_NEIGHBORS: [[i64; 3]; 6] = [
    [-1, 0, 0],
    [1, 0, 0],
    [0, -1, 0],
    [0, 1, 0],
    [0, 0, -1],
    [0, 0, 1],
];
type CanonicalOriginMap = BTreeMap<(u32, u32), BTreeMap<[i64; 3], u16>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditBounds {
    pub min: [i64; 3],
    pub max: [i64; 3],
}

impl EditBounds {
    #[must_use]
    pub fn point(point: [i64; 3]) -> Self {
        Self {
            min: point,
            max: point,
        }
    }

    #[must_use]
    pub fn contains(self, coordinate: [i64; 3]) -> bool {
        (0..3).all(|axis| (self.min[axis]..=self.max[axis]).contains(&coordinate[axis]))
    }

    fn validate(self) -> Result<(), EditError> {
        for axis in 0..3 {
            if self.min[axis] > self.max[axis]
                || !(-MAX_COORDINATE_ABS..=MAX_COORDINATE_ABS).contains(&self.min[axis])
                || !(-MAX_COORDINATE_ABS..=MAX_COORDINATE_ABS).contains(&self.max[axis])
            {
                return Err(EditError::new(
                    "edit.invalidBounds",
                    "bounds",
                    format!("invalid bounded region {:?}..{:?}", self.min, self.max),
                ));
            }
        }
        Ok(())
    }

    fn contains_bounds(self, other: Self) -> bool {
        self.contains(other.min) && self.contains(other.max)
    }

    fn union(self, other: Self) -> Self {
        Self {
            min: std::array::from_fn(|axis| self.min[axis].min(other.min[axis])),
            max: std::array::from_fn(|axis| self.max[axis].max(other.max[axis])),
        }
    }

    fn translated(self, offset: [i64; 3]) -> Result<Self, EditError> {
        let translated = Self {
            min: translate(self.min, offset)?,
            max: translate(self.max, offset)?,
        };
        translated.validate()?;
        Ok(translated)
    }

    fn volume(self) -> Result<usize, EditError> {
        (0..3).try_fold(1usize, |volume, axis| {
            let extent = self.max[axis]
                .checked_sub(self.min[axis])
                .and_then(|value| value.checked_add(1))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    EditError::new(
                        "edit.regionQuotaExceeded",
                        "bounds",
                        "edit-region volume overflowed",
                    )
                })?;
            volume.checked_mul(extent).ok_or_else(|| {
                EditError::new(
                    "edit.regionQuotaExceeded",
                    "bounds",
                    "edit-region volume overflowed",
                )
            })
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FrameEditOperation {
    AddVoxel {
        at: [i64; 3],
        material_slot: u16,
    },
    RemoveVoxel {
        at: [i64; 3],
    },
    MoveVoxel {
        from: [i64; 3],
        to: [i64; 3],
    },
    FillBox {
        region: EditBounds,
        material_slot: u16,
    },
    ClearBox {
        region: EditBounds,
    },
    ReplaceMaterial {
        region: EditBounds,
        from_material_slot: u16,
        to_material_slot: u16,
    },
    BridgeRegions {
        from: [i64; 3],
        to: [i64; 3],
        material_slot: u16,
    },
    ThickenRegion {
        region: EditBounds,
        material_slot: u16,
        layers: u8,
    },
    ThinRegion {
        region: EditBounds,
        layers: u8,
    },
    CopyCanonicalRegion {
        source: EditBounds,
        offset: [i64; 3],
    },
    RestoreFromPreviousFrame {
        region: EditBounds,
    },
    RestoreFromNextFrame {
        region: EditBounds,
    },
    SmoothLocalSurface {
        region: EditBounds,
        material_slot: u16,
    },
    CarveLocalSurface {
        region: EditBounds,
        maximum_face_neighbors: u8,
    },
    EnforceConnectivity {
        region: EditBounds,
        material_slot: u16,
        maximum_bridge_length: u32,
    },
    ShiftComponent {
        region: EditBounds,
        offset: [i64; 3],
    },
    SetAnchor {
        id: String,
        position: [i64; 3],
    },
}

impl FrameEditOperation {
    pub fn affected_bounds(&self) -> Result<EditBounds, EditError> {
        let bounds = match self {
            Self::AddVoxel { at, .. } | Self::RemoveVoxel { at } => EditBounds::point(*at),
            Self::MoveVoxel { from, to } | Self::BridgeRegions { from, to, .. } => {
                EditBounds::point(*from).union(EditBounds::point(*to))
            }
            Self::FillBox { region, .. }
            | Self::ClearBox { region }
            | Self::ReplaceMaterial { region, .. }
            | Self::ThickenRegion { region, .. }
            | Self::ThinRegion { region, .. }
            | Self::RestoreFromPreviousFrame { region }
            | Self::RestoreFromNextFrame { region }
            | Self::SmoothLocalSurface { region, .. }
            | Self::CarveLocalSurface { region, .. }
            | Self::EnforceConnectivity { region, .. } => *region,
            Self::CopyCanonicalRegion { source, offset }
            | Self::ShiftComponent {
                region: source,
                offset,
            } => source.union(source.translated(*offset)?),
            Self::SetAnchor { position, .. } => EditBounds::point(*position),
        };
        bounds.validate()?;
        Ok(bounds)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CleanupPass {
    AgentGeometry { pass: u8 },
    Temporal,
    Human,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameEditDiff {
    pub schema_version: u32,
    pub base_frame_sha256: String,
    pub pass: CleanupPass,
    pub operations: Vec<FrameEditOperation>,
}

impl FrameEditDiff {
    pub fn new(
        base: &FusedFrame,
        pass: CleanupPass,
        operations: Vec<FrameEditOperation>,
    ) -> Result<Self, EditError> {
        Ok(Self {
            schema_version: EDIT_DIFF_SCHEMA_VERSION,
            base_frame_sha256: frame_hash(base)?,
            pass,
            operations,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditedFrame {
    pub frame: FusedFrame,
    pub anchors: BTreeMap<String, [i64; 3]>,
    pub diffs: Vec<FrameEditDiff>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupMetricGates {
    pub max_occupied_voxel_increase: usize,
    pub max_component_increase: usize,
    pub allow_additional_warnings: bool,
}

impl Default for CleanupMetricGates {
    fn default() -> Self {
        Self {
            max_occupied_voxel_increase: 8_192,
            max_component_increase: 0,
            allow_additional_warnings: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CleanupDecision {
    Accept,
    Revise { reasons: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CleanupEvaluation {
    pub decision: CleanupDecision,
    pub candidate: EditedFrame,
    pub before: AgentInputBundle,
    pub after: AgentInputBundle,
    pub occupied_voxel_delta: i64,
    pub connected_component_delta: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPolicy {
    pub declared_regions: Vec<EditBounds>,
    pub material_slots: BTreeSet<u16>,
    pub max_voxel_count: usize,
    pub max_operation_cells: usize,
    pub max_operations_per_diff: usize,
    pub required_anchors: BTreeSet<String>,
    pub protected_origins: BTreeSet<(u32, u32)>,
    pub protected_parts: BTreeSet<u32>,
    pub protected_dimension_tolerance: [i64; 3],
    pub connected_parts: BTreeSet<u32>,
}

impl EditPolicy {
    pub fn for_kit(
        kit: &VoxelKit,
        declared_regions: Vec<EditBounds>,
        max_voxel_count: usize,
    ) -> Result<Self, EditError> {
        kit.validate()
            .map_err(|error| EditError::new("edit.invalidKit", "kit", error.to_string()))?;
        if declared_regions.is_empty() {
            return Err(EditError::new(
                "edit.noDeclaredRegions",
                "declaredRegions",
                "at least one editable region is required",
            ));
        }
        for region in &declared_regions {
            region.validate()?;
        }
        if max_voxel_count == 0 || max_voxel_count > 1_000_000 {
            return Err(EditError::new(
                "edit.invalidQuota",
                "maxVoxelCount",
                "maxVoxelCount must be within 1..=1000000",
            ));
        }
        let protected_names = kit
            .invariants
            .protected_parts
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let mut protected_origins = BTreeSet::new();
        let mut protected_parts = BTreeSet::new();
        for (part_index, part) in kit.parts.iter().enumerate() {
            let part_index = u32::try_from(part_index).map_err(|_| {
                EditError::new("edit.invalidKit", "kit.parts", "part index exceeds u32")
            })?;
            if protected_names.contains(part.id.as_str()) {
                protected_parts.insert(part_index);
            }
            for (voxel_index, cell) in part.cells.iter().enumerate() {
                if part.protected_regions.iter().any(|region| {
                    EditBounds {
                        min: region.min,
                        max: region.max,
                    }
                    .contains(cell.coordinate)
                }) {
                    protected_origins.insert((
                        part_index,
                        u32::try_from(voxel_index).map_err(|_| {
                            EditError::new(
                                "edit.invalidKit",
                                "kit.parts.cells",
                                "voxel index exceeds u32",
                            )
                        })?,
                    ));
                }
            }
        }
        Ok(Self {
            declared_regions,
            material_slots: kit
                .palette
                .iter()
                .flat_map(|group| group.slots.iter().map(|slot| slot.slot))
                .collect(),
            max_voxel_count,
            max_operation_cells: 262_144,
            max_operations_per_diff: 256,
            required_anchors: BTreeSet::new(),
            protected_origins,
            protected_parts,
            protected_dimension_tolerance: [0, 0, 0],
            connected_parts: BTreeSet::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditError {
    code: &'static str,
    path: String,
    message: String,
    operation_index: Option<usize>,
}

impl EditError {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
            operation_index: None,
        }
    }

    fn at_operation(mut self, index: usize) -> Self {
        self.operation_index = Some(index);
        self
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn operation_index(&self) -> Option<usize> {
        self.operation_index
    }
}

impl fmt::Display for EditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(index) = self.operation_index {
            write!(
                formatter,
                "{} at operations[{index}].{}: {}",
                self.code, self.path, self.message
            )
        } else {
            write!(
                formatter,
                "{} at {}: {}",
                self.code, self.path, self.message
            )
        }
    }
}

impl std::error::Error for EditError {}

pub fn replay_frame_edits(
    base: &FusedFrame,
    previous: Option<&FusedFrame>,
    next: Option<&FusedFrame>,
    initial_anchors: &BTreeMap<String, [i64; 3]>,
    policy: &EditPolicy,
    diffs: &[FrameEditDiff],
) -> Result<EditedFrame, EditError> {
    validate_pass_budget(diffs)?;
    let expected_hash = frame_hash(base)?;
    let mut result = EditedFrame {
        frame: base.clone(),
        anchors: initial_anchors.clone(),
        diffs: Vec::new(),
    };
    for diff in diffs {
        if diff.schema_version != EDIT_DIFF_SCHEMA_VERSION {
            return Err(EditError::new(
                "edit.unsupportedSchema",
                "schemaVersion",
                format!("unsupported edit schema {}", diff.schema_version),
            ));
        }
        if diff.base_frame_sha256 != expected_hash {
            return Err(EditError::new(
                "edit.baseHashMismatch",
                "baseFrameSha256",
                "edit diff does not target the supplied deterministic base",
            ));
        }
        apply_diff(&mut result, base, previous, next, policy, diff)?;
        result.diffs.push(diff.clone());
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub fn evaluate_cleanup_diff(
    kit: &VoxelKit,
    base: &FusedFrame,
    previous: Option<&FusedFrame>,
    next: Option<&FusedFrame>,
    initial_anchors: &BTreeMap<String, [i64; 3]>,
    policy: &EditPolicy,
    accepted_diffs: &[FrameEditDiff],
    proposed_diff: FrameEditDiff,
    style_rules: Vec<String>,
    metric_gates: CleanupMetricGates,
) -> Result<CleanupEvaluation, EditError> {
    let before_frame = replay_frame_edits(
        base,
        previous,
        next,
        initial_anchors,
        policy,
        accepted_diffs,
    )?;
    let mut candidate_diffs = accepted_diffs.to_vec();
    candidate_diffs.push(proposed_diff);
    let candidate = replay_frame_edits(
        base,
        previous,
        next,
        initial_anchors,
        policy,
        &candidate_diffs,
    )?;
    let before = build_agent_input_bundle(
        kit,
        previous,
        &before_frame.frame,
        next,
        style_rules.clone(),
        before_frame.anchors,
    )?;
    let after = build_agent_input_bundle(
        kit,
        previous,
        &candidate.frame,
        next,
        style_rules,
        candidate.anchors.clone(),
    )?;
    let occupied_voxel_delta = signed_delta(
        after.metrics.occupied_voxels,
        before.metrics.occupied_voxels,
    )?;
    let connected_component_delta = signed_delta(
        after.metrics.connected_components,
        before.metrics.connected_components,
    )?;
    let mut reasons = Vec::new();
    if occupied_voxel_delta
        > i64::try_from(metric_gates.max_occupied_voxel_increase).unwrap_or(i64::MAX)
    {
        reasons.push(format!(
            "occupied voxel increase {occupied_voxel_delta} exceeds {}",
            metric_gates.max_occupied_voxel_increase
        ));
    }
    if connected_component_delta
        > i64::try_from(metric_gates.max_component_increase).unwrap_or(i64::MAX)
    {
        reasons.push(format!(
            "connected component increase {connected_component_delta} exceeds {}",
            metric_gates.max_component_increase
        ));
    }
    if !metric_gates.allow_additional_warnings
        && after.structural_warnings.len() > before.structural_warnings.len()
    {
        reasons.push(format!(
            "structural warnings increased from {} to {}",
            before.structural_warnings.len(),
            after.structural_warnings.len()
        ));
    }
    Ok(CleanupEvaluation {
        decision: if reasons.is_empty() {
            CleanupDecision::Accept
        } else {
            CleanupDecision::Revise { reasons }
        },
        candidate,
        before,
        after,
        occupied_voxel_delta,
        connected_component_delta,
    })
}

fn apply_diff(
    result: &mut EditedFrame,
    base: &FusedFrame,
    previous: Option<&FusedFrame>,
    next: Option<&FusedFrame>,
    policy: &EditPolicy,
    diff: &FrameEditDiff,
) -> Result<(), EditError> {
    if diff.operations.len() > policy.max_operations_per_diff {
        return Err(EditError::new(
            "edit.operationQuotaExceeded",
            "operations",
            format!(
                "{} operations exceed the {} operation quota",
                diff.operations.len(),
                policy.max_operations_per_diff
            ),
        ));
    }
    let mut cells = cell_map(&result.frame)?;
    let mut anchors = result.anchors.clone();
    for (index, operation) in diff.operations.iter().enumerate() {
        let affected = operation
            .affected_bounds()
            .map_err(|error| error.at_operation(index))?;
        if !policy
            .declared_regions
            .iter()
            .any(|region| region.contains_bounds(affected))
        {
            return Err(EditError::new(
                "edit.undeclaredRegion",
                "affectedBounds",
                format!(
                    "operation affects {:?}..{:?}, outside every declared region",
                    affected.min, affected.max
                ),
            )
            .at_operation(index));
        }
        if affected
            .volume()
            .map_err(|error| error.at_operation(index))?
            > policy.max_operation_cells
        {
            return Err(EditError::new(
                "edit.regionQuotaExceeded",
                "affectedBounds",
                "operation region exceeds maxOperationCells",
            )
            .at_operation(index));
        }
        apply_operation(
            &mut cells,
            &mut anchors,
            previous,
            next,
            policy,
            operation,
            index,
        )
        .map_err(|error| error.at_operation(index))?;
        if cells.len() > policy.max_voxel_count {
            return Err(EditError::new(
                "edit.voxelQuotaExceeded",
                "operation",
                format!(
                    "{} cells exceed maxVoxelCount {}",
                    cells.len(),
                    policy.max_voxel_count
                ),
            )
            .at_operation(index));
        }
    }
    validate_candidate(base, previous, next, &cells, &anchors, policy)?;
    result.frame.voxels = cells.into_values().collect();
    result.frame.generated_voxels = result
        .frame
        .voxels
        .iter()
        .filter(|cell| !matches!(cell.origin, FusedVoxelOrigin::Canonical { .. }))
        .count();
    result.anchors = anchors;
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn apply_operation(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    anchors: &mut BTreeMap<String, [i64; 3]>,
    previous: Option<&FusedFrame>,
    next: Option<&FusedFrame>,
    policy: &EditPolicy,
    operation: &FrameEditOperation,
    operation_index: usize,
) -> Result<(), EditError> {
    let edited = |coordinate, material_slot| FusedVoxelCell {
        coordinate,
        material_slot,
        origin: FusedVoxelOrigin::AuthoredEdit {
            operation_index: u32::try_from(operation_index).unwrap_or(u32::MAX),
        },
        operations: Vec::new(),
    };
    match operation {
        FrameEditOperation::AddVoxel { at, material_slot } => {
            validate_material(policy, *material_slot)?;
            if cells.insert(*at, edited(*at, *material_slot)).is_some() {
                return Err(EditError::new(
                    "edit.coordinateOccupied",
                    "at",
                    format!("voxel already exists at {at:?}"),
                ));
            }
        }
        FrameEditOperation::RemoveVoxel { at } => {
            if cells.remove(at).is_none() {
                return Err(EditError::new(
                    "edit.coordinateEmpty",
                    "at",
                    format!("no voxel exists at {at:?}"),
                ));
            }
        }
        FrameEditOperation::MoveVoxel { from, to } => {
            if cells.contains_key(to) {
                return Err(EditError::new(
                    "edit.coordinateOccupied",
                    "to",
                    format!("destination {to:?} is occupied"),
                ));
            }
            let mut cell = cells.remove(from).ok_or_else(|| {
                EditError::new(
                    "edit.coordinateEmpty",
                    "from",
                    format!("no voxel exists at {from:?}"),
                )
            })?;
            cell.coordinate = *to;
            cells.insert(*to, cell);
        }
        FrameEditOperation::FillBox {
            region,
            material_slot,
        } => {
            validate_material(policy, *material_slot)?;
            for coordinate in coordinates(*region) {
                cells
                    .entry(coordinate)
                    .or_insert_with(|| edited(coordinate, *material_slot));
            }
        }
        FrameEditOperation::ClearBox { region } => {
            cells.retain(|coordinate, _| !region.contains(*coordinate));
        }
        FrameEditOperation::ReplaceMaterial {
            region,
            from_material_slot,
            to_material_slot,
        } => {
            validate_material(policy, *from_material_slot)?;
            validate_material(policy, *to_material_slot)?;
            for cell in cells.values_mut().filter(|cell| {
                region.contains(cell.coordinate) && cell.material_slot == *from_material_slot
            }) {
                cell.material_slot = *to_material_slot;
            }
        }
        FrameEditOperation::BridgeRegions {
            from,
            to,
            material_slot,
        } => {
            validate_material(policy, *material_slot)?;
            for coordinate in manhattan_path(*from, *to) {
                cells
                    .entry(coordinate)
                    .or_insert_with(|| edited(coordinate, *material_slot));
            }
        }
        FrameEditOperation::ThickenRegion {
            region,
            material_slot,
            layers,
        } => {
            validate_material(policy, *material_slot)?;
            validate_layers(*layers)?;
            for _ in 0..*layers {
                let additions = cells
                    .keys()
                    .copied()
                    .filter(|coordinate| region.contains(*coordinate))
                    .flat_map(neighbors)
                    .filter(|coordinate| {
                        region.contains(*coordinate) && !cells.contains_key(coordinate)
                    })
                    .collect::<BTreeSet<_>>();
                for coordinate in additions {
                    cells.insert(coordinate, edited(coordinate, *material_slot));
                }
            }
        }
        FrameEditOperation::ThinRegion { region, layers } => {
            validate_layers(*layers)?;
            for _ in 0..*layers {
                let removals = cells
                    .keys()
                    .copied()
                    .filter(|coordinate| region.contains(*coordinate))
                    .filter(|coordinate| {
                        neighbors(*coordinate)
                            .into_iter()
                            .any(|neighbor| !cells.contains_key(&neighbor))
                    })
                    .collect::<Vec<_>>();
                for coordinate in removals {
                    cells.remove(&coordinate);
                }
            }
        }
        FrameEditOperation::CopyCanonicalRegion { source, offset } => {
            let copies = cells
                .values()
                .filter(|cell| {
                    source.contains(cell.coordinate)
                        && matches!(cell.origin, FusedVoxelOrigin::Canonical { .. })
                })
                .cloned()
                .collect::<Vec<_>>();
            for mut cell in copies {
                cell.coordinate = translate(cell.coordinate, *offset)?;
                // A copy is new authored geometry, not a second authority for
                // the canonical source voxel it was shaped from.
                cell.origin = FusedVoxelOrigin::AuthoredEdit {
                    operation_index: u32::try_from(operation_index).unwrap_or(u32::MAX),
                };
                cells.entry(cell.coordinate).or_insert(cell);
            }
        }
        FrameEditOperation::RestoreFromPreviousFrame { region } => {
            restore_region(cells, previous, *region, "previous")?;
        }
        FrameEditOperation::RestoreFromNextFrame { region } => {
            restore_region(cells, next, *region, "next")?;
        }
        FrameEditOperation::SmoothLocalSurface {
            region,
            material_slot,
        } => {
            validate_material(policy, *material_slot)?;
            let additions = coordinates(*region)
                .filter(|coordinate| !cells.contains_key(coordinate))
                .filter(|coordinate| {
                    neighbors(*coordinate)
                        .into_iter()
                        .filter(|neighbor| cells.contains_key(neighbor))
                        .count()
                        >= 5
                })
                .collect::<Vec<_>>();
            for coordinate in additions {
                cells.insert(coordinate, edited(coordinate, *material_slot));
            }
        }
        FrameEditOperation::CarveLocalSurface {
            region,
            maximum_face_neighbors,
        } => {
            if *maximum_face_neighbors > 5 {
                return Err(EditError::new(
                    "edit.invalidSurfaceThreshold",
                    "maximumFaceNeighbors",
                    "maximumFaceNeighbors must be within 0..=5",
                ));
            }
            let removals = cells
                .keys()
                .copied()
                .filter(|coordinate| region.contains(*coordinate))
                .filter(|coordinate| {
                    neighbors(*coordinate)
                        .into_iter()
                        .filter(|neighbor| cells.contains_key(neighbor))
                        .count()
                        <= usize::from(*maximum_face_neighbors)
                })
                .collect::<Vec<_>>();
            for coordinate in removals {
                cells.remove(&coordinate);
            }
        }
        FrameEditOperation::EnforceConnectivity {
            region,
            material_slot,
            maximum_bridge_length,
        } => {
            validate_material(policy, *material_slot)?;
            connect_region(
                cells,
                *region,
                *material_slot,
                *maximum_bridge_length,
                operation_index,
            )?;
        }
        FrameEditOperation::ShiftComponent { region, offset } => {
            let moving = cells
                .values()
                .filter(|cell| region.contains(cell.coordinate))
                .cloned()
                .collect::<Vec<_>>();
            let sources = moving
                .iter()
                .map(|cell| cell.coordinate)
                .collect::<BTreeSet<_>>();
            let mut shifted = Vec::with_capacity(moving.len());
            for mut cell in moving {
                let destination = translate(cell.coordinate, *offset)?;
                if cells.contains_key(&destination) && !sources.contains(&destination) {
                    return Err(EditError::new(
                        "edit.coordinateOccupied",
                        "offset",
                        format!("shift destination {destination:?} is occupied"),
                    ));
                }
                cell.coordinate = destination;
                shifted.push(cell);
            }
            cells.retain(|coordinate, _| !sources.contains(coordinate));
            cells.extend(shifted.into_iter().map(|cell| (cell.coordinate, cell)));
        }
        FrameEditOperation::SetAnchor { id, position } => {
            if id.is_empty() || id.len() > 128 {
                return Err(EditError::new(
                    "edit.invalidAnchor",
                    "id",
                    "anchor identity must contain 1..=128 bytes",
                ));
            }
            anchors.insert(id.clone(), *position);
        }
    }
    Ok(())
}

fn validate_candidate(
    base: &FusedFrame,
    previous: Option<&FusedFrame>,
    next: Option<&FusedFrame>,
    cells: &BTreeMap<[i64; 3], FusedVoxelCell>,
    anchors: &BTreeMap<String, [i64; 3]>,
    policy: &EditPolicy,
) -> Result<(), EditError> {
    for required in &policy.required_anchors {
        if !anchors.contains_key(required) {
            return Err(EditError::new(
                "edit.requiredAnchorMissing",
                "anchors",
                format!("required anchor {required} is missing"),
            ));
        }
    }
    let base_origins = canonical_origin_map(base.voxels.iter())?;
    let previous_origins = previous
        .map(|frame| canonical_origin_map(frame.voxels.iter()))
        .transpose()?;
    let next_origins = next
        .map(|frame| canonical_origin_map(frame.voxels.iter()))
        .transpose()?;
    let candidate_origins = canonical_origin_map(cells.values())?;
    for (identity, candidate_footprint) in &candidate_origins {
        let allowed_count = base_origins
            .get(identity)
            .map_or(0, BTreeMap::len)
            .max(
                previous_origins
                    .as_ref()
                    .and_then(|origins| origins.get(identity))
                    .map_or(0, BTreeMap::len),
            )
            .max(
                next_origins
                    .as_ref()
                    .and_then(|origins| origins.get(identity))
                    .map_or(0, BTreeMap::len),
            )
            .max(1);
        if candidate_footprint.len() > allowed_count {
            return Err(EditError::new(
                "edit.duplicateCanonicalIdentity",
                "voxels",
                format!(
                    "canonical identity {identity:?} has {} occupied records; \
                     authoritative base/neighbor frames allow at most {allowed_count}",
                    candidate_footprint.len(),
                ),
            ));
        }
    }
    for origin in &policy.protected_origins {
        if candidate_origins.get(origin) != base_origins.get(origin) {
            return Err(EditError::new(
                "edit.protectedRegionChanged",
                "voxels",
                format!("protected canonical voxel {origin:?} changed"),
            ));
        }
    }
    for part in &policy.protected_parts {
        let base_part = part_cells(&base_origins, *part);
        let candidate_part = part_cells(&candidate_origins, *part);
        if base_part.len() != candidate_part.len() {
            return Err(EditError::new(
                "edit.protectedPartChanged",
                "voxels",
                format!("protected part {part} changed voxel count"),
            ));
        }
        let base_bounds = bounds(base_part.iter().copied()).ok_or_else(|| {
            EditError::new(
                "edit.invalidBase",
                "voxels",
                format!("protected part {part} is absent from the base"),
            )
        })?;
        let candidate_bounds = bounds(candidate_part.iter().copied()).ok_or_else(|| {
            EditError::new(
                "edit.protectedPartChanged",
                "voxels",
                format!("protected part {part} was removed"),
            )
        })?;
        for axis in 0..3 {
            let base_extent = base_bounds.1[axis] - base_bounds.0[axis];
            let candidate_extent = candidate_bounds.1[axis] - candidate_bounds.0[axis];
            if (candidate_extent - base_extent).abs() > policy.protected_dimension_tolerance[axis] {
                return Err(EditError::new(
                    "edit.protectedDimensionChanged",
                    "voxels",
                    format!("protected part {part} changed extent on axis {axis}"),
                ));
            }
        }
    }
    for part in &policy.connected_parts {
        let coordinates = part_cells(&candidate_origins, *part);
        if !coordinates.is_empty() && components(&coordinates).len() != 1 {
            return Err(EditError::new(
                "edit.requiredComponentDisconnected",
                "voxels",
                format!("canonical cells for part {part} are disconnected"),
            ));
        }
    }
    Ok(())
}

fn validate_pass_budget(diffs: &[FrameEditDiff]) -> Result<(), EditError> {
    let mut geometry = BTreeSet::new();
    let mut temporal = 0usize;
    for diff in diffs {
        match diff.pass {
            CleanupPass::AgentGeometry { pass } if (1..=3).contains(&pass) => {
                if !geometry.insert(pass) {
                    return Err(EditError::new(
                        "edit.passBudgetExceeded",
                        "pass",
                        format!("agent geometry pass {pass} is duplicated"),
                    ));
                }
            }
            CleanupPass::AgentGeometry { pass } => {
                return Err(EditError::new(
                    "edit.passBudgetExceeded",
                    "pass",
                    format!("agent geometry pass {pass} is outside 1..=3"),
                ));
            }
            CleanupPass::Temporal => temporal += 1,
            CleanupPass::Human => {}
        }
    }
    if temporal > 1 {
        return Err(EditError::new(
            "edit.passBudgetExceeded",
            "pass",
            "at most one temporal pass is allowed",
        ));
    }
    Ok(())
}

fn validate_material(policy: &EditPolicy, material_slot: u16) -> Result<(), EditError> {
    if !policy.material_slots.contains(&material_slot) {
        return Err(EditError::new(
            "edit.invalidMaterial",
            "materialSlot",
            format!("material slot {material_slot} is outside the kit palette"),
        ));
    }
    Ok(())
}

fn validate_layers(layers: u8) -> Result<(), EditError> {
    if !(1..=8).contains(&layers) {
        return Err(EditError::new(
            "edit.invalidLayers",
            "layers",
            "layers must be within 1..=8",
        ));
    }
    Ok(())
}

fn cell_map(frame: &FusedFrame) -> Result<BTreeMap<[i64; 3], FusedVoxelCell>, EditError> {
    let cells = frame
        .voxels
        .iter()
        .cloned()
        .map(|cell| (cell.coordinate, cell))
        .collect::<BTreeMap<_, _>>();
    if cells.len() != frame.voxels.len() {
        return Err(EditError::new(
            "edit.invalidBase",
            "voxels",
            "base frame contains duplicate coordinates",
        ));
    }
    Ok(cells)
}

fn frame_hash(frame: &FusedFrame) -> Result<String, EditError> {
    serde_json::to_vec(frame)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| EditError::new("edit.encode", "frame", error.to_string()))
}

fn coordinates(region: EditBounds) -> impl Iterator<Item = [i64; 3]> {
    (region.min[0]..=region.max[0]).flat_map(move |x| {
        (region.min[1]..=region.max[1])
            .flat_map(move |y| (region.min[2]..=region.max[2]).map(move |z| [x, y, z]))
    })
}

fn neighbors(coordinate: [i64; 3]) -> [[i64; 3]; 6] {
    FACE_NEIGHBORS
        .map(|offset| std::array::from_fn(|axis| coordinate[axis].saturating_add(offset[axis])))
}

fn translate(coordinate: [i64; 3], offset: [i64; 3]) -> Result<[i64; 3], EditError> {
    let mut translated = [0; 3];
    for axis in 0..3 {
        translated[axis] = coordinate[axis].checked_add(offset[axis]).ok_or_else(|| {
            EditError::new(
                "edit.coordinateOverflow",
                "offset",
                "edit translation overflowed",
            )
        })?;
    }
    EditBounds::point(translated).validate()?;
    Ok(translated)
}

fn manhattan_path(from: [i64; 3], to: [i64; 3]) -> Vec<[i64; 3]> {
    let mut current = from;
    let mut path = vec![from];
    for axis in 0..3 {
        while current[axis] != to[axis] {
            current[axis] += if current[axis] < to[axis] { 1 } else { -1 };
            path.push(current);
        }
    }
    path
}

fn restore_region(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    source: Option<&FusedFrame>,
    region: EditBounds,
    direction: &str,
) -> Result<(), EditError> {
    let source = source.ok_or_else(|| {
        EditError::new(
            "edit.temporalFrameMissing",
            "region",
            format!("{direction} frame is not available"),
        )
    })?;
    cells.retain(|coordinate, _| !region.contains(*coordinate));
    cells.extend(
        source
            .voxels
            .iter()
            .filter(|cell| region.contains(cell.coordinate))
            .cloned()
            .map(|cell| (cell.coordinate, cell)),
    );
    Ok(())
}

fn connect_region(
    cells: &mut BTreeMap<[i64; 3], FusedVoxelCell>,
    region: EditBounds,
    material_slot: u16,
    maximum_bridge_length: u32,
    operation_index: usize,
) -> Result<(), EditError> {
    if maximum_bridge_length == 0 || maximum_bridge_length > 256 {
        return Err(EditError::new(
            "edit.invalidBridgeLength",
            "maximumBridgeLength",
            "maximumBridgeLength must be within 1..=256",
        ));
    }
    loop {
        let occupied = cells
            .keys()
            .copied()
            .filter(|coordinate| region.contains(*coordinate))
            .collect::<BTreeSet<_>>();
        let groups = components(&occupied);
        if groups.len() <= 1 {
            return Ok(());
        }
        let first = &groups[0];
        let (from, to, distance) = groups[1..]
            .iter()
            .flat_map(|group| {
                first.iter().flat_map(move |from| {
                    group.iter().map(move |to| {
                        (
                            *from,
                            *to,
                            (0..3)
                                .map(|axis| (from[axis] - to[axis]).abs())
                                .sum::<i64>(),
                        )
                    })
                })
            })
            .min_by_key(|(from, to, distance)| (*distance, *from, *to))
            .expect("two non-empty components have a closest pair");
        if distance > i64::from(maximum_bridge_length) {
            return Err(EditError::new(
                "edit.connectivityBridgeTooLong",
                "maximumBridgeLength",
                format!("closest disconnected regions require {distance} cells"),
            ));
        }
        for coordinate in manhattan_path(from, to) {
            cells.entry(coordinate).or_insert(FusedVoxelCell {
                coordinate,
                material_slot,
                origin: FusedVoxelOrigin::AuthoredEdit {
                    operation_index: u32::try_from(operation_index).unwrap_or(u32::MAX),
                },
                operations: Vec::new(),
            });
        }
    }
}

fn canonical_origin_map<'a>(
    cells: impl IntoIterator<Item = &'a FusedVoxelCell>,
) -> Result<CanonicalOriginMap, EditError> {
    let mut origins = CanonicalOriginMap::new();
    for cell in cells {
        if let FusedVoxelOrigin::Canonical {
            part_id,
            source_voxel_index,
        } = cell.origin
        {
            let identity = (part_id, source_voxel_index);
            if origins
                .entry(identity)
                .or_default()
                .insert(cell.coordinate, cell.material_slot)
                .is_some()
            {
                return Err(EditError::new(
                    "edit.duplicateCanonicalIdentity",
                    "voxels",
                    format!(
                        "canonical identity {identity:?} repeats occupied coordinate {:?}",
                        cell.coordinate
                    ),
                ));
            }
        }
    }
    Ok(origins)
}

fn part_cells(origins: &CanonicalOriginMap, part: u32) -> BTreeSet<[i64; 3]> {
    origins
        .iter()
        .filter(|((part_id, _), _)| *part_id == part)
        .flat_map(|(_, footprint)| footprint.keys().copied())
        .collect()
}

fn bounds(coordinates: impl IntoIterator<Item = [i64; 3]>) -> Option<([i64; 3], [i64; 3])> {
    let mut coordinates = coordinates.into_iter();
    let first = coordinates.next()?;
    Some(
        coordinates.fold((first, first), |(mut min, mut max), coordinate| {
            for axis in 0..3 {
                min[axis] = min[axis].min(coordinate[axis]);
                max[axis] = max[axis].max(coordinate[axis]);
            }
            (min, max)
        }),
    )
}

fn components(coordinates: &BTreeSet<[i64; 3]>) -> Vec<BTreeSet<[i64; 3]>> {
    let mut remaining = coordinates.clone();
    let mut result = Vec::new();
    while let Some(start) = remaining.pop_first() {
        let mut group = BTreeSet::from([start]);
        let mut frontier = VecDeque::from([start]);
        while let Some(coordinate) = frontier.pop_front() {
            for neighbor in neighbors(coordinate) {
                if remaining.remove(&neighbor) {
                    group.insert(neighbor);
                    frontier.push_back(neighbor);
                }
            }
        }
        result.push(group);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentInputBundle {
    pub frame: FusedFrame,
    pub frame_sha256: String,
    pub canonical_parts: Vec<CanonicalPartSummary>,
    pub metrics: FrameMetrics,
    pub structural_warnings: Vec<StructuralWarning>,
    pub style_rules: Vec<String>,
    pub multiview: Vec<DiagnosticView>,
    pub id_passes: Vec<DiagnosticView>,
    pub difference_overlays: Vec<DifferenceOverlay>,
    pub temporal_window: TemporalWindow,
    pub anchors: BTreeMap<String, [i64; 3]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalPartSummary {
    pub part_id: String,
    pub part_index: u32,
    pub retained_voxels: usize,
    pub bounds: Option<EditBounds>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameMetrics {
    pub occupied_voxels: usize,
    pub generated_or_edited_voxels: usize,
    pub connected_components: usize,
    pub bounds: Option<EditBounds>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StructuralWarning {
    pub code: String,
    pub part_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticViewKind {
    Front,
    Side,
    Top,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticView {
    pub view: DiagnosticViewKind,
    pub pixels: Vec<DiagnosticPixel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticPixel {
    pub coordinate: [i64; 2],
    pub depth: i64,
    pub material_slot: u16,
    pub part_index: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DifferenceKind {
    Removed,
    Added,
    Retained,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DifferenceOverlay {
    pub neighbor: String,
    pub cells: Vec<DifferenceCell>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DifferenceCell {
    pub coordinate: [i64; 3],
    pub kind: DifferenceKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TemporalWindow {
    pub previous: Option<FrameWindowSummary>,
    pub current: FrameWindowSummary,
    pub next: Option<FrameWindowSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrameWindowSummary {
    pub time_microseconds: u64,
    pub duration_microseconds: u64,
    pub frame_sha256: String,
    pub occupied_voxels: usize,
}

pub fn build_agent_input_bundle(
    kit: &VoxelKit,
    previous: Option<&FusedFrame>,
    current: &FusedFrame,
    next: Option<&FusedFrame>,
    style_rules: Vec<String>,
    anchors: BTreeMap<String, [i64; 3]>,
) -> Result<AgentInputBundle, EditError> {
    kit.validate()
        .map_err(|error| EditError::new("edit.invalidKit", "kit", error.to_string()))?;
    let occupied = current
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let origins = canonical_origin_map(current.voxels.iter())?;
    let canonical_parts = kit
        .parts
        .iter()
        .enumerate()
        .map(|(index, part)| {
            let part_index = u32::try_from(index).expect("validated part count fits u32");
            let cells = part_cells(&origins, part_index);
            CanonicalPartSummary {
                part_id: part.id.clone(),
                part_index,
                retained_voxels: cells.len(),
                bounds: edit_bounds(cells.iter().copied()),
            }
        })
        .collect::<Vec<_>>();
    let mut structural_warnings = Vec::new();
    for summary in &canonical_parts {
        let Some(part_bounds) = summary.bounds else {
            structural_warnings.push(StructuralWarning {
                code: "frame.part_missing".to_owned(),
                part_id: Some(summary.part_id.clone()),
                message: "no canonical voxels remain for this part".to_owned(),
            });
            continue;
        };
        let extents = std::array::from_fn::<_, 3, _>(|axis| {
            part_bounds.max[axis] - part_bounds.min[axis] + 1
        });
        if extents.into_iter().min() == Some(1) {
            structural_warnings.push(StructuralWarning {
                code: "frame.part_one_voxel_thick".to_owned(),
                part_id: Some(summary.part_id.clone()),
                message: format!("part has one-voxel thickness within {extents:?}"),
            });
        }
        let part_coordinates = part_cells(&origins, summary.part_index);
        if !part_coordinates.is_empty() && components(&part_coordinates).len() > 1 {
            structural_warnings.push(StructuralWarning {
                code: "frame.part_disconnected".to_owned(),
                part_id: Some(summary.part_id.clone()),
                message: "canonical part occupies multiple face-connected components".to_owned(),
            });
        }
    }
    let multiview = [
        DiagnosticViewKind::Front,
        DiagnosticViewKind::Side,
        DiagnosticViewKind::Top,
    ]
    .into_iter()
    .map(|view| project_view(current, view, false))
    .collect();
    let id_passes = [
        DiagnosticViewKind::Front,
        DiagnosticViewKind::Side,
        DiagnosticViewKind::Top,
    ]
    .into_iter()
    .map(|view| project_view(current, view, true))
    .collect();
    let mut difference_overlays = Vec::new();
    if let Some(previous) = previous {
        difference_overlays.push(difference_overlay("previous", previous, current));
    }
    if let Some(next) = next {
        difference_overlays.push(difference_overlay("next", current, next));
    }
    Ok(AgentInputBundle {
        frame: current.clone(),
        frame_sha256: frame_hash(current)?,
        canonical_parts,
        metrics: FrameMetrics {
            occupied_voxels: current.voxels.len(),
            generated_or_edited_voxels: current
                .voxels
                .iter()
                .filter(|cell| !matches!(cell.origin, FusedVoxelOrigin::Canonical { .. }))
                .count(),
            connected_components: components(&occupied).len(),
            bounds: edit_bounds(occupied.iter().copied()),
        },
        structural_warnings,
        style_rules,
        multiview,
        id_passes,
        difference_overlays,
        temporal_window: TemporalWindow {
            previous: previous.map(frame_window_summary).transpose()?,
            current: frame_window_summary(current)?,
            next: next.map(frame_window_summary).transpose()?,
        },
        anchors,
    })
}

fn project_view(frame: &FusedFrame, view: DiagnosticViewKind, id_pass: bool) -> DiagnosticView {
    let mut pixels = BTreeMap::<[i64; 2], DiagnosticPixel>::new();
    for cell in &frame.voxels {
        let (coordinate, depth) = match view {
            DiagnosticViewKind::Front => {
                ([cell.coordinate[0], cell.coordinate[1]], cell.coordinate[2])
            }
            DiagnosticViewKind::Side => {
                ([cell.coordinate[2], cell.coordinate[1]], cell.coordinate[0])
            }
            DiagnosticViewKind::Top => {
                ([cell.coordinate[0], cell.coordinate[2]], cell.coordinate[1])
            }
        };
        let part_index = match cell.origin {
            FusedVoxelOrigin::Canonical { part_id, .. } => Some(part_id),
            _ => None,
        };
        let candidate = DiagnosticPixel {
            coordinate,
            depth,
            material_slot: cell.material_slot,
            part_index,
        };
        pixels
            .entry(coordinate)
            .and_modify(|existing| {
                let replace = if id_pass {
                    (candidate.part_index, candidate.depth) < (existing.part_index, existing.depth)
                } else {
                    candidate.depth < existing.depth
                };
                if replace {
                    *existing = candidate.clone();
                }
            })
            .or_insert(candidate);
    }
    DiagnosticView {
        view,
        pixels: pixels.into_values().collect(),
    }
}

fn difference_overlay(neighbor: &str, from: &FusedFrame, to: &FusedFrame) -> DifferenceOverlay {
    let from = from
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let to = to
        .voxels
        .iter()
        .map(|cell| cell.coordinate)
        .collect::<BTreeSet<_>>();
    let cells = from
        .union(&to)
        .copied()
        .map(|coordinate| DifferenceCell {
            coordinate,
            kind: match (from.contains(&coordinate), to.contains(&coordinate)) {
                (true, false) => DifferenceKind::Removed,
                (false, true) => DifferenceKind::Added,
                (true, true) => DifferenceKind::Retained,
                (false, false) => unreachable!("coordinate comes from set union"),
            },
        })
        .collect();
    DifferenceOverlay {
        neighbor: neighbor.to_owned(),
        cells,
    }
}

fn frame_window_summary(frame: &FusedFrame) -> Result<FrameWindowSummary, EditError> {
    Ok(FrameWindowSummary {
        time_microseconds: frame.time_microseconds,
        duration_microseconds: frame.duration_microseconds,
        frame_sha256: frame_hash(frame)?,
        occupied_voxels: frame.voxels.len(),
    })
}

fn edit_bounds(coordinates: impl IntoIterator<Item = [i64; 3]>) -> Option<EditBounds> {
    bounds(coordinates).map(|(min, max)| EditBounds { min, max })
}

fn signed_delta(after: usize, before: usize) -> Result<i64, EditError> {
    let after = i64::try_from(after).map_err(|_| {
        EditError::new(
            "edit.metricOverflow",
            "metrics",
            "after metric does not fit i64",
        )
    })?;
    let before = i64::try_from(before).map_err(|_| {
        EditError::new(
            "edit.metricOverflow",
            "metrics",
            "before metric does not fit i64",
        )
    })?;
    Ok(after - before)
}
