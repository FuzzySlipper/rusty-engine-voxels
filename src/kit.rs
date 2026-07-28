//! Canonical exploded voxel kit: part format, character format, socket
//! assembly, and provenance for the baked character animation pipeline.
//!
//! A canonical kit is the *source of truth* for a character's identity. It is
//! authored once as a set of rigid voxel parts (each a stable integer cell set
//! with a pivot), plus the palette and the identity invariants that every
//! downstream frame must respect. A pose is later produced by rigidly
//! transforming these stable parts (M2) and fusing their joints (M3); those
//! stages consume the socket/pivot semantics defined here.
//!
//! This module owns only the *authoring intent* — the kit/part formats,
//! validation, deterministic neutral assembly, and provenance. It deliberately
//! does not reproduce engine conversion/runtime semantics; the assembled frame
//! is a plain coordinate→material map that later milestones feed into the
//! existing engine voxel-object format.
//!
//! Everything here is deterministic: the same kit always assembles to the same
//! neutral frame, byte-for-byte.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Current canonical kit schema version. Bump on any breaking format change.
pub const KIT_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Identity types
// ---------------------------------------------------------------------------

/// Stable identity of one voxel within a part, used for provenance. The
/// `voxel_index` is the cell's index in the part's canonical (sorted) cell
/// list, so it is stable across edits that do not reorder cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalVoxelId {
    pub part_index: u32,
    pub voxel_index: u32,
}

/// Where an assembled voxel came from. Today every assembled voxel is
/// canonical (M1); joint-bridge and cleanup origins are added by later
/// milestones and recorded here so provenance stays a closed enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VoxelOrigin {
    Canonical(CanonicalVoxelId),
}

// ---------------------------------------------------------------------------
// Sockets
// ---------------------------------------------------------------------------

/// A named attachment point on a part, in part-local cell coordinates. Two
/// parts are joined by mating one socket on each; assembly translates (and in
/// later milestones orients) the child so the mates coincide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Socket {
    pub id: String,
    /// Part-local position of the socket center, in cells (fractional allowed
    /// so a socket can sit at a face/edge midpoint).
    pub position: [f64; 3],
    /// Outward direction the socket faces, used for orientation during posing.
    pub forward: [f64; 3],
    /// Approximate radius of the joint this socket forms, in cells.
    pub radius: f64,
    /// Optional explicit mate: `<partId>.<socketId>`. When present, assembly
    /// resolves this part relative to the named mate. When absent, the socket
    /// is a free attachment point (e.g. a hand for equipment).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mate: Option<String>,
}

impl Socket {
    fn validate(&self, part_id: &str) -> Result<(), KitError> {
        require_identity(&self.id, "socket.id")
            .map_err(|m| KitError::validation(format!("part {part_id}: {m}")))?;
        for (name, value) in [
            ("position", self.position.as_slice()),
            ("forward", self.forward.as_slice()),
        ] {
            if !value.iter().all(|v| v.is_finite()) {
                return Err(KitError::validation(format!(
                    "part {part_id} socket {}: {name} must be finite",
                    self.id
                )));
            }
        }
        let len =
            (self.forward[0].powi(2) + self.forward[1].powi(2) + self.forward[2].powi(2)).sqrt();
        if len < 1e-6 {
            return Err(KitError::validation(format!(
                "part {part_id} socket {}: forward must be a non-zero direction",
                self.id
            )));
        }
        if !self.radius.is_finite() || self.radius <= 0.0 {
            return Err(KitError::validation(format!(
                "part {part_id} socket {}: radius must be positive",
                self.id
            )));
        }
        if let Some(mate) = &self.mate {
            parse_mate(mate).map_err(|m| {
                KitError::validation(format!("part {part_id} socket {}: {m}", self.id))
            })?;
        }
        Ok(())
    }
}

/// Parse a `<partId>.<socketId>` mate reference into its two identities.
fn parse_mate(mate: &str) -> Result<(&str, &str), String> {
    let (part, socket) = mate
        .split_once('.')
        .ok_or_else(|| format!("mate {mate:?} must be <partId>.<socketId>"))?;
    if part.is_empty() || socket.is_empty() || socket.contains('.') {
        return Err(format!("mate {mate:?} must be <partId>.<socketId>"));
    }
    Ok((part, socket))
}

