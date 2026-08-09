use rusty_engine::voxel_asset;
use rusty_engine_voxels::assemble::{AssembledVoxelCell, RoughFrame};
use rusty_engine_voxels::flipbook::{
    compile_posed_flipbook, publish_compiled_flipbook, FlipbookCompileSettings,
};
use rusty_engine_voxels::kit::load_kit;
use std::path::PathBuf;
use voxel_asset::{
    encode_voxel_object, represented_voxel_count, with_computed_voxel_object_hashes,
};

fn main() -> Result<(), String> {
    let root = PathBuf::from("/home/dev/rusty-engine-voxels");
    let kit = load_kit(
        &root,
        "content/characters/directional-sentinel/character.json",
    )
    .map_err(|e| e.to_string())?;
    let mut vox_map = std::collections::BTreeMap::new();
    let mut add_box = |x0: i64, x1: i64, y0: i64, y1: i64, z0: i64, z1: i64, slot: u16| {
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    vox_map.insert([x, y, z], slot);
                }
            }
        }
    };
    // LLM-authored from front sprite 41x57, vague reference, no deterministic code
    // Head 6x6x6 at y 48-53
    add_box(-3, 3, 48, 53, -3, 3, 2);
    // Torso 8x12x4 at y 32-44
    add_box(-4, 4, 32, 44, -2, 2, 1);
    // Legs
    add_box(-4, -2, 0, 16, -2, 1, 1);
    add_box(2, 4, 0, 16, -2, 1, 1);
    // red blood accents
    add_box(-4, -2, 4, 6, -1, 1, 3);
    add_box(2, 4, 8, 10, -1, 1, 3);
    // Arms
    add_box(-6, -4, 30, 40, -1, 1, 1);
    add_box(4, 6, 30, 38, -1, 1, 1);
    // Rifle long forward
    add_box(0, 12, 36, 38, 2, 3, 3);
    // Boots - overlapping intentionally, last write wins via map
    add_box(-4, -1, 0, 2, -2, 1, 1);
    add_box(2, 5, 0, 2, -2, 1, 1);

    let voxels: Vec<AssembledVoxelCell> = vox_map
        .into_iter()
        .enumerate()
        .map(|(idx, (coord, slot))| AssembledVoxelCell {
            coordinate: coord,
            material_slot: slot,
            part_id: 0,
            source_voxel_index: idx as u32,
            needs_fusion: false,
        })
        .collect();
    let frame = RoughFrame {
        time_microseconds: 0,
        duration_microseconds: 120000,
        voxels,
        discarded_overlaps: Vec::new(),
    };
    let settings = FlipbookCompileSettings {
        asset_id: "voxel-object/posed-directional-sentinel-llm".to_string(),
        clip_id: "clip/idle-llm".to_string(),
        clip_name: "LLM idle (single view vague reference)".to_string(),
        source_path: "content/characters/directional-sentinel/carve-idle-0.json".to_string(),
        chunk_size: 16,
        anchors: Vec::new(),
        body_collision: None,
        hit_regions: Vec::new(),
    };
    let source_bytes = b"llm-kit single view vague reference front sprite 41x57";
    let mut compiled = compile_posed_flipbook(&kit, &[frame], &settings, source_bytes, 0.01)
        .map_err(|e| e.to_string())?;
    compiled.asset.provenance.converter = "rusty-engine-voxels.llm-kit-v1".to_string();
    compiled.asset =
        with_computed_voxel_object_hashes(compiled.asset).map_err(|e| e.to_string())?;
    compiled.canonical_json = encode_voxel_object(&compiled.asset).map_err(|e| e.to_string())?;
    let publication = publish_compiled_flipbook(&root, "content/voxel-objects", &compiled)
        .map_err(|e| e.to_string())?;
    let voxel_count = represented_voxel_count(&compiled.asset.clips[0].frames[0].frame);
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "path": publication.path,
            "contentHash": publication.content_hash,
            "voxels": voxel_count,
            "bounds": compiled.asset.bounds,
        }))
        .unwrap()
    );
    Ok(())
}
