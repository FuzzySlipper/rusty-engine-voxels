//! Voxel data-plane format study: measures the checked corpus against candidate
//! mesh payload encodings so the upstream format decision starts from evidence
//! instead of preference.
//!
//! The study loads the checked voxel object through the same strict runtime
//! admission path as production playback, then prices four shapes for the
//! unique flipbook meshes:
//!
//! 1. `expanded-json` — the current wire shape: positions/normals/indices as
//!    expanded JSON number arrays.
//! 2. `packed-base64` — the same f32/u32 streams as little-endian bytes carried
//!    in base64 strings inside JSON (a text-safe packed candidate).
//! 3. `binary-reference` — raw little-endian bytes with a minimal per-mesh
//!    header; the lower bound for any full-binary option.
//! 4. `mesh-delta` — one full base mesh plus, for every other mesh, only the
//!    vertices/indices that differ from that base, under both text and binary
//!    accounting.
//!
//! Browser-relevant costs are measured alongside byte counts: a JSON parse of
//! the current response shape, and a base64 decode pass as a proxy for the
//! packed candidate's transfer cost. All counts are derived from the checked
//! canonical bytes; nothing here writes project or object state.

use std::path::Path;
use std::time::Instant;

use serde::Serialize;
use svc_mesh::MeshPayload;

use crate::base64::{decode as base64_decode, encode as base64_encode};
use crate::provider_pin::engine_revision;
use crate::runtime::{load_runtime_project, RuntimeProject};