// ---------------------------------------------------------------------------
// Part
// ---------------------------------------------------------------------------

/// One rigid voxel part: a stable integer cell set plus a pivot and sockets.
/// Cells are stored sorted (lexicographic by coordinate) so the voxel index is
/// a stable provenance key and the part serializes deterministically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitPart {
    pub id: String,
    pub version: u32,
    /// Local origin of this part's primary parent joint, in part-local cells.
    pub pivot: [i64; 3],
    /// Occupied cells in part-local coordinates, sorted lexicographically.
    pub cells: Vec<KitCell>,
    pub sockets: Vec<Socket>,
    /// Palette group names this part draws from (must exist in the kit).
    pub palette_groups: Vec<String>,
    /// Optional mirror part for symmetry tooling (e.g. right_lower_arm).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symmetry_partner: Option<String>,
}

/// One occupied cell in a part, with its material slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KitCell {
    pub coordinate: [i64; 3],
    pub material_slot: u16,
}

impl KitPart {
    fn validate(&self, palette_groups: &BTreeSet<&str>) -> Result<(), KitError> {
        require_identity(&self.id, "part.id").map_err(KitError::validation)?;
        if self.version == 0 {
            return Err(KitError::validation(format!(
                "part {}: version must be >= 1",
                self.id
            )));
        }
        if self.cells.is_empty() {
            return Err(KitError::validation(format!(
                "part {}: must occupy at least one cell",
                self.id
            )));
        }
        // Cells must be sorted and deduplicated for stable provenance.
        for window in self.cells.windows(2) {
            if window[0].coordinate >= window[1].coordinate {
                return Err(KitError::validation(format!(
                    "part {}: cells must be sorted and deduplicated (out of order at {:?})",
                    self.id, window[1].coordinate
                )));
            }
        }
        for cell in &self.cells {
            if cell.material_slot == 0 {
                return Err(KitError::validation(format!(
                    "part {}: cell {:?} uses reserved material slot 0",
                    self.id, cell.coordinate
                )));
            }
        }
        let mut socket_ids = BTreeSet::new();
        for socket in &self.sockets {
            socket.validate(&self.id)?;
            if !socket_ids.insert(socket.id.as_str()) {
                return Err(KitError::validation(format!(
                    "part {}: duplicate socket id {}",
                    self.id, socket.id
                )));
            }
        }
        for group in &self.palette_groups {
            if !palette_groups.contains(group.as_str()) {
                return Err(KitError::validation(format!(
                    "part {}: references unknown palette group {group}",
                    self.id
                )));
            }
        }
        if let Some(partner) = &self.symmetry_partner {
            require_identity(partner, "part.symmetryPartner").map_err(KitError::validation)?;
        }
        Ok(())
    }

    pub fn socket(&self, id: &str) -> Option<&Socket> {
        self.sockets.iter().find(|socket| socket.id == id)
    }
}

// ---------------------------------------------------------------------------
// Identity invariants
// ---------------------------------------------------------------------------

/// Identity rules every downstream frame must respect. These are the
/// machine-readable form of "what makes this character itself."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityInvariants {
    /// Minimum thickness (in cells) any limb part must retain anywhere.
    pub min_limb_thickness: u32,
    /// Parts that may not be removed or have their protected regions carved.
    #[serde(default)]
    pub protected_parts: Vec<String>,
    /// Approximate allowed volume range (in cells) for the whole character.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_range: Option<[u64; 2]>,
    /// Sockets that must remain present and usable across all frames.
    #[serde(default)]
    pub required_sockets: Vec<String>,
}

