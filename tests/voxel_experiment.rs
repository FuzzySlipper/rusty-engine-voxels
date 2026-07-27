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
            "protocolVersion": 8,
            "requestId": "describe-test",
        }))
        .expect("describe should succeed");
    assert_eq!(described["type"], "described");
    assert_eq!(described["adapter"]["projectKind"], "rustyEngineVoxelLab");

    let opened = adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 8,
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
        "protocolVersion": 8,
        "requestId": "unsupported-test",
    }));
    assert!(unsupported.is_err());
}

#[test]
fn reopened_applied_instance_playback_is_transient_and_rust_timed() {
    let loaded = load_project(&root(), DEFAULT_PROJECT_FILE).expect("checked project should load");
    let object_entry = loaded
        .project
        .voxel_objects
        .first()
        .expect("checked project should contain a voxel object");
    let project_before = fs::read(&loaded.path).expect("project bytes should be readable");
    let object_path = loaded.root.join(&object_entry.path);
    let object_before = fs::read(&object_path).expect("object bytes should be readable");
    let project_hash = loaded.project_hash.clone();

    let mut adapter = StudioAdapter::default();
    adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 8,
            "requestId": "playback-open-one",
            "root": root(),
            "projectFile": DEFAULT_PROJECT_FILE,
        }))
        .expect("initial open should succeed");
    adapter
        .dispatch(json!({
            "type": "closeProject",
            "protocolVersion": 8,
            "requestId": "playback-close",
        }))
        .expect("close should discard transient state");
    adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 8,
            "requestId": "playback-open-two",
            "root": root(),
            "projectFile": DEFAULT_PROJECT_FILE,
        }))
        .expect("reopen should rebuild canonical state");

    let unselected = adapter.dispatch(json!({
        "type": "previewVoxelObjectInstance",
        "protocolVersion": 8,
        "requestId": "playback-unselected",
        "expectedProjectHash": project_hash,
        "sceneId": "scene/voxel-lab",
        "instanceId": "retro-character",
        "nowMicroseconds": 1_000_000,
        "command": { "kind": "sample" }
    }));
    assert!(
        unselected.is_err(),
        "sample must not invent a player session"
    );
    let ambient_field = adapter.dispatch(json!({
        "type": "previewVoxelObjectInstance",
        "protocolVersion": 8,
        "requestId": "playback-ambient-field",
        "expectedProjectHash": project_hash,
        "sceneId": "scene/voxel-lab",
        "instanceId": "retro-character",
        "nowMicroseconds": 1_000_000,
        "command": {
            "kind": "scrub",
            "clipId": "clip/run",
            "clipFrame": 1,
            "loopMode": "repeat",
            "browserTimer": true
        }
    }));
    assert!(ambient_field.is_err(), "playback commands must stay closed");

    let scrubbed = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 8,
            "requestId": "playback-scrub",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 1_000_000,
            "command": {
                "kind": "scrub",
                "clipId": "clip/run",
                "clipFrame": 1,
                "loopMode": "repeat"
            }
        }))
        .expect("scrub should select an admitted stored frame");
    assert_eq!(scrubbed["type"], "voxelObjectInstancePreviewed");
    assert_eq!(scrubbed["playback"]["status"], "paused");
    assert_eq!(scrubbed["playback"]["clipFrame"], 1);
    assert_eq!(
        scrubbed["playback"]["durableFrame"],
        json!({ "kind": "clip", "clipId": "clip/run", "frameIndex": 0 })
    );
    assert_eq!(scrubbed["playback"]["projectHash"], project_hash);
    let scrubbed_runtime_frame = scrubbed["playback"]["runtimeFrame"]
        .as_u64()
        .expect("runtime frame should be an integer");
    assert_eq!(
        projected_instance_frame(&scrubbed),
        scrubbed_runtime_frame,
        "renderer-neutral presentation must use the Rust sample"
    );

    let playing = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 8,
            "requestId": "playback-play",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 1_000_000,
            "command": { "kind": "play" }
        }))
        .expect("play should resume the scrubbed posture");
    assert_eq!(playing["playback"]["status"], "playing");

    let sampled = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 8,
            "requestId": "playback-sample",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 1_200_000,
            "command": { "kind": "sample" }
        }))
        .expect("explicit-time sample should succeed");
    let sampled_runtime_frame = sampled["playback"]["runtimeFrame"]
        .as_u64()
        .expect("runtime frame should be an integer");
    assert_ne!(sampled_runtime_frame, scrubbed_runtime_frame);
    assert_eq!(projected_instance_frame(&sampled), sampled_runtime_frame);

    let stopped = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 8,
            "requestId": "playback-stop",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 1_200_000,
            "command": { "kind": "stop" }
        }))
        .expect("stop should restore the saved pose");
    assert_eq!(stopped["playback"]["status"], "stopped");
    assert_eq!(stopped["playback"]["clipId"], serde_json::Value::Null);

    assert_eq!(fs::read(&loaded.path).unwrap(), project_before);
    assert_eq!(fs::read(&object_path).unwrap(), object_before);
    let after = load_project(&root(), DEFAULT_PROJECT_FILE).unwrap();
    assert_eq!(after.project_hash, project_hash);
    assert_eq!(after.project.revision, loaded.project.revision);
    assert_eq!(
        after.project.instances[0].frame,
        loaded.project.instances[0].frame
    );
}

fn projected_instance_frame(response: &serde_json::Value) -> u64 {
    response["projection"]["ops"]
        .as_array()
        .expect("projection operations should be an array")
        .iter()
        .find(|operation| operation["op"] == "createVoxelObjectInstance")
        .and_then(|operation| operation["instance"]["frame"].as_u64())
        .expect("projection should create the canonical voxel-object instance")
}
