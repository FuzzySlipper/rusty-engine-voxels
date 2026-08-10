use std::collections::BTreeMap;

use core_space::{ChunkCoord, ChunkDims, GridId, LocalVoxelCoord, VoxelGridSpec};
use core_voxel::VoxelValue;
use render_model::{TextureDescriptor, TextureFilter, TextureWrap};
use rusty_engine::{core_space, core_voxel, render_model, svc_mesh, svc_spatial, svc_volume};
use serde_json::{json, Value};
use svc_mesh::{mesh_cells_standalone, mesh_chunk_in_world, MeshPayload, MeshVoxelCell};
use svc_spatial::VoxelWorld;
use svc_volume::VoxelChunk;

use crate::project::sha256;

pub fn build_textured_voxel_report(texture_bytes: &[u8]) -> Result<Value, String> {
    let texture = TextureDescriptor::admit_png_rgba8_resource(
        "texture/directional-atlas".to_owned(),
        texture_bytes,
        TextureFilter::Nearest,
        TextureWrap::Clamp,
        1,
    )
    .map_err(|error| format!("directional texture rejected: {error:?}"))?;
    if [texture.width, texture.height] != [16, 8] {
        return Err("directional texture dimensions drifted".to_owned());
    }

    let wall_cells = rectangle_cells([48, 32, 1], 1);
    let wall = mesh_cells_standalone(0.25, [0.0; 3], &wall_cells, 4_000)
        .map_err(|error| error.to_string())?;
    require_greedy_box(&wall, 3_232)?;

    let floor_cells = rectangle_cells([48, 1, 32], 2);
    let floor = mesh_cells_standalone(0.25, [0.0; 3], &floor_cells, 4_000)
        .map_err(|error| error.to_string())?;
    require_greedy_box(&floor, 3_232)?;

    let negative = mesh_cells_standalone(
        1.0,
        [0.0; 3],
        &[MeshVoxelCell {
            coordinate: [-7, -5, -3],
            material_slot: 3,
        }],
        6,
    )
    .map_err(|error| error.to_string())?;
    let directions = direction_readout(&negative)?;

    let mixed = mesh_cells_standalone(
        1.0,
        [0.0; 3],
        &[
            MeshVoxelCell {
                coordinate: [0, 0, 0],
                material_slot: 1,
            },
            MeshVoxelCell {
                coordinate: [1, 0, 0],
                material_slot: 2,
            },
            MeshVoxelCell {
                coordinate: [2, 0, 0],
                material_slot: 3,
            },
            MeshVoxelCell {
                coordinate: [3, 0, 0],
                material_slot: 4,
            },
        ],
        64,
    )
    .map_err(|error| error.to_string())?;
    if mixed.groups.len() != 4 {
        return Err("mixed surface mesh lost a material group".to_owned());
    }

    let adjacent = adjacent_chunk_readout()?;
    Ok(json!({
        "schemaVersion": 1,
        "fixture": {
            "path": "content/textures/directional-atlas.png",
            "contentHash": texture.content_hash,
            "encodedByteLength": texture_bytes.len(),
            "dimensions": [texture.width, texture.height],
            "regions": [
                { "id": "warm-arrow", "contentMin": [1, 1], "contentExtent": [6, 6], "padding": [1, 1, 1, 1] },
                { "id": "cool-arrow", "contentMin": [9, 1], "contentExtent": [6, 6], "padding": [1, 1, 1, 1] }
            ]
        },
        "largeGreedySurfaces": {
            "wall48x32x1": mesh_readout(&wall),
            "floor48x1x32": mesh_readout(&floor),
            "untexturedBaselineContract": { "quads": 6, "vertices": 24, "indices": 36 }
        },
        "negativeCellSixFaces": {
            "cell": [-7, -5, -3],
            "mesh": mesh_readout(&negative),
            "directions": directions
        },
        "adjacentChunks": adjacent,
        "mixedMaterials": {
            "mesh": mesh_readout(&mixed),
            "slots": [
                { "slot": 1, "role": "standalone-repeat" },
                { "slot": 2, "role": "warm-atlas-region" },
                { "slot": 3, "role": "cool-atlas-region-tinted-emissive" },
                { "slot": 4, "role": "color-only-alpha-policy" }
            ]
        },
        "claims": {
            "greedyGeometryPreserved": true,
            "tileCoordinatesUseProductionMesher": true,
            "adjacentChunksWereMeshedIndependently": true,
            "rendererPixelsAndLifecycleAreOwnedByTheExactStudioIntegration": true
        }
    }))
}

fn rectangle_cells(extent: [i64; 3], slot: u16) -> Vec<MeshVoxelCell> {
    (0..extent[2])
        .flat_map(|z| {
            (0..extent[1]).flat_map(move |y| {
                (0..extent[0]).map(move |x| MeshVoxelCell {
                    coordinate: [x, y, z],
                    material_slot: slot,
                })
            })
        })
        .collect()
}

fn require_greedy_box(mesh: &MeshPayload, source_faces: u32) -> Result<(), String> {
    if mesh.stats.source_faces != source_faces
        || mesh.stats.quads != 6
        || mesh.stats.vertices != 24
        || mesh.stats.indices != 36
    {
        return Err(format!("greedy rectangular mesh drifted: {:?}", mesh.stats));
    }
    Ok(())
}

