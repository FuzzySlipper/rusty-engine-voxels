//! Mesh→kit authoring: bake a complicated static mesh into a canonical
//! exploded voxel kit.
//!
//! The checked exploded kits (rifleman) are hand-authored at ~1,260 cells.
//! This module authors kits from real meshes at two orders of magnitude more
//! detail. Ownership split:
//!
//! - **Engine-owned** (`voxel-convert`): GLB import, per-node mesh selection
//!   (`meshPrimitive: node/N`), triangle voxelization into canonical objects.
//!   Each source node is baked once at the highest cells-per-unit rate that
//!   fits the Engine's conversion caps (256 cells/axis, 16.7M cells/grid).
//! - **Downstream-owned** (this module): *kit composition* — source-node
//!   selection, voxel-space region predicates that split one baked piece into
//!   several parts (legs from an armor piece, arms from a torso), volume-vote
//!   re-rasterization of independently fitted bakes into one shared kit
//!   lattice, pivot/socket placement, and kit validation/assembly evidence.
//!
//! Two documented tolerances:
//!
//! - The Engine derives each bake's Contain fit from that piece's own bounds,
//!   so separately baked pieces use slightly different cells-per-unit rates
//!   (ceil effects at the ≤0.5% level, plus the sword's cap-limited rate).
//!   Re-rasterization into the kit lattice is volume-exact and conservative:
//!   every bake contributes its cells at sub-cell registration error, and the
//!   kit rate is the maximum achieved rate so all re-raster scales are ≥ 1
//!   (pure upsampling, no erosion). rusty-engine #6590 would let the Engine
//!   do this natively with a shared envelope; until then the downstream
//!   re-raster is the supported path.
//! - Socket positions are rounded to integer part-local cells, so the neutral
//!   assembly can differ from the source arrangement by ≤1 cell per mated
//!   part. That error is frozen deterministically into the kit.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use rusty_engine::{voxel_asset, voxel_convert, voxel_object_runtime};
use serde::{Deserialize, Serialize};
use voxel_asset::{
    VoxelAssetMaterialBinding, VoxelAssetMaterialMapping, VoxelConversionFitPolicy,
    VoxelConversionMode, VoxelConversionOriginPolicy, VoxelConversionSettings,
    MAX_REPRESENTED_VOXELS,
};
use voxel_convert::{
    identity_transform, import_mesh_source, plan_static_voxel_object_conversion,
    ConversionMaterialPolicy, ConversionPlanSettings, ImportedMeshSource, ImportedStaticMesh,
    MeshSourceFormat, MeshSourceImportRequest, VoxelObjectConversionPlanRequest,
    VoxelObjectConversionSettings,
};
use voxel_object_runtime::{admit_voxel_object_json, VoxelObjectRuntimeLimits};

use crate::kit::{
    assemble_neutral, AssembledFrame, CoordinateConvention, DeformationBudget, FixedDimension,
    IdentityInvariants, KitCell, KitPart, PaletteGroup, Socket, VoxelKit, KIT_SCHEMA_VERSION,
};
use crate::project::{atomic_write, read_bounded, safe_join, sha256, MAX_SOURCE_BYTES};
use crate::provider_pin::engine_revision;

/// Highest cells-per-unit a piece may bake at: the conversion grid admits at
/// most 256 cells per axis, and resolution is `1 + ceil(span * rate)`.
const MAX_RESOLUTION_AXIS: u32 = 256;
/// Engine conversion grid product cap (`MAX_CONVERSION_CELLS`).
const MAX_GRID_CELLS: u64 = 16_777_216;

