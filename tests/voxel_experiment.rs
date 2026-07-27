use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusty_engine_voxels::adapter::StudioAdapter;
use rusty_engine_voxels::conversion::prepare_project_conversion;
use rusty_engine_voxels::project::load_project;
use rusty_engine_voxels::runtime::verify_runtime_project;
use rusty_engine_voxels::DEFAULT_PROJECT_FILE;
use serde_json::json;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn checked_animated_conversion_is_deterministic() {
    let prepared = prepare_project_conversion(&root(), DEFAULT_PROJECT_FILE)
        .expect("checked animated source should convert");
    let candidate = prepared.prepared.candidate();
    let object = prepared
        .loaded
        .project
        .voxel_objects
        .iter()
        .find(|object| object.asset_id == candidate.asset.asset_id)
        .expect("project should point at the converted object");
    let checked = fs::read_to_string(root().join(&object.path))
        .expect("checked voxel object should be readable");

    assert_eq!(candidate.canonical_json, checked);
    assert_eq!(candidate.content_hash, object.expected_content_hash);
    assert_eq!(candidate.sampled_frames, 16);
    assert_eq!(candidate.stored_frames, 13);
    assert_eq!(candidate.clips.len(), 3);
    assert_eq!(candidate.aggregate_voxels, 2_265);
}

#[test]
fn checked_object_loads_plays_and_projects() {
    let evidence = verify_runtime_project(&root(), DEFAULT_PROJECT_FILE)
        .expect("checked voxel object should load through the runtime");

    assert_eq!(evidence.frame_count, 13);
    assert_eq!(evidence.clip_count, 3);
    assert_eq!(evidence.unique_mesh_count, 12);
    assert_eq!(evidence.projection_operation_count, 3);
    assert_eq!(evidence.defined_voxel_objects, 1);
    assert_eq!(evidence.created_voxel_instances, 1);
    assert_eq!(evidence.playback_samples.len(), 5);
    assert!(
        evidence
            .playback_samples
            .iter()
            .map(|sample| sample.mesh_index)
            .collect::<BTreeSet<_>>()
            .len()
            > 1,
        "explicit-time playback should traverse more than one voxel mesh"
    );
}

#[test]
fn project_rejects_unsafe_content_paths_and_dangling_instances() {
    let loaded = load_project(&root(), DEFAULT_PROJECT_FILE).expect("checked project should load");

    let mut unsafe_path = loaded.project.clone();
    unsafe_path.voxel_objects[0].path = "../outside.voxel-object.json".to_owned();
    assert!(unsafe_path.validate().is_err());

    let mut dangling = loaded.project;
    dangling.instances[0].voxel_object_asset_id = "voxel-object/missing".to_owned();
    assert!(dangling.validate().is_err());
}

#[test]
fn studio_adapter_opens_the_project_and_rejects_unowned_mutation() {
    let mut adapter = StudioAdapter::default();
    let described = adapter
        .dispatch(json!({
            "type": "describe",
            "protocolVersion": 7,
            "requestId": "describe-test",
        }))
        .expect("describe should succeed");
    assert_eq!(described["type"], "described");
    assert_eq!(described["adapter"]["projectKind"], "rustyEngineVoxelLab");

    let opened = adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 7,
            "requestId": "open-test",
            "root": root(),
            "projectFile": DEFAULT_PROJECT_FILE,
        }))
        .expect("open should return a Studio readout");
    assert_eq!(opened["type"], "projectOpened");
    assert_eq!(
        opened["project"]["voxelObjectAuthoring"]["assets"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        opened["project"]["projection"]["ops"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );
    assert_eq!(
        opened["project"]["animatedMeshResources"][0]["clipIds"],
        json!(["idle", "run", "jump"])
    );

    let unsupported = adapter.dispatch(json!({
        "type": "createSceneObject",
        "protocolVersion": 7,
        "requestId": "unsupported-test",
    }));
    assert!(unsupported.is_err());
}
