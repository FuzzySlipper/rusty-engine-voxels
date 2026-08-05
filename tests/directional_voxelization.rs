use std::fs;
use std::path::{Path, PathBuf};

use rusty_engine_voxels::directional_voxel::run_directional_voxelization;

const SPEC: &str = "content/characters/directional-sentinel/voxelization.json";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn dense_directional_object_regenerates_byte_identically_and_stays_grounded() {
    let root = root();
    let run = run_directional_voxelization(&root, SPEC, "target/directional-voxelization-test")
        .expect("checked directional source should regenerate");
    let checked = fs::read_to_string(root.join(&run.publication.path)).expect("published object");
    assert_eq!(run.compiled.canonical_json, checked);
    assert_eq!(
        run.publication.content_hash,
        "sha256:c825561702dfe5a3da9df1ad8c8d0f46a65da3f6f1cf165744e06c991f24422a"
    );

    let evidence = run.evidence.as_object().expect("evidence object");
    assert_eq!(evidence["cellSizeMeters"], 0.01);
    assert_eq!(evidence["depthCells"], 24);
    assert_eq!(evidence["peakVoxelsPerFrame"], 24_528);
    assert_eq!(evidence["totalVoxels"], 164_544);
    let frames = evidence["frames"].as_array().expect("frame evidence");
    assert_eq!(frames.len(), 8);
    assert!(frames.iter().all(|frame| frame["bounds"]["min"][1] == 0));
    assert!(frames
        .iter()
        .all(|frame| (10_000..=100_000).contains(&frame["voxels"].as_u64().unwrap())));
}