// ---------------------------------------------------------------------------
// Spec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitBakeSpec {
    pub schema_version: u32,
    pub kit_id: String,
    pub source: KitBakeSource,
    /// Real-world height of the character in meters, used only to declare the
    /// kit's `voxelSizeMeters`.
    pub character_height_meters: f64,
    /// Source-space Y coordinate that becomes kit ground (`y = 0`).
    pub ground_y_source: f64,
    /// Default bake rate in cells per source unit for every source node. Each
    /// node bakes at `min(this rate, the cap-limited maximum for its span)`.
    /// The kit lattice rate is the maximum *achieved* rate across nodes.
    pub target_cells_per_unit: f64,
    pub palette: Vec<PaletteGroup>,
    /// glTF source material slot → kit palette slot.
    pub material_slots: BTreeMap<u32, u16>,
    pub parts: Vec<KitBakePart>,
    pub sockets: Vec<KitBakeSocket>,
    pub min_limb_thickness: u32,
    #[serde(default)]
    pub protected_parts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitBakeSource {
    pub asset_id: String,
    pub path: String,
    pub expected_source_sha256: String,
    pub license_path: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitBakePart {
    pub id: String,
    pub palette_groups: Vec<String>,
    #[serde(default)]
    pub limb: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symmetry_partner: Option<String>,
    /// Part pivot in source-space world coordinates (the rotation center for
    /// later manual posing). Rounded to the kit lattice in the emitted kit.
    pub pivot_world: [f64; 3],
    pub slices: Vec<KitBakeSlice>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitBakeSlice {
    /// Exact source node name (e.g. `Armor_Material.002_0`).
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<KitBakeRegion>,
    /// Optional kit palette slot override for every cell claimed from this
    /// slice (e.g. give a cloth piece its own color even when it shares the
    /// armor's source material).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kit_slot: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitBakeRegion {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_below: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_at_least: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_below: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_at_least: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_below: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub z_at_least: Option<f64>,
}

impl KitBakeRegion {
    fn contains(&self, point: [f64; 3]) -> bool {
        self.x_below.is_none_or(|v| point[0] < v)
            && self.x_at_least.is_none_or(|v| point[0] >= v)
            && self.y_below.is_none_or(|v| point[1] < v)
            && self.y_at_least.is_none_or(|v| point[1] >= v)
            && self.z_below.is_none_or(|v| point[2] < v)
            && self.z_at_least.is_none_or(|v| point[2] >= v)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitBakeSocket {
    /// Socket id, created on both parts with mates pointing at each other.
    pub id: String,
    /// The two mated part ids.
    pub parts: [String; 2],
    /// Joint center in source-space world coordinates.
    pub world: [f64; 3],
    /// Outward direction from the *first* part toward the second.
    pub forward: [f64; 3],
    /// Joint radius in source units (scaled to kit cells in the emitted kit).
    pub radius_source: f64,
}

// ---------------------------------------------------------------------------
// Evidence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KitBakeEvidence {
    pub schema_version: u32,
    pub engine_revision: String,
    pub kit_id: String,
    pub source_asset_id: String,
    pub source_path: String,
    pub source_sha256: String,
    pub kit_cells_per_unit: f64,
    pub bakes: Vec<BakeEvidence>,
    pub parts: Vec<PartEvidence>,
    pub assembly: AssemblyEvidence,
    pub front_view: Vec<String>,
    pub side_view: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BakeEvidence {
    pub node: String,
    pub node_index: u32,
    pub resolution: [u32; 3],
    pub cells_per_unit: f64,
    pub source_vertices: usize,
    pub source_triangles: usize,
    pub voxelization_work: u64,
    pub voxels: usize,
    pub conversion_microseconds: u128,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartEvidence {
    pub part_id: String,
    pub cells: usize,
    pub pivot_kit: [i64; 3],
    pub bounds_local: Option<[[i64; 3]; 2]>,
    pub discarded_to_earlier_parts: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssemblyEvidence {
    pub fingerprint: u64,
    pub voxels: usize,
    pub bounds: Option<[[i64; 3]; 2]>,
    pub region_unassigned_baked_cells: usize,
    pub source_height_cells: f64,
    pub voxel_size_meters: f64,
}

pub struct KitBakeOutput {
    pub kit: VoxelKit,
    pub kit_json: String,
    pub evidence: KitBakeEvidence,
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

pub fn run_kit_bake(root: &Path, relative_spec: &str) -> Result<KitBakeOutput, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    let spec_path = safe_join(&root, relative_spec)?;
    let spec_text = crate::project::read_bounded_text(&spec_path, 1024 * 1024, "kit bake spec")?;
    let spec: KitBakeSpec = serde_json::from_str(&spec_text)
        .map_err(|error| format!("{}: {error}", spec_path.display()))?;
    spec.validate()?;
    let source_path = safe_join(&root, &spec.source.path)?;
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "kit bake mesh source")?;
    let actual_source_hash = sha256(&source_bytes);
    if actual_source_hash != spec.source.expected_source_sha256 {
        return Err(format!(
            "source identity drift: expected {}, computed {actual_source_hash}",
            spec.source.expected_source_sha256
        ));
    }

    // Import the whole scene once to resolve node names to indices and to
    // measure the model for convention declarations.
    let whole = import_mesh_source(&MeshSourceImportRequest {
        source_asset_id: spec.source.asset_id.clone(),
        asset_version: 1,
        source_path: spec.source.path.clone(),
        format: MeshSourceFormat::Glb,
        source_bytes: source_bytes.clone(),
        expected_source_sha256: Some(actual_source_hash.clone()),
        mesh_primitive: None,
    })
    .map_err(|error| error.to_string())?;
    let node_indices = node_indices(&whole);

    // One bake per unique source node, each at the highest admitted rate.
    let mut bake_order: Vec<String> = Vec::new();
    for part in &spec.parts {
        for slice in &part.slices {
            if !bake_order.contains(&slice.node) {
                bake_order.push(slice.node.clone());
            }
        }
    }
    let mut bakes: BTreeMap<String, NodeBake> = BTreeMap::new();
    let mut bake_evidence: Vec<BakeEvidence> = Vec::new();
    for node in &bake_order {
        let node_index = *node_indices.get(node.as_str()).ok_or_else(|| {
            format!(
                "source has no mesh node named {node:?}; available mesh nodes: {}",
                node_indices
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let (bake, evidence) = bake_node(&spec, node, node_index, &source_bytes)?;
        bakes.insert(node.clone(), bake);
        bake_evidence.push(evidence);
    }

    // The kit lattice rate is the maximum achieved rate, so every re-raster
    // scale is >= 1 (pure upsampling, no erosion).
    let kit_rate = bakes
        .values()
        .map(|bake| bake.cells_per_unit)
        .fold(f64::NEG_INFINITY, f64::max);
    if !(kit_rate.is_finite() && kit_rate > 0.0) {
        return Err("no bake produced a usable cells-per-unit rate".to_owned());
    }
    let kit_origin = [0.0, spec.ground_y_source, 0.0];

    // First claim wins across parts (matches assembly's earlier-part-wins
    // overlap rule). Claims are keyed per baked cell so a later part cannot
    // steal a cell an earlier part already owns.
    let mut claimed: BTreeMap<(&str, [i64; 3]), usize> = BTreeMap::new();
    let mut part_cells: Vec<BTreeMap<[i64; 3], u16>> =
        spec.parts.iter().map(|_| BTreeMap::new()).collect();
    let mut discarded: Vec<usize> = vec![0; spec.parts.len()];
    let mut region_passing = 0usize;
    for (part_index, part) in spec.parts.iter().enumerate() {
        for slice in &part.slices {
            let bake = bakes
                .get(&slice.node)
                .ok_or_else(|| format!("part {}: node {} was not baked", part.id, slice.node))?;
            for cell in &bake.cells {
                let center = bake.source_center(cell.coordinate);
                if !slice.region.is_none_or(|region| region.contains(center)) {
                    continue;
                }
                region_passing += 1;
                if claimed.contains_key(&(slice.node.as_str(), cell.coordinate)) {
                    discarded[part_index] += 1;
                    continue;
                }
                let kit_coordinate = reraster_cell(bake, cell.coordinate, kit_origin, kit_rate);
                let slot = match slice.kit_slot {
                    Some(slot) => slot,
                    None => spec
                        .material_slots
                        .get(&cell.source_material_slot)
                        .copied()
                        .ok_or_else(|| {
                            format!(
                                "source material slot {} has no kit palette mapping",
                                cell.source_material_slot
                            )
                        })?,
                };
                if (0..part_index).any(|earlier| part_cells[earlier].contains_key(&kit_coordinate))
                {
                    discarded[part_index] += 1;
                    continue;
                }
                claimed.insert((slice.node.as_str(), cell.coordinate), part_index);
                part_cells[part_index].entry(kit_coordinate).or_insert(slot);
            }
        }
    }
    let baked_total: usize = bakes.values().map(|bake| bake.cells.len()).sum();

    let mut parts: Vec<KitPart> = Vec::with_capacity(spec.parts.len());
    let mut part_evidence: Vec<PartEvidence> = Vec::with_capacity(spec.parts.len());
    for (part_index, part) in spec.parts.iter().enumerate() {
        let part_pivot_kit = pivot_kit(part.pivot_world, kit_origin, kit_rate);
        let cells: Vec<KitCell> = part_cells[part_index]
            .iter()
            .map(|(coordinate, slot)| KitCell {
                coordinate: [
                    coordinate[0] - part_pivot_kit[0],
                    coordinate[1] - part_pivot_kit[1],
                    coordinate[2] - part_pivot_kit[2],
                ],
                material_slot: *slot,
            })
            .collect();
        let bounds_local = cells.first().map(|first| {
            cells.iter().fold(
                [first.coordinate, first.coordinate],
                |[mut lo, mut hi], cell| {
                    for axis in 0..3 {
                        lo[axis] = lo[axis].min(cell.coordinate[axis]);
                        hi[axis] = hi[axis].max(cell.coordinate[axis]);
                    }
                    [lo, hi]
                },
            )
        });
        parts.push(KitPart {
            id: part.id.clone(),
            version: 1,
            // Cells are stored pivot-relative; the pivot is the local origin.
            pivot: [0, 0, 0],
            sockets: Vec::new(),
            palette_groups: part.palette_groups.clone(),
            limb: part.limb,
            deformation_budget: if part.limb {
                DeformationBudget {
                    max_length_change: 0.1,
                    max_volume_change: 0.1,
                    allow_joint_compression: true,
                }
            } else {
                DeformationBudget {
                    max_length_change: 0.05,
                    max_volume_change: 0.05,
                    allow_joint_compression: false,
                }
            },
            protected_regions: Vec::new(),
            symmetry_partner: part.symmetry_partner.clone(),
            cells,
        });
        part_evidence.push(PartEvidence {
            part_id: part.id.clone(),
            cells: part_cells[part_index].len(),
            pivot_kit: part_pivot_kit,
            bounds_local,
            discarded_to_earlier_parts: discarded[part_index],
        });
    }

    // Sockets: authored world points rounded to part-local integer cells.
    // The first part of a pair is the parent (a free attachment point, no
    // mate); the second declares the mate — this keeps `torso` the single
    // root part the assembly requires.
    let mut required_sockets = Vec::new();
    for socket in &spec.sockets {
        let [parent_id, child_id] = &socket.parts;
        let forward = normalize(socket.forward)
            .ok_or_else(|| format!("socket {}: forward must be non-zero", socket.id))?;
        let radius_cells = socket.radius_source * kit_rate;
        for (part_id, mate, direction) in [
            (parent_id, None, forward),
            (
                child_id,
                Some(format!("{parent_id}.{}", socket.id)),
                [-forward[0], -forward[1], -forward[2]],
            ),
        ] {
            let part = parts
                .iter_mut()
                .find(|part| part.id == *part_id)
                .ok_or_else(|| format!("socket {}: unknown part {part_id}", socket.id))?;
            let spec_part = spec
                .parts
                .iter()
                .find(|part| part.id == *part_id)
                .expect("socket parts were validated");
            let part_pivot_kit = pivot_kit(spec_part.pivot_world, kit_origin, kit_rate);
            let socket_kit: [i64; 3] = std::array::from_fn(|axis| {
                ((socket.world[axis] - kit_origin[axis]) * kit_rate).round() as i64
            });
            part.sockets.push(Socket {
                id: socket.id.clone(),
                position: [
                    (socket_kit[0] - part_pivot_kit[0]) as f64,
                    (socket_kit[1] - part_pivot_kit[1]) as f64,
                    (socket_kit[2] - part_pivot_kit[2]) as f64,
                ],
                forward: direction,
                radius: radius_cells,
                mate,
            });
        }
        required_sockets.push(format!("{parent_id}.{}", socket.id));
        required_sockets.push(format!("{child_id}.{}", socket.id));
    }

    let whole_bounds = mesh_bounds(&whole.mesh)?;
    let source_height_cells = (whole_bounds[1][1] - whole_bounds[0][1]) * kit_rate;
    let voxel_size_meters = spec.character_height_meters / source_height_cells;

    let mut kit = VoxelKit {
        schema_version: KIT_SCHEMA_VERSION,
        id: spec.kit_id.clone(),
        version: 1,
        convention: CoordinateConvention {
            coordinate_system: "right_handed_y_up".to_owned(),
            forward_axis: "-Z".to_owned(),
            voxel_size_meters,
            ground_y: 0,
            neutral_facing: [0, 0, -1],
        },
        palette: spec.palette.clone(),
        parts,
        invariants: IdentityInvariants {
            min_limb_thickness: spec.min_limb_thickness,
            protected_parts: spec.protected_parts.clone(),
            volume_range: None,
            required_sockets,
            fixed_dimensions: Vec::new(),
        },
    };
    kit.validate().map_err(|error| error.to_string())?;
    let assembled = assemble_neutral(&kit).map_err(|error| error.to_string())?;
    let volume = assembled.len() as u64;
    kit.invariants.volume_range = Some([(volume * 4 / 5).max(1), volume + volume / 4 + 1]);
    if let Some((min, max)) = assembled.bounds() {
        let height = max[1] - min[1] + 1;
        let width = max[0] - min[0] + 1;
        kit.invariants.fixed_dimensions = vec![
            FixedDimension {
                subject: "character".to_owned(),
                axis: "height".to_owned(),
                range: [height - 2, height + 2],
            },
            FixedDimension {
                subject: "character".to_owned(),
                axis: "width".to_owned(),
                range: [(width - 4).max(1), width + 4],
            },
        ];
    }
    kit.validate().map_err(|error| error.to_string())?;
    let assembled = assemble_neutral(&kit).map_err(|error| error.to_string())?;

    let kit_json = format!(
        "{}\n",
        serde_json::to_string_pretty(&kit).map_err(|error| error.to_string())?
    );
    let (front_view, side_view) = render_views(&assembled);
    let evidence = KitBakeEvidence {
        schema_version: 1,
        engine_revision: engine_revision()?,
        kit_id: spec.kit_id.clone(),
        source_asset_id: spec.source.asset_id.clone(),
        source_path: spec.source.path.clone(),
        source_sha256: actual_source_hash,
        kit_cells_per_unit: kit_rate,
        bakes: bake_evidence,
        parts: part_evidence,
        assembly: AssemblyEvidence {
            fingerprint: assembled.fingerprint(),
            voxels: assembled.len(),
            bounds: assembled.bounds().map(|(min, max)| [min, max]),
            region_unassigned_baked_cells: baked_total.saturating_sub(region_passing),
            source_height_cells,
            voxel_size_meters,
        },
        front_view,
        side_view,
    };
    Ok(KitBakeOutput {
        kit,
        kit_json,
        evidence,
    })
}

pub fn write_kit_bake_output(
    root: &Path,
    kit_path: &str,
    report_path: &str,
    output: &KitBakeOutput,
) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    atomic_write(&safe_join(&root, kit_path)?, output.kit_json.as_bytes())?;
    let evidence_json = format!(
        "{}\n",
        serde_json::to_string_pretty(&output.evidence).map_err(|error| error.to_string())?
    );
    atomic_write(&safe_join(&root, report_path)?, evidence_json.as_bytes())
}

// ---------------------------------------------------------------------------
// Node baking (Engine-owned conversion)
// ---------------------------------------------------------------------------

struct BakedCell {
    coordinate: [i64; 3],
    source_material_slot: u32,
}

struct NodeBake {
    cells: Vec<BakedCell>,
    /// Kit cell `c` covers source-space cube `[source_lo + c*step, +step)`.
    source_lo: [f64; 3],
    step: f64,
    cells_per_unit: f64,
}

impl NodeBake {
    fn source_center(&self, coordinate: [i64; 3]) -> [f64; 3] {
        std::array::from_fn(|axis| {
            self.source_lo[axis] + (coordinate[axis] as f64 + 0.5) * self.step
        })
    }
}

fn node_indices(imported: &ImportedMeshSource) -> BTreeMap<String, u32> {
    let mut names = BTreeMap::new();
    for node in &imported.scene.nodes {
        if node.source_mesh_index.is_some() {
            if let Some(name) = &node.source_node_name {
                names.insert(name.clone(), node.source_node_index);
            }
        }
    }
    names
}

fn mesh_bounds(mesh: &ImportedStaticMesh) -> Result<[[f64; 3]; 2], String> {
    let first = mesh
        .positions
        .first()
        .ok_or("imported mesh has no positions")?;
    let mut lo = *first;
    let mut hi = *first;
    for position in &mesh.positions {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(position[axis]);
            hi[axis] = hi[axis].max(position[axis]);
        }
    }
    Ok([lo, hi])
}

fn bake_node(
    spec: &KitBakeSpec,
    node: &str,
    node_index: u32,
    source_bytes: &[u8],
) -> Result<(NodeBake, BakeEvidence), String> {
    let imported = import_mesh_source(&MeshSourceImportRequest {
        source_asset_id: spec.source.asset_id.clone(),
        asset_version: 1,
        source_path: spec.source.path.clone(),
        format: MeshSourceFormat::Glb,
        source_bytes: source_bytes.to_vec(),
        expected_source_sha256: Some(spec.source.expected_source_sha256.clone()),
        mesh_primitive: Some(format!("node/{node_index}")),
    })
    .map_err(|error| error.to_string())?;
    let [lo, hi] = mesh_bounds(&imported.mesh)?;
    let span: [f64; 3] = std::array::from_fn(|axis| hi[axis] - lo[axis]);
    let max_span = span.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if !(max_span > 0.0 && max_span.is_finite()) {
        return Err(format!("node {node}: degenerate source bounds"));
    }
    let cap_rate = f64::from(MAX_RESOLUTION_AXIS - 1) / max_span;
    let target_rate = spec.target_cells_per_unit.min(cap_rate);
    let resolution: [u32; 3] =
        std::array::from_fn(|axis| 1 + (span[axis] * target_rate).ceil() as u32);
    if resolution.iter().any(|axis| *axis > MAX_RESOLUTION_AXIS) {
        return Err(format!(
            "node {node}: resolution {resolution:?} exceeds the {MAX_RESOLUTION_AXIS}-axis cap"
        ));
    }
    let grid_cells = resolution
        .iter()
        .map(|axis| u64::from(*axis))
        .product::<u64>();
    if grid_cells > MAX_GRID_CELLS {
        return Err(format!(
            "node {node}: grid {resolution:?} exceeds the {MAX_GRID_CELLS}-cell cap"
        ));
    }
    // cells_per_unit = min over axes of (res_a - 1) / span_a (cell size cancels).
    let cells_per_unit = span
        .iter()
        .zip(resolution.iter())
        .map(|(span_axis, resolution_axis)| {
            if *span_axis > f64::EPSILON {
                f64::from(resolution_axis - 1) / span_axis
            } else {
                f64::INFINITY
            }
        })
        .fold(f64::INFINITY, f64::min);
    if !(cells_per_unit.is_finite() && cells_per_unit > 0.0) {
        return Err(format!("node {node}: could not derive a finite bake rate"));
    }
    let cell_size = 1.0 / cells_per_unit;
    let materials = bake_materials(&spec.kit_id, &imported);
    let max_output_voxels = resolution
        .into_iter()
        .try_fold(1_u32, u32::checked_mul)
        .ok_or_else(|| format!("node {node}: resolution product overflows u32"))?
        .min(MAX_REPRESENTED_VOXELS as u32);
    let node_slug: String = node
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let request = VoxelObjectConversionPlanRequest {
        source: imported.receipt.source.clone(),
        source_path: spec.source.path.clone(),
        target_asset_id: format!("voxel-object/{}-bake-{node_slug}", spec.kit_id),
        license_path: Some(spec.source.license_path.clone()),
        settings: VoxelObjectConversionSettings {
            mesh: ConversionPlanSettings {
                conversion: VoxelConversionSettings {
                    resolution,
                    cell_size,
                    chunk_size: 16,
                    origin: [0, 0, 0],
                    fit_policy: VoxelConversionFitPolicy::Contain,
                    origin_policy: VoxelConversionOriginPolicy::Centered,
                    mode: VoxelConversionMode::Surface,
                    material_palette: materials.palette,
                    material_map: materials.mappings,
                    max_output_voxels,
                },
                transform: identity_transform(),
                material_policy: ConversionMaterialPolicy::default(),
            },
            source_bounds: None,
            pivot: [0.0, 0.0, 0.0],
            anchor_policy: voxel_convert::AnimationAnchorPolicy::PreserveSourceSpace,
        },
        clips: Vec::new(),
        default_clip: None,
    };
    let started = Instant::now();
    let prepared = plan_static_voxel_object_conversion(&request, &imported)
        .map_err(|error| format!("node {node}: {error}"))?;
    let conversion_microseconds = started.elapsed().as_micros();
    let candidate = prepared.candidate();
    let object = admit_voxel_object_json(
        &candidate.canonical_json,
        VoxelObjectRuntimeLimits::default(),
    )
    .map_err(|error| format!("node {node}: admission failed: {error}"))?;
    let frame = object
        .frames()
        .first()
        .ok_or_else(|| format!("node {node}: admitted object has no frames"))?;
    // Reverse the Engine's Centered/Contain mapping for this bake: the same
    // inputs (piece bounds, resolution, cell size) determine the map.
    let scale = resolution
        .iter()
        .zip(span.iter())
        .map(|(resolution_axis, span_axis)| {
            if *span_axis > f64::EPSILON {
                f64::from(resolution_axis - 1) * cell_size / span_axis
            } else {
                f64::INFINITY
            }
        })
        .fold(f64::INFINITY, f64::min);
    if !(scale.is_finite() && scale > 0.0) {
        return Err(format!("node {node}: could not derive a finite fit scale"));
    }
    let step = cell_size / scale;
    let source_lo: [f64; 3] = std::array::from_fn(|axis| {
        let target_span = f64::from(resolution[axis] - 1) * cell_size;
        let offset_cells = ((target_span - span[axis] * scale) / 2.0).max(0.0) / cell_size;
        lo[axis] - offset_cells * step
    });
    let cells: Vec<BakedCell> = frame
        .cells
        .iter()
        .map(|cell| {
            let slot_index = usize::from(cell.material_slot)
                .checked_sub(1)
                .ok_or_else(|| format!("node {node}: reserved material slot 0 in bake"))?;
            let material = imported
                .mesh
                .materials
                .get(slot_index)
                .ok_or_else(|| format!("node {node}: bake cell references unknown slot"))?;
            Ok(BakedCell {
                coordinate: cell.coordinate,
                source_material_slot: material.source_material_slot,
            })
        })
        .collect::<Result<_, String>>()?;
    let evidence = BakeEvidence {
        node: node.to_owned(),
        node_index,
        resolution,
        cells_per_unit,
        source_vertices: candidate.source_vertices,
        source_triangles: candidate.source_triangles,
        voxelization_work: candidate.voxelization_work,
        voxels: candidate.aggregate_voxels,
        conversion_microseconds,
    };
    Ok((
        NodeBake {
            cells,
            source_lo,
            step,
            cells_per_unit,
        },
        evidence,
    ))
}

struct BakeMaterials {
    palette: Vec<VoxelAssetMaterialBinding>,
    mappings: Vec<VoxelAssetMaterialMapping>,
}

fn bake_materials(kit_id: &str, imported: &ImportedMeshSource) -> BakeMaterials {
    imported
        .mesh
        .materials
        .iter()
        .enumerate()
        .map(|(index, material)| {
            let material_slot = u16::try_from(index + 1)
                .expect("kit bake sources have far fewer than u16::MAX materials");
            (
                VoxelAssetMaterialBinding {
                    material_slot,
                    material_asset_id: format!(
                        "material/{kit_id}-slot-{}",
                        material.source_material_slot
                    ),
                    display_name: material.source_material_name.clone(),
                },
                VoxelAssetMaterialMapping {
                    source_material_slot: material.source_material_slot,
                    source_material_name: material.source_material_name.clone(),
                    voxel_material_slot: material_slot,
                },
            )
        })
        .fold(
            BakeMaterials {
                palette: Vec::new(),
                mappings: Vec::new(),
            },
            |mut materials, (binding, mapping)| {
                materials.palette.push(binding);
                materials.mappings.push(mapping);
                materials
            },
        )
}

// ---------------------------------------------------------------------------
// Re-raster (bake lattice → shared kit lattice)
// ---------------------------------------------------------------------------

/// Map one baked cell into the kit lattice by volume-argmax per axis: the
/// source-space cube `[lo, lo+step)` projects to the kit cell with the
/// largest overlap. Deterministic (lowest coordinate wins ties).
fn reraster_cell(
    bake: &NodeBake,
    coordinate: [i64; 3],
    kit_origin: [f64; 3],
    kit_rate: f64,
) -> [i64; 3] {
    std::array::from_fn(|axis| {
        let lo = (bake.source_lo[axis] + coordinate[axis] as f64 * bake.step - kit_origin[axis])
            * kit_rate;
        let hi = lo + bake.step * kit_rate;
        let first = lo.floor() as i64 - 1;
        let mut best = first;
        let mut best_overlap = -1.0f64;
        for candidate in first..=(hi.ceil() as i64 + 1) {
            let overlap = (hi.min(candidate as f64 + 1.0) - lo.max(candidate as f64)).max(0.0);
            if overlap > best_overlap {
                best_overlap = overlap;
                best = candidate;
            }
        }
        best
    })
}

fn pivot_kit(pivot_world: [f64; 3], kit_origin: [f64; 3], kit_rate: f64) -> [i64; 3] {
    std::array::from_fn(|axis| ((pivot_world[axis] - kit_origin[axis]) * kit_rate).round() as i64)
}

fn normalize(vector: [f64; 3]) -> Option<[f64; 3]> {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length < 1e-9 {
        return None;
    }
    Some([vector[0] / length, vector[1] / length, vector[2] / length])
}

// ---------------------------------------------------------------------------
// ASCII multiview renders for review
// ---------------------------------------------------------------------------

fn render_views(frame: &AssembledFrame) -> (Vec<String>, Vec<String>) {
    let Some((min, max)) = frame.bounds() else {
        return (Vec::new(), Vec::new());
    };
    let glyph = |slot: u16| -> char {
        match slot {
            1 => '#',
            2 => 'H',
            3 => 'S',
            4 => 'h',
            5 => 'W',
            6 => 'C',
            _ => '+',
        }
    };
    let project = |outer: usize| -> Vec<String> {
        let width = (max[outer] - min[outer] + 1) as usize;
        let height = (max[1] - min[1] + 1) as usize;
        let scale = (80.0 / width.max(1) as f64).min(40.0 / height.max(1) as f64);
        let out_w = ((width as f64 * scale).ceil() as usize).max(1);
        let out_h = ((height as f64 * scale).ceil() as usize).max(1);
        let mut grid = vec![vec![' '; out_w]; out_h];
        for (coordinate, voxel) in &frame.voxels {
            let gx = ((coordinate[outer] - min[outer]) as f64 * scale) as usize;
            let gy = ((max[1] - coordinate[1]) as f64 * scale) as usize;
            if gx < out_w && gy < out_h {
                grid[gy][gx] = glyph(voxel.material_slot);
            }
        }
        grid.into_iter()
            .map(|row| row.into_iter().collect::<String>())
            .collect()
    };
    (project(0), project(2))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl KitBakeSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "kit bake spec schema {} is unsupported; expected 1",
                self.schema_version
            ));
        }
        if self.kit_id.is_empty()
            || !self
                .kit_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err("kitId must be a non-empty identity".to_owned());
        }
        if !self.source.asset_id.starts_with("mesh/") {
            return Err("source.assetId must be a mesh/... static mesh identity".to_owned());
        }
        for (field, value) in [
            ("source.path", self.source.path.as_str()),
            ("source.licensePath", self.source.license_path.as_str()),
        ] {
            let path = Path::new(value);
            if value.is_empty()
                || path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(format!("{field} must be a safe relative path"));
            }
        }
        if !self
            .source
            .expected_source_sha256
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        {
            return Err("source.expectedSourceSha256 is not a canonical SHA-256".to_owned());
        }
        if !self.character_height_meters.is_finite()
            || self.character_height_meters <= 0.0
            || !self.ground_y_source.is_finite()
            || !self.target_cells_per_unit.is_finite()
            || self.target_cells_per_unit <= 0.0
        {
            return Err(
                "character height, ground, and target rate must be positive finite".to_owned(),
            );
        }
        if self.palette.is_empty() {
            return Err("palette must not be empty".to_owned());
        }
        let palette_slots: BTreeSet<u16> = self
            .palette
            .iter()
            .flat_map(|group| group.slots.iter().map(|slot| slot.slot))
            .collect();
        for (source_slot, kit_slot) in &self.material_slots {
            if !palette_slots.contains(kit_slot) {
                return Err(format!(
                    "source material slot {source_slot} maps to unknown kit slot {kit_slot}"
                ));
            }
        }
        if self.parts.is_empty() {
            return Err("parts must not be empty".to_owned());
        }
        let palette_groups: BTreeSet<&str> = self
            .palette
            .iter()
            .map(|group| group.name.as_str())
            .collect();
        let mut part_ids = BTreeSet::new();
        for part in &self.parts {
            if part.id.is_empty()
                || !part
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!("part id {:?} is not an identity", part.id));
            }
            if !part_ids.insert(part.id.as_str()) {
                return Err(format!("duplicate part id {}", part.id));
            }
            if part.slices.is_empty() {
                return Err(format!("part {} has no slices", part.id));
            }
            for group in &part.palette_groups {
                if !palette_groups.contains(group.as_str()) {
                    return Err(format!(
                        "part {} references unknown palette group {group}",
                        part.id
                    ));
                }
            }
            if !part.pivot_world.iter().all(|v| v.is_finite()) {
                return Err(format!("part {}: pivotWorld must be finite", part.id));
            }
            for slice in &part.slices {
                if slice.node.is_empty() {
                    return Err(format!("part {} has an empty slice node", part.id));
                }
                if let Some(slot) = slice.kit_slot {
                    if !palette_slots.contains(&slot) {
                        return Err(format!(
                            "part {} slice {}: kitSlot {slot} is not in the palette",
                            part.id, slice.node
                        ));
                    }
                }
                if let Some(region) = slice.region {
                    for (lower, upper) in [
                        (region.x_at_least, region.x_below),
                        (region.y_at_least, region.y_below),
                        (region.z_at_least, region.z_below),
                    ] {
                        if let (Some(lower), Some(upper)) = (lower, upper) {
                            if lower >= upper {
                                return Err(format!(
                                    "part {} slice {}: region lower bound must be < upper bound",
                                    part.id, slice.node
                                ));
                            }
                        }
                    }
                }
            }
        }
        for socket in &self.sockets {
            if socket.id.is_empty()
                || !socket
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!("socket id {:?} is not an identity", socket.id));
            }
            for part_id in &socket.parts {
                if !part_ids.contains(part_id.as_str()) {
                    return Err(format!(
                        "socket {} references unknown part {part_id}",
                        socket.id
                    ));
                }
            }
            if socket.parts[0] == socket.parts[1] {
                return Err(format!("socket {} mates a part to itself", socket.id));
            }
            if !socket.world.iter().all(|v| v.is_finite())
                || !socket.forward.iter().all(|v| v.is_finite())
                || !socket.radius_source.is_finite()
                || socket.radius_source <= 0.0
            {
                return Err(format!("socket {} has invalid geometry", socket.id));
            }
        }
        if self.min_limb_thickness == 0 {
            return Err("minLimbThickness must be >= 1".to_owned());
        }
        for part_id in &self.protected_parts {
            if !part_ids.contains(part_id.as_str()) {
                return Err(format!("protectedParts references unknown part {part_id}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_spec() -> KitBakeSpec {
        KitBakeSpec {
            schema_version: 1,
            kit_id: "unit-kit".to_owned(),
            source: KitBakeSource {
                asset_id: "mesh/unit".to_owned(),
                path: "content/sources/unit/unit.glb".to_owned(),
                expected_source_sha256: format!("sha256:{}", "0".repeat(64)),
                license_path: "content/sources/unit/LICENSE.txt".to_owned(),
            },
            character_height_meters: 1.8,
            ground_y_source: 0.0,
            target_cells_per_unit: 2.0,
            palette: vec![PaletteGroup {
                name: "body".to_owned(),
                slots: vec![crate::kit::PaletteSlot {
                    slot: 1,
                    display_name: "body".to_owned(),
                    color: [0.5, 0.5, 0.5, 1.0],
                }],
            }],
            material_slots: BTreeMap::from([(0, 1)]),
            parts: vec![KitBakePart {
                id: "torso".to_owned(),
                palette_groups: vec!["body".to_owned()],
                limb: false,
                symmetry_partner: None,
                pivot_world: [0.0, 1.0, 0.0],
                slices: vec![KitBakeSlice {
                    node: "Torso_0".to_owned(),
                    region: None,
                    kit_slot: None,
                }],
            }],
            sockets: Vec::new(),
            min_limb_thickness: 2,
            protected_parts: Vec::new(),
        }
    }

    #[test]
    fn valid_spec_passes() {
        valid_spec().validate().expect("valid spec");
    }

    #[test]
    fn animated_source_identity_is_rejected() {
        let mut spec = valid_spec();
        spec.source.asset_id = "mesh-animation/unit".to_owned();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn inverted_region_bounds_are_rejected() {
        let mut spec = valid_spec();
        spec.parts[0].slices[0].region = Some(KitBakeRegion {
            y_at_least: Some(5.0),
            y_below: Some(4.0),
            ..KitBakeRegion::default()
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn self_mating_socket_is_rejected() {
        let mut spec = valid_spec();
        spec.sockets.push(KitBakeSocket {
            id: "neck".to_owned(),
            parts: ["torso".to_owned(), "torso".to_owned()],
            world: [0.0, 1.0, 0.0],
            forward: [0.0, 1.0, 0.0],
            radius_source: 1.0,
        });
        assert!(spec.validate().is_err());
    }

    #[test]
    fn reraster_upsampling_is_connected() {
        let bake = NodeBake {
            cells: Vec::new(),
            source_lo: [0.0, 0.0, 0.0],
            step: 1.0,
            cells_per_unit: 1.0,
        };
        // Upsampling 1.0 → 1.4: a run of adjacent source cells must stay
        // adjacent in the kit lattice (no gaps, no tears).
        let mapped: Vec<i64> = (0..8)
            .map(|x| reraster_cell(&bake, [x, 0, 0], [0.0, 0.0, 0.0], 1.4)[0])
            .collect();
        for window in mapped.windows(2) {
            assert!(window[1] - window[0] >= 1, "run must not collapse");
            assert!(window[1] - window[0] <= 2, "run must not tear: {mapped:?}");
        }
    }

    #[test]
    fn region_predicate_bounds() {
        let region = KitBakeRegion {
            y_below: Some(-45.0),
            ..KitBakeRegion::default()
        };
        assert!(region.contains([0.0, -46.0, 0.0]));
        assert!(!region.contains([0.0, -45.0, 0.0]));
        assert!(!region.contains([0.0, 0.0, 0.0]));
    }
}