impl IdentityInvariants {
    fn validate(&self) -> Result<(), KitError> {
        if self.min_limb_thickness == 0 {
            return Err(KitError::validation(
                "invariants.minLimbThickness must be >= 1".to_owned(),
            ));
        }
        if let Some([lo, hi]) = self.volume_range {
            if lo > hi {
                return Err(KitError::validation(
                    "invariants.volumeRange must be [min, max]".to_owned(),
                ));
            }
        }
        for socket in &self.required_sockets {
            parse_mate(socket).map_err(KitError::validation)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaletteGroup {
    pub name: String,
    /// Material slots in this group, by slot id.
    pub slots: Vec<PaletteSlot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PaletteSlot {
    pub slot: u16,
    pub display_name: String,
    pub color: [f32; 4],
}

// ---------------------------------------------------------------------------
// Kit
// ---------------------------------------------------------------------------

/// Coordinate/scale declaration, enforced across all parts and frames.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoordinateConvention {
    pub coordinate_system: String,
    pub forward_axis: String,
    pub voxel_size_meters: f64,
    pub ground_y: i64,
    pub neutral_facing: [i64; 3],
}

impl CoordinateConvention {
    fn validate(&self) -> Result<(), KitError> {
        if self.coordinate_system != "right_handed_y_up" {
            return Err(KitError::validation(format!(
                "coordinateSystem must be right_handed_y_up, got {}",
                self.coordinate_system
            )));
        }
        if self.forward_axis != "-Z" {
            return Err(KitError::validation(format!(
                "forwardAxis must be -Z, got {}",
                self.forward_axis
            )));
        }
        if !self.voxel_size_meters.is_finite() || self.voxel_size_meters <= 0.0 {
            return Err(KitError::validation(
                "voxelSizeMeters must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

/// A canonical exploded character kit: identity source of truth.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoxelKit {
    pub schema_version: u32,
    pub id: String,
    pub version: u32,
    pub convention: CoordinateConvention,
    pub palette: Vec<PaletteGroup>,
    pub parts: Vec<KitPart>,
    pub invariants: IdentityInvariants,
}

impl VoxelKit {
    pub fn validate(&self) -> Result<(), KitError> {
        if self.schema_version != KIT_SCHEMA_VERSION {
            return Err(KitError::validation(format!(
                "kit schema {} is unsupported; expected {KIT_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        require_identity(&self.id, "kit.id").map_err(KitError::validation)?;
        if self.version == 0 {
            return Err(KitError::validation("kit.version must be >= 1".to_owned()));
        }
        self.convention.validate()?;
        self.invariants.validate()?;

        // Palette: unique group names, unique slot ids across the kit.
        let mut group_names = BTreeSet::new();
        let mut slot_ids = BTreeSet::new();
        for group in &self.palette {
            require_identity(&group.name, "palette.name").map_err(KitError::validation)?;
            if !group_names.insert(group.name.as_str()) {
                return Err(KitError::validation(format!(
                    "duplicate palette group {}",
                    group.name
                )));
            }
            for slot in &group.slots {
                if slot.slot == 0 {
                    return Err(KitError::validation(format!(
                        "palette group {} uses reserved slot 0",
                        group.name
                    )));
                }
                if !slot_ids.insert(slot.slot) {
                    return Err(KitError::validation(format!(
                        "duplicate material slot {} across palette",
                        slot.slot
                    )));
                }
                if !slot
                    .color
                    .iter()
                    .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
                {
                    return Err(KitError::validation(format!(
                        "palette slot {} color out of range",
                        slot.slot
                    )));
                }
            }
        }
        let group_set: BTreeSet<&str> = group_names.iter().map(|s| &**s).collect();

        // Parts: unique ids, valid references.
        let mut part_ids = BTreeSet::new();
        for part in &self.parts {
            part.validate(&group_set)?;
            if !part_ids.insert(part.id.as_str()) {
                return Err(KitError::validation(format!(
                    "duplicate part id {}",
                    part.id
                )));
            }
        }
        if self.parts.is_empty() {
            return Err(KitError::validation(
                "kit must contain at least one part".to_owned(),
            ));
        }

        // Cross-reference checks: symmetry partners, mates, required sockets,
        // and cell material slots must all resolve.
        for part in &self.parts {
            if let Some(partner) = &part.symmetry_partner {
                if !part_ids.contains(partner.as_str()) {
                    return Err(KitError::validation(format!(
                        "part {}: symmetry partner {partner} is not a part",
                        part.id
                    )));
                }
            }
            for cell in &part.cells {
                if !slot_ids.contains(&cell.material_slot) {
                    return Err(KitError::validation(format!(
                        "part {}: cell {:?} uses unknown material slot {}",
                        part.id, cell.coordinate, cell.material_slot
                    )));
                }
            }
            for socket in &part.sockets {
                if let Some(mate) = &socket.mate {
                    let (mate_part, mate_socket) =
                        parse_mate(mate).map_err(KitError::validation)?;
                    let target =
                        self.parts
                            .iter()
                            .find(|p| p.id == mate_part)
                            .ok_or_else(|| {
                                KitError::validation(format!(
                                    "part {} socket {}: mate part {mate_part} is not a part",
                                    part.id, socket.id
                                ))
                            })?;
                    if target.socket(mate_socket).is_none() {
                        return Err(KitError::validation(format!(
                            "part {} socket {}: mate socket {mate_socket} missing on {mate_part}",
                            part.id, socket.id
                        )));
                    }
                }
            }
        }
        // Required sockets must exist somewhere and be mated consistently.
        for required in &self.invariants.required_sockets {
            let (part_id, socket_id) = parse_mate(required).map_err(KitError::validation)?;
            let part = self.parts.iter().find(|p| p.id == part_id).ok_or_else(|| {
                KitError::validation(format!(
                    "required socket {required}: part {part_id} is not a part"
                ))
            })?;
            if part.socket(socket_id).is_none() {
                return Err(KitError::validation(format!(
                    "required socket {required}: socket {socket_id} missing on {part_id}"
                )));
            }
        }
        Ok(())
    }

    pub fn part(&self, id: &str) -> Option<&KitPart> {
        self.parts.iter().find(|part| part.id == id)
    }

    /// Total occupied canonical cells across all parts.
    pub fn total_cells(&self) -> usize {
        self.parts.iter().map(|part| part.cells.len()).sum()
    }
}

// ---------------------------------------------------------------------------
// Neutral assembly
// ---------------------------------------------------------------------------

/// One voxel in the assembled neutral frame, with its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssembledVoxel {
    pub coordinate: [i64; 3],
    pub material_slot: u16,
    pub origin: VoxelOrigin,
}

/// The assembled neutral character: a deterministic coordinate→voxel map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledFrame {
    pub voxels: BTreeMap<[i64; 3], AssembledVoxel>,
}

impl AssembledFrame {
    pub fn len(&self) -> usize {
        self.voxels.len()
    }
    pub fn is_empty(&self) -> bool {
        self.voxels.is_empty()
    }
    pub fn bounds(&self) -> Option<([i64; 3], [i64; 3])> {
        let mut iter = self.voxels.keys();
        let first = *iter.next()?;
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
    /// Deterministic content fingerprint for regeneration-stability tests.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for (coord, voxel) in &self.voxels {
            for component in *coord {
                hash ^= component as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
            hash ^= u64::from(voxel.material_slot);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// Deterministically assemble the neutral character from the kit.
///
/// Assembly walks parts in dependency order (a part is placed once its mate's
/// part is placed). Each part with a mate is translated so its mating socket
/// coincides with its mate's socket; the root part(s) (no mate) are placed at
/// their pivot relative to the ground plane. Overlaps resolve by part order
/// (earlier parts win) and are recorded for diagnostics.
///
/// Because M1 has no posing yet, assembly is translation-only: sockets mate by
/// aligning their positions, with orientation handled by later milestones.
pub fn assemble_neutral(kit: &VoxelKit) -> Result<AssembledFrame, KitError> {
    kit.validate()?;

    // Resolve placement order via mate dependencies (parts referenced by a
    // mate must be placed before the part that mates to them).
    let order = placement_order(kit)?;

    // placed_part -> map of socketId -> world position of that socket.
    let mut placed_sockets: BTreeMap<usize, BTreeMap<String, [f64; 3]>> = BTreeMap::new();
    // part_index -> world translation applied to its local coordinates.
    let mut translations: BTreeMap<usize, [i64; 3]> = BTreeMap::new();

    for &part_index in &order {
        let part = &kit.parts[part_index];
        let translation = resolve_translation(kit, part_index, &placed_sockets, &translations)?;
        translations.insert(part_index, translation);
        // Record this part's socket world positions for dependents.
        let mut world = BTreeMap::new();
        for socket in &part.sockets {
            world.insert(
                socket.id.clone(),
                [
                    socket.position[0] + translation[0] as f64,
                    socket.position[1] + translation[1] as f64,
                    socket.position[2] + translation[2] as f64,
                ],
            );
        }
        placed_sockets.insert(part_index, world);
    }

    // Emit cells with overlap resolution (earlier part wins) and provenance.
    let mut frame = AssembledFrame {
        voxels: BTreeMap::new(),
    };
    for &part_index in &order {
        let part = &kit.parts[part_index];
        let translation = translations[&part_index];
        for (voxel_index, cell) in part.cells.iter().enumerate() {
            let coordinate = [
                cell.coordinate[0] + translation[0],
                cell.coordinate[1] + translation[1],
                cell.coordinate[2] + translation[2],
            ];
            // Earlier part wins; do not overwrite.
            frame.voxels.entry(coordinate).or_insert(AssembledVoxel {
                coordinate,
                material_slot: cell.material_slot,
                origin: VoxelOrigin::Canonical(CanonicalVoxelId {
                    part_index: part_index as u32,
                    voxel_index: voxel_index as u32,
                }),
            });
        }
    }

    // Ground the assembled character: shift the whole frame vertically so its
    // lowest occupied cell rests on the declared ground plane. Root parts are
    // placed by their pivot, but limbs and equipment may hang below that root,
    // so grounding is a property of the finished assembly, not any single part.
    if let Some((lo, _)) = frame.bounds() {
        let shift = kit.convention.ground_y - lo[1];
        if shift != 0 {
            frame = AssembledFrame {
                voxels: frame
                    .voxels
                    .into_values()
                    .map(|mut voxel| {
                        voxel.coordinate[1] += shift;
                        (voxel.coordinate, voxel)
                    })
                    .collect(),
            };
        }
    }
    Ok(frame)
}

/// Compute a placement order where every part appears after the part it mates
/// to (when it has a mate). Deterministic: ties broken by part index.
fn placement_order(kit: &VoxelKit) -> Result<Vec<usize>, KitError> {
    let index_of: BTreeMap<&str, usize> = kit
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i))
        .collect();
    let mut placed = vec![false; kit.parts.len()];
    let mut order = Vec::with_capacity(kit.parts.len());
    // Roots (no mate on any socket) first, in declaration order.
    for (i, part) in kit.parts.iter().enumerate() {
        if part.sockets.iter().all(|s| s.mate.is_none()) {
            placed[i] = true;
            order.push(i);
        }
    }
    if order.is_empty() {
        return Err(KitError::Assembly(
            "kit has no root part (every part declares a mate)".to_owned(),
        ));
    }
    // Repeatedly place parts whose mate's part is already placed.
    let mut progressed = true;
    while order.len() < kit.parts.len() && progressed {
        progressed = false;
        for i in 0..kit.parts.len() {
            if placed[i] {
                continue;
            }
            let mates = kit.parts[i]
                .sockets
                .iter()
                .filter_map(|s| s.mate.as_deref())
                .map(|m| parse_mate(m).map(|(p, _)| p))
                .collect::<Result<Vec<_>, _>>()
                .map_err(KitError::validation)?;
            let all_placed = mates.iter().all(|mate_part| {
                index_of
                    .get(mate_part)
                    .map(|&idx| placed[idx])
                    .unwrap_or(false)
            });
            if all_placed {
                placed[i] = true;
                order.push(i);
                progressed = true;
            }
        }
    }
    if order.len() != kit.parts.len() {
        return Err(KitError::Assembly(
            "mate relationships form a cycle; cannot order parts".to_owned(),
        ));
    }
    Ok(order)
}

/// Determine the world translation for one part. Root parts translate so their
/// pivot sits at the ground plane at the kit origin; mated parts translate so
/// their mating socket coincides with the mate's world socket position.
fn resolve_translation(
    kit: &VoxelKit,
    part_index: usize,
    placed_sockets: &BTreeMap<usize, BTreeMap<String, [f64; 3]>>,
    translations: &BTreeMap<usize, [i64; 3]>,
) -> Result<[i64; 3], KitError> {
    let part = &kit.parts[part_index];
    let index_of: BTreeMap<&str, usize> = kit
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.as_str(), i))
        .collect();

    // Find the first mated socket to anchor this part.
    for socket in &part.sockets {
        if let Some(mate) = &socket.mate {
            let (mate_part_id, mate_socket_id) = parse_mate(mate).map_err(KitError::validation)?;
            let &mate_index = index_of.get(mate_part_id).ok_or_else(|| {
                KitError::Assembly(format!(
                    "part {}: mate part {mate_part_id} not placed",
                    part.id
                ))
            })?;
            let mate_translation = translations.get(&mate_index).ok_or_else(|| {
                KitError::Assembly(format!(
                    "part {}: mate part {mate_part_id} has no translation",
                    part.id
                ))
            })?;
            let mate_part = &kit.parts[mate_index];
            let mate_socket = mate_part.socket(mate_socket_id).ok_or_else(|| {
                KitError::Assembly(format!(
                    "part {}: mate socket {mate_socket_id} missing on {mate_part_id}",
                    part.id
                ))
            })?;
            let _ = placed_sockets; // socket world positions are derived from translations.
                                    // World position of mate socket:
            let mate_world = [
                mate_socket.position[0] + mate_translation[0] as f64,
                mate_socket.position[1] + mate_translation[1] as f64,
                mate_socket.position[2] + mate_translation[2] as f64,
            ];
            // Translate this part so its socket lands on the mate socket.
            return Ok([
                (mate_world[0] - socket.position[0]).round() as i64,
                (mate_world[1] - socket.position[1]).round() as i64,
                (mate_world[2] - socket.position[2]).round() as i64,
            ]);
        }
    }
    // Root: place pivot at ground plane, centered on origin in X/Z.
    Ok([
        -part.pivot[0],
        kit.convention.ground_y - part.pivot[1],
        -part.pivot[2],
    ])
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Maximum bytes accepted for a single canonical kit document.
pub const MAX_KIT_BYTES: u64 = 4 * 1024 * 1024;

/// Load and validate a canonical kit from a JSON document on disk.
pub fn load_kit(root: &std::path::Path, relative_path: &str) -> Result<VoxelKit, KitError> {
    let path = crate::project::safe_join(root, relative_path).map_err(KitError::validation)?;
    let text = crate::project::read_bounded_text(&path, MAX_KIT_BYTES, "voxel kit")
        .map_err(KitError::validation)?;
    let kit: VoxelKit = serde_json::from_str(&text)
        .map_err(|e| KitError::validation(format!("{relative_path}: {e}")))?;
    kit.validate()?;
    Ok(kit)
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum KitError {
    /// A format or cross-reference validation failure, with an actionable message.
    Validation(String),
    /// A failure during neutral assembly (ordering, mate resolution).
    Assembly(String),
}

impl KitError {
    fn validation(message: String) -> KitError {
        KitError::Validation(message)
    }
}

impl fmt::Display for KitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KitError::Validation(m) => write!(f, "kit validation: {m}"),
            KitError::Assembly(m) => write!(f, "kit assembly: {m}"),
        }
    }
}

impl std::error::Error for KitError {}

fn require_identity(value: &str, path: &str) -> Result<(), String> {
    if value.trim().is_empty() || value != value.trim() {
        Err(format!("{path} must be non-empty canonical text"))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(slot: u16) -> PaletteSlot {
        PaletteSlot {
            slot,
            display_name: format!("slot {slot}"),
            color: [0.5, 0.5, 0.5, 1.0],
        }
    }

    fn cell(x: i64, y: i64, z: i64, material_slot: u16) -> KitCell {
        KitCell {
            coordinate: [x, y, z],
            material_slot,
        }
    }

    fn two_part_kit() -> VoxelKit {
        VoxelKit {
            schema_version: KIT_SCHEMA_VERSION,
            id: "test".to_owned(),
            version: 1,
            convention: CoordinateConvention {
                coordinate_system: "right_handed_y_up".to_owned(),
                forward_axis: "-Z".to_owned(),
                voxel_size_meters: 0.04,
                ground_y: 0,
                neutral_facing: [0, 0, -1],
            },
            palette: vec![PaletteGroup {
                name: "body".to_owned(),
                slots: vec![slot(1), slot(2)],
            }],
            parts: vec![
                KitPart {
                    id: "torso".to_owned(),
                    version: 1,
                    pivot: [0, 0, 0],
                    cells: vec![cell(0, 0, 0, 1), cell(0, 1, 0, 1)],
                    sockets: vec![Socket {
                        id: "neck".to_owned(),
                        position: [0.0, 2.0, 0.0],
                        forward: [0.0, 1.0, 0.0],
                        radius: 2.0,
                        mate: None,
                    }],
                    palette_groups: vec!["body".to_owned()],
                    symmetry_partner: None,
                },
                KitPart {
                    id: "head".to_owned(),
                    version: 1,
                    pivot: [0, 0, 0],
                    cells: vec![cell(0, 0, 0, 2)],
                    sockets: vec![Socket {
                        id: "neck".to_owned(),
                        position: [0.0, 0.0, 0.0],
                        forward: [0.0, -1.0, 0.0],
                        radius: 2.0,
                        mate: Some("torso.neck".to_owned()),
                    }],
                    palette_groups: vec!["body".to_owned()],
                    symmetry_partner: None,
                },
            ],
            invariants: IdentityInvariants {
                min_limb_thickness: 2,
                protected_parts: vec![],
                volume_range: None,
                required_sockets: vec![],
            },
        }
    }

    #[test]
    fn valid_kit_passes() {
        assert!(two_part_kit().validate().is_ok());
    }

    #[test]
    fn head_mates_onto_torso_neck() {
        let kit = two_part_kit();
        let frame = assemble_neutral(&kit).expect("assembly");
        // Torso root: pivot [0,0,0] -> translation [0,0,0]; cells at (0,0,0),(0,1,0).
        // Head mates its neck socket ([0,0,0]) to torso.neck world ([0,2,0]).
        // So head translation is [0,2,0] and its cell lands at (0,2,0).
        assert!(frame.voxels.contains_key(&[0, 0, 0]));
        assert!(frame.voxels.contains_key(&[0, 1, 0]));
        let head_voxel = frame.voxels.get(&[0, 2, 0]).expect("head cell placed");
        assert_eq!(head_voxel.material_slot, 2);
        match head_voxel.origin {
            VoxelOrigin::Canonical(id) => assert_eq!(id.part_index, 1),
        }
    }

    #[test]
    fn assembly_is_deterministic() {
        let kit = two_part_kit();
        let a = assemble_neutral(&kit).expect("first");
        let b = assemble_neutral(&kit).expect("second");
        assert_eq!(a, b);
        assert_eq!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn rejects_unsorted_cells() {
        let mut kit = two_part_kit();
        kit.parts[0].cells = vec![cell(0, 1, 0, 1), cell(0, 0, 0, 1)];
        assert!(kit.validate().is_err());
    }

    #[test]
    fn rejects_missing_mate_socket() {
        let mut kit = two_part_kit();
        kit.parts[1].sockets[0].mate = Some("torso.missing".to_owned());
        assert!(kit.validate().is_err());
    }

    #[test]
    fn rejects_unknown_material_slot() {
        let mut kit = two_part_kit();
        kit.parts[0].cells[0].material_slot = 9;
        assert!(kit.validate().is_err());
    }

    #[test]
    fn rejects_unknown_palette_group() {
        let mut kit = two_part_kit();
        kit.parts[0].palette_groups = vec!["nope".to_owned()];
        assert!(kit.validate().is_err());
    }

    #[test]
    fn rejects_mate_cycle() {
        let mut kit = two_part_kit();
        kit.parts[0].sockets[0].mate = Some("head.neck".to_owned());
        // Now torso mates to head and head mates to torso -> no root / cycle.
        assert!(assemble_neutral(&kit).is_err());
    }

    #[test]
    fn rejects_reserved_slot_zero() {
        let mut kit = two_part_kit();
        kit.parts[0].cells[0].material_slot = 0;
        assert!(kit.validate().is_err());
    }
}