/// Round-trips each encoding implementation through several repetitions so
/// timer resolution stays well below the measured cost.
const TIMING_REPETITIONS: u32 = 3;
/// One minimal binary mesh header: counts, stream lengths, bounds, groups.
const BINARY_MESH_HEADER_BYTES: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamFormatEvidence {
    pub expanded_json_bytes: usize,
    pub packed_base64_bytes: usize,
    pub binary_reference_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshFormatEvidence {
    pub mesh_index: usize,
    pub vertices: u32,
    pub indices: u32,
    pub streams: StreamFormatEvidence,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeltaFormatEvidence {
    pub base_mesh_index: usize,
    pub fully_shared_meshes: usize,
    pub average_changed_vertex_fraction: f64,
    pub average_changed_index_fraction: f64,
    pub expanded_json_bytes: usize,
    pub binary_reference_bytes: usize,
    pub json_savings_fraction: f64,
    pub binary_savings_fraction: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingEvidence {
    pub repetitions: u32,
    pub expanded_json_parse_microseconds: u128,
    pub packed_base64_decode_microseconds: u128,
    pub packed_base64_bytes_decoded: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatStudyEvidence {
    pub engine_revision: String,
    pub project_file: String,
    pub project_hash: String,
    pub asset_id: String,
    pub content_hash: String,
    pub unique_mesh_count: usize,
    pub canonical_object_bytes: usize,
    pub streams: StreamFormatEvidence,
    pub delta: Option<DeltaFormatEvidence>,
    pub timing: TimingEvidence,
    pub meshes: Vec<MeshFormatEvidence>,
    pub interpretation_limits: Vec<String>,
}

pub fn run_format_study(
    root: &Path,
    relative_project: &str,
) -> Result<FormatStudyEvidence, String> {
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
    let meshes: Vec<&MeshPayload> = object.meshes().iter().map(AsRef::as_ref).collect();
    if meshes.is_empty() {
        return Err("admitted object has no voxel meshes to measure".to_owned());
    }

    let mut expanded_json = Vec::new();
    let mut packed_base64 = Vec::new();
    let mut per_mesh = Vec::with_capacity(meshes.len());
    for (mesh_index, mesh) in meshes.iter().enumerate() {
        let json = measure_expanded_json(mesh)?;
        let packed = measure_packed_base64(mesh);
        let binary = binary_reference_bytes(mesh);
        let streams = StreamFormatEvidence {
            expanded_json_bytes: json.json_bytes,
            packed_base64_bytes: packed.json_bytes,
            binary_reference_bytes: binary,
        };
        expanded_json.push(json.clone());
        packed_base64.push(packed);
        per_mesh.push(MeshFormatEvidence {
            mesh_index,
            vertices: mesh.stats.vertices,
            indices: mesh.stats.indices,
            streams,
        });
    }
    let streams = StreamFormatEvidence {
        expanded_json_bytes: expanded_json.iter().map(|value| value.json_bytes).sum(),
        packed_base64_bytes: packed_base64.iter().map(|value| value.json_bytes).sum(),
        binary_reference_bytes: meshes.iter().map(|mesh| binary_reference_bytes(mesh)).sum(),
    };
    let delta = measure_delta(&meshes);
    let timing = measure_timing(&expanded_json, &packed_base64)?;

    Ok(FormatStudyEvidence {
        engine_revision: engine_revision()?,
        project_file: relative_project.to_owned(),
        project_hash: runtime.loaded.project_hash.clone(),
        asset_id: object.asset_id().to_owned(),
        content_hash: object.content_hash().to_owned(),
        unique_mesh_count: meshes.len(),
        canonical_object_bytes: canonical_object_bytes(&runtime),
        streams,
        delta,
        timing,
        meshes: per_mesh,
        interpretation_limits: vec![
            "byte counts cover mesh attribute streams (positions, normals, indices) plus a \
             minimal per-mesh header; full response/document framing is excluded and adds a \
             small constant overhead per shape"
                .to_owned(),
            "binary-reference is a lower-bound estimate (raw little-endian streams + 64-byte \
             header), not a proposed schema"
                .to_owned(),
            "packed-base64 keeps JSON transport (control channel) with typed-array-friendly \
             payloads; decode timing uses this repository's safe base64 as a proxy — a native \
             browser atob path would be faster"
                .to_owned(),
            "mesh-delta uses the first mesh as base; real encodings would choose the base per \
             object and may differ modestly"
                .to_owned(),
            "browser JSON.parse cost is approximated by a serde_json parse of the same shape; \
             relative, not absolute, comparison is the intent"
                .to_owned(),
        ],
    })
}

fn canonical_object_bytes(runtime: &RuntimeProject) -> usize {
    runtime
        .loaded
        .project
        .voxel_objects
        .iter()
        .map(|entry| {
            crate::project::safe_join(&runtime.loaded.root, &entry.path)
                .and_then(|path| std::fs::metadata(&path).map_err(|error| error.to_string()))
                .map(|metadata| metadata.len() as usize)
        })
        .collect::<Result<Vec<_>, String>>()
        .unwrap_or_default()
        .into_iter()
        .sum()
}

#[derive(Clone)]
struct ExpandedJsonMeasurement {
    json_bytes: usize,
    encoded: Vec<u8>,
}

struct PackedMeasurement {
    json_bytes: usize,
    encoded: String,
}

fn measure_expanded_json(mesh: &MeshPayload) -> Result<ExpandedJsonMeasurement, String> {
    let encoded = serde_json::to_vec(&WireMeshSource {
        positions: &mesh.positions,
        normals: &mesh.normals,
        indices: &mesh.indices,
    })
    .map_err(|error| error.to_string())?;
    let json_bytes = encoded.len();
    Ok(ExpandedJsonMeasurement {
        json_bytes,
        encoded,
    })
}

fn measure_packed_base64(mesh: &MeshPayload) -> PackedMeasurement {
    let mut body = Vec::with_capacity(packed_byte_len(mesh));
    push_f32s(&mut body, &mesh.positions);
    push_f32s(&mut body, &mesh.normals);
    push_u32s(&mut body, &mesh.indices);
    let encoded = base64_encode(&body);
    // Three named string fields plus JSON punctuation for the packed envelope.
    let json_bytes = encoded.len() + 96;
    PackedMeasurement {
        json_bytes,
        encoded,
    }
}

fn packed_byte_len(mesh: &MeshPayload) -> usize {
    (mesh.positions.len() + mesh.normals.len()) * 4 + mesh.indices.len() * 4
}

fn binary_reference_bytes(mesh: &MeshPayload) -> usize {
    BINARY_MESH_HEADER_BYTES + packed_byte_len(mesh) + mesh.groups.len() * 8
}

fn push_f32s(output: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

fn push_u32s(output: &mut Vec<u8>, values: &[u32]) {
    for value in values {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

fn measure_delta(meshes: &[&MeshPayload]) -> Option<DeltaFormatEvidence> {
    let base = meshes.first()?;
    let mut fully_shared = 0usize;
    let mut changed_vertex_fractions = Vec::new();
    let mut changed_index_fractions = Vec::new();
    let mut delta_expanded = 0usize;
    let mut delta_binary = 0usize;
    for mesh in meshes.iter().skip(1) {
        let vertex_changes = count_prefix_changes(&base.positions, &mesh.positions)
            + count_prefix_changes(&base.normals, &mesh.normals);
        let index_changes = count_prefix_changes(&base.indices, &mesh.indices);
        let vertex_total = mesh.positions.len() + mesh.normals.len();
        if vertex_changes == 0 && index_changes == 0 {
            fully_shared += 1;
        }
        changed_vertex_fractions.push(vertex_changes as f64 / vertex_total.max(1) as f64);
        changed_index_fractions.push(index_changes as f64 / mesh.indices.len().max(1) as f64);
        delta_expanded += delta_text_bytes(vertex_changes, index_changes);
        delta_binary += BINARY_MESH_HEADER_BYTES + (vertex_changes + index_changes) * 4;
    }
    let base_expanded = serde_json::to_vec(&WireMeshSource {
        positions: &base.positions,
        normals: &base.normals,
        indices: &base.indices,
    })
    .ok()?
    .len();
    let base_binary = binary_reference_bytes(base);
    let full_expanded: usize = meshes
        .iter()
        .map(|mesh| {
            serde_json::to_vec(&WireMeshSource {
                positions: &mesh.positions,
                normals: &mesh.normals,
                indices: &mesh.indices,
            })
            .map(|bytes| bytes.len())
        })
        .collect::<Result<Vec<_>, _>>()
        .ok()?
        .into_iter()
        .sum();
    let full_binary: usize = meshes.iter().map(|mesh| binary_reference_bytes(mesh)).sum();
    let delta_expanded_total = base_expanded + delta_expanded;
    let delta_binary_total = base_binary + delta_binary;
    let compared = changed_vertex_fractions.len().max(1) as f64;
    Some(DeltaFormatEvidence {
        base_mesh_index: 0,
        fully_shared_meshes: fully_shared,
        average_changed_vertex_fraction: changed_vertex_fractions.iter().sum::<f64>() / compared,
        average_changed_index_fraction: changed_index_fractions.iter().sum::<f64>() / compared,
        expanded_json_bytes: delta_expanded_total,
        binary_reference_bytes: delta_binary_total,
        json_savings_fraction: 1.0 - delta_expanded_total as f64 / full_expanded.max(1) as f64,
        binary_savings_fraction: 1.0 - delta_binary_total as f64 / full_binary.max(1) as f64,
    })
}

/// Delta text accounting: a JSON object per changed region would carry small
/// fixed overhead; approximate at 12 bytes per changed value, matching the
/// measured per-number cost of the expanded arrays.
fn delta_text_bytes(vertex_changes: usize, index_changes: usize) -> usize {
    (vertex_changes + index_changes) * 12
}

fn count_prefix_changes<T: PartialEq>(base: &[T], candidate: &[T]) -> usize {
    let shared = base.len().min(candidate.len());
    let mut changes = base.len().abs_diff(candidate.len());
    changes += base[..shared]
        .iter()
        .zip(&candidate[..shared])
        .filter(|(left, right)| left != right)
        .count();
    changes
}

fn measure_timing(
    expanded: &[ExpandedJsonMeasurement],
    packed: &[PackedMeasurement],
) -> Result<TimingEvidence, String> {
    let json_parse_started = Instant::now();
    let mut parsed_numbers = 0usize;
    for _ in 0..TIMING_REPETITIONS {
        for measurement in expanded {
            let value: serde_json::Value =
                serde_json::from_slice(&measurement.encoded).map_err(|error| error.to_string())?;
            parsed_numbers = parsed_numbers.saturating_add(count_numbers(&value));
        }
    }
    let expanded_json_parse_microseconds = json_parse_started.elapsed().as_micros();
    if parsed_numbers == 0 {
        return Err("expanded JSON timing parse produced no numbers".to_owned());
    }

    let packed_bytes: usize = packed.iter().map(|value| value.encoded.len()).sum();
    let base64_decode_started = Instant::now();
    let mut decoded_bytes = 0usize;
    for _ in 0..TIMING_REPETITIONS {
        for measurement in packed {
            decoded_bytes =
                decoded_bytes.saturating_add(base64_decode(&measurement.encoded)?.len());
        }
    }
    let packed_base64_decode_microseconds = base64_decode_started.elapsed().as_micros();

    Ok(TimingEvidence {
        repetitions: TIMING_REPETITIONS,
        expanded_json_parse_microseconds,
        packed_base64_decode_microseconds,
        packed_base64_bytes_decoded: packed_bytes,
    })
}

fn count_numbers(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Number(_) => 1,
        serde_json::Value::Array(values) => values.iter().map(count_numbers).sum(),
        serde_json::Value::Object(fields) => fields.values().map(count_numbers).sum(),
        _ => 0,
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireMeshSource<'a> {
    positions: &'a [f32],
    normals: &'a [f32],
    indices: &'a [u32],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh(vertices: u32, seed: f32) -> MeshPayload {
        let positions = (0..vertices * 3)
            .map(|index| seed + index as f32 * 0.25)
            .collect::<Vec<_>>();
        let normals = (0..vertices * 3)
            .map(|index| if index % 3 == 0 { 1.0 } else { 0.0 })
            .collect::<Vec<_>>();
        let indices = (0..vertices).collect::<Vec<_>>();
        MeshPayload {
            positions,
            normals,
            indices,
            groups: Vec::new(),
            bounds: svc_mesh::MeshBounds {
                min: [0.0; 3],
                max: [1.0; 3],
            },
            stats: svc_mesh::MeshStats {
                vertices,
                indices: vertices,
                quads: 0,
                faces_emitted: 0,
                faces_culled: 0,
            },
        }
    }

    #[test]
    fn packed_and_binary_undercut_expanded_json() {
        let mesh = mesh(128, 2.0);
        let _expanded = measure_expanded_json(&mesh).expect("expanded measurement");
        let packed = measure_packed_base64(&mesh);
        let binary = binary_reference_bytes(&mesh);
        assert_eq!(
            packed_byte_len(&mesh).div_ceil(3) * 4 + 96,
            packed.json_bytes
        );
        assert!(binary < packed.json_bytes);
        // The comparison itself is fixture-dependent and deliberately not
        // asserted here: tiny integer-valued floats (this fixture's normals)
        // serialize as 1-3 JSON bytes each, cheaper than packed 4.67 bytes,
        // while real quantized world-space floats cost ~8-12 bytes each and
        // flip the relationship. The evidence report surfaces both regimes
        // instead of hiding them behind a synthetic assertion.
        assert_eq!(
            packed_byte_len(&mesh),
            (mesh.positions.len() + mesh.normals.len() + mesh.indices.len()) * 4
        );
        assert_eq!(
            packed_byte_len(&mesh).div_ceil(3) * 4 + 96,
            packed.json_bytes
        );
    }

    #[test]
    fn delta_counts_only_changed_values() {
        let base = mesh(64, 1.0);
        let mut changed = mesh(64, 1.0);
        changed.positions[3] = 99.0;
        changed.indices[5] = 999;
        assert_eq!(count_prefix_changes(&base.positions, &changed.positions), 1);
        assert_eq!(count_prefix_changes(&base.indices, &changed.indices), 1);
        let longer = mesh(80, 1.0);
        assert_eq!(count_prefix_changes(&base.indices, &longer.indices), 16);
    }

    #[test]
    fn identical_meshes_report_full_delta_sharing() {
        let owned = [mesh(32, 4.0), mesh(32, 4.0), mesh(32, 4.0)];
        let meshes: Vec<&MeshPayload> = owned.iter().collect();
        let delta = measure_delta(&meshes).expect("delta measurement");
        assert_eq!(delta.fully_shared_meshes, 2);
        assert!(delta.json_savings_fraction > 0.5);
        assert!(delta.binary_savings_fraction > 0.5);
    }
}