fn mesh_readout(mesh: &MeshPayload) -> Value {
    json!({
        "sourceFaces": mesh.stats.source_faces,
        "culledFaces": mesh.stats.faces_culled,
        "quads": mesh.stats.quads,
        "vertices": mesh.stats.vertices,
        "indices": mesh.stats.indices,
        "groups": mesh.groups.iter().map(|group| json!({
            "materialSlot": group.material_slot,
            "start": group.start,
            "count": group.count,
        })).collect::<Vec<_>>(),
        "bounds": { "min": mesh.bounds.min, "max": mesh.bounds.max },
        "positionStreamHash": f32_stream_hash(&mesh.positions),
        "normalStreamHash": f32_stream_hash(&mesh.normals),
        "tileCoordinateStreamHash": f32_stream_hash(&mesh.tile_coordinates),
        "indexStreamHash": u32_stream_hash(&mesh.indices),
        "streamBytes": {
            "positions": mesh.positions.len() * 4,
            "normals": mesh.normals.len() * 4,
            "tileCoordinates": mesh.tile_coordinates.len() * 4,
            "indices": mesh.indices.len() * 4,
        }
    })
}

fn direction_readout(mesh: &MeshPayload) -> Result<Value, String> {
    let mut directions = BTreeMap::<String, Vec<[f32; 2]>>::new();
    for vertex in 0..mesh.stats.vertices as usize {
        let normal = &mesh.normals[vertex * 3..vertex * 3 + 3];
        let key = format!("{:+.0},{:+.0},{:+.0}", normal[0], normal[1], normal[2]);
        directions.entry(key).or_default().push([
            mesh.tile_coordinates[vertex * 2],
            mesh.tile_coordinates[vertex * 2 + 1],
        ]);
    }
    if directions.len() != 6 || directions.values().any(|corners| corners.len() != 4) {
        return Err("single negative cell did not expose exactly six textured faces".to_owned());
    }
    serde_json::to_value(directions).map_err(|error| error.to_string())
}

fn adjacent_chunk_readout() -> Result<Value, String> {
    let spec = VoxelGridSpec::new(
        GridId::new(17),
        1.0,
        ChunkDims::cubic(16).ok_or_else(|| "invalid chunk dimensions".to_owned())?,
    )
    .ok_or_else(|| "invalid voxel grid".to_owned())?;
    let mut world = VoxelWorld::new(spec);
    for coordinate in [ChunkCoord::new(-1, 0, 0), ChunkCoord::new(0, 0, 0)] {
        let mut chunk = VoxelChunk::from_spec(&spec);
        chunk
            .fill_region(
                LocalVoxelCoord::new(0, 0, 0),
                LocalVoxelCoord::new(16, 16, 1),
                VoxelValue::solid_raw(1),
            )
            .map_err(|error| format!("chunk fixture rejected: {error:?}"))?;
        world.insert(coordinate, chunk);
    }
    let left = mesh_chunk_in_world(&world, ChunkCoord::new(-1, 0, 0))
        .ok_or_else(|| "left chunk missing".to_owned())?
        .map_err(|error| error.to_string())?;
    let right = mesh_chunk_in_world(&world, ChunkCoord::new(0, 0, 0))
        .ok_or_else(|| "right chunk missing".to_owned())?
        .map_err(|error| error.to_string())?;
    let left_extent = tile_extent_for_normal(&left, [0.0, 0.0, 1.0])?;
    let right_extent = tile_extent_for_normal(&right, [0.0, 0.0, 1.0])?;
    if left_extent[1][0] != right_extent[0][0] || left_extent[0][1] != right_extent[0][1] {
        return Err("independent chunk tile coordinates are discontinuous".to_owned());
    }
    Ok(json!({
        "leftChunk": { "coordinate": [-1, 0, 0], "mesh": mesh_readout(&left), "positiveZTileExtent": left_extent },
        "rightChunk": { "coordinate": [0, 0, 0], "mesh": mesh_readout(&right), "positiveZTileExtent": right_extent },
        "seamU": left_extent[1][0],
        "continuous": true
    }))
}

fn tile_extent_for_normal(mesh: &MeshPayload, expected: [f32; 3]) -> Result<[[f32; 2]; 2], String> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    let mut found = false;
    for vertex in 0..mesh.stats.vertices as usize {
        if mesh.normals[vertex * 3..vertex * 3 + 3] == expected {
            found = true;
            for axis in 0..2 {
                let value = mesh.tile_coordinates[vertex * 2 + axis];
                min[axis] = min[axis].min(value);
                max[axis] = max[axis].max(value);
            }
        }
    }
    if !found {
        return Err("expected textured face direction was absent".to_owned());
    }
    Ok([min, max])
}

fn f32_stream_hash(values: &[f32]) -> String {
    let bytes = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    sha256(&bytes)
}

fn u32_stream_hash(values: &[u32]) -> String {
    let bytes = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    sha256(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_texture_drives_bounded_directional_greedy_evidence() {
        let report = build_textured_voxel_report(include_bytes!(
            "../content/textures/directional-atlas.png"
        ))
        .expect("checked report");
        assert_eq!(report["largeGreedySurfaces"]["wall48x32x1"]["quads"], 6);
        assert_eq!(
            report["negativeCellSixFaces"]["directions"]
                .as_object()
                .map(serde_json::Map::len),
            Some(6)
        );
        assert_eq!(report["adjacentChunks"]["continuous"], true);
        assert_eq!(
            report["mixedMaterials"]["mesh"]["groups"]
                .as_array()
                .map(Vec::len),
            Some(4)
        );
    }
}
