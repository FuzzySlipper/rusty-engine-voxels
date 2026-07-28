use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use rusty_engine_voxels::adapter::StudioAdapter;
use rusty_engine_voxels::conversion::prepare_project_conversion;
use rusty_engine_voxels::format_study::run_format_study;
use rusty_engine_voxels::project::load_project;
use rusty_engine_voxels::runtime::verify_runtime_project;
use rusty_engine_voxels::DEFAULT_PROJECT_FILE;
use serde_json::json;

const HIGH_FIDELITY_PROJECT_FILE: &str =
    "content/projects/retro-character-high-fidelity.project.json";

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
    assert_eq!(candidate.stored_frames, 15);
    assert_eq!(candidate.clips.len(), 3);
    assert_eq!(candidate.aggregate_voxels, 9_650);
}

#[test]
fn checked_object_loads_plays_and_projects() {
    let evidence = verify_runtime_project(&root(), DEFAULT_PROJECT_FILE)
        .expect("checked voxel object should load through the runtime");

    assert_eq!(evidence.frame_count, 15);
    assert_eq!(evidence.clip_count, 3);
    assert_eq!(evidence.unique_mesh_count, 14);
    assert_eq!(evidence.projection_operation_count, 3);
    assert_eq!(evidence.defined_voxel_objects, 1);
    assert_eq!(evidence.created_voxel_instances, 1);
    assert_eq!(evidence.playback_samples.len(), 5);
    assert_eq!(evidence.behavior.saved_frame, "default");
    assert_eq!(evidence.behavior.default_runtime_frame, 0);
    assert!(evidence.behavior.once_ended);
    assert!(evidence.behavior.repeat_wrapped_to_first_frame);
    assert!(evidence.behavior.paused_frame_stayed_stable);
    assert!(evidence.behavior.resumed_to_later_frame);
    assert!(evidence.behavior.posture_round_trip_matched);
    assert!(evidence.behavior.project_reopen_matched);
    assert!(evidence.behavior.missing_asset_rejected);
    assert!(evidence.behavior.corrupt_asset_rejected);
    assert_eq!(evidence.behavior.collision_kind, "stableFrame");
    assert!(evidence.behavior.collision_voxel_data_hash.is_some());
    assert!(evidence.behavior.collision_stayed_stable_during_playback);
    assert!(evidence.behavior.durable_project_bytes_unchanged);
    assert!(evidence.behavior.durable_object_bytes_unchanged);
    assert_eq!(evidence.frame_switch.requested_switches, 512);
    assert!(evidence.frame_switch.emitted_frame_swaps >= 500);
    assert_eq!(evidence.frame_switch.unique_meshes_reused, 14);
    assert!(evidence.resources.unique_mesh_payload_bytes > 0);
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

    let loaded = load_project(&root(), DEFAULT_PROJECT_FILE).expect("checked project should load");
    let mut duplicate_owner = loaded.project;
    duplicate_owner
        .instances
        .push(duplicate_owner.instances[0].clone());
    duplicate_owner.instances[1].instance_id = "retro-character-copy".to_owned();
    assert!(duplicate_owner.validate().is_err());
}

#[test]
fn studio_adapter_opens_the_project_and_rejects_unowned_mutation() {
    let mut adapter = StudioAdapter::default();
    let described = adapter
        .dispatch(json!({
            "type": "describe",
            "protocolVersion": 9,
            "requestId": "describe-test",
        }))
        .expect("describe should succeed");
    assert_eq!(described["type"], "described");
    assert_eq!(described["adapter"]["projectKind"], "rustyEngineVoxelLab");

    let opened = adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 9,
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
        opened["project"]["voxelObjectAuthoring"]["instances"][0]["ownerEntityId"],
        1
    );
    assert_eq!(
        opened["project"]["sceneHierarchy"]["nodes"][0]["entityId"],
        1
    );
    assert_eq!(
        opened["project"]["inspections"]["entityState"]["capabilities"][0],
        json!({ "name": "voxelObject", "count": 1 })
    );
    assert_eq!(
        opened["project"]["projection"]["ops"][2]["instance"]["metadata"]["sourceEntity"],
        1
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
        "protocolVersion": 9,
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
    let reopened = adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 9,
            "requestId": "playback-open-one",
            "root": root(),
            "projectFile": DEFAULT_PROJECT_FILE,
        }))
        .expect("initial open should succeed");
    adapter
        .dispatch(json!({
            "type": "closeProject",
            "protocolVersion": 9,
            "requestId": "playback-close",
        }))
        .expect("close should discard transient state");
    adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": 9,
            "requestId": "playback-open-two",
            "root": root(),
            "projectFile": DEFAULT_PROJECT_FILE,
        }))
        .expect("reopen should rebuild canonical state");
    let reopened_response_bytes = serde_json::to_vec(&reopened)
        .expect("reopened response should serialize")
        .len();

    let unselected = adapter.dispatch(json!({
        "type": "previewVoxelObjectInstance",
        "protocolVersion": 9,
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
        "protocolVersion": 9,
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
            "protocolVersion": 9,
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
        json!({ "kind": "default" })
    );
    assert_eq!(scrubbed["playback"]["projectHash"], project_hash);
    assert_eq!(
        scrubbed["projection"]["ops"][0]["op"],
        "setVoxelObjectFrame"
    );
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
            "protocolVersion": 9,
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
            "protocolVersion": 9,
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
    assert_eq!(sampled["projection"]["ops"][0]["op"], "setVoxelObjectFrame");
    let sampled_response_bytes = serde_json::to_vec(&sampled)
        .expect("sampled response should serialize")
        .len();
    assert!(
        sampled_response_bytes * 100 < reopened_response_bytes,
        "steady-state playback should be at least 100x smaller than the complete project response"
    );

    let stopped = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 9,
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

    let once_scrubbed = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 9,
            "requestId": "playback-once-scrub",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 2_000_000,
            "command": {
                "kind": "scrub",
                "clipId": "clip/run",
                "clipFrame": 0,
                "loopMode": "once"
            }
        }))
        .expect("once playback should scrub to its first pose");
    assert_eq!(once_scrubbed["playback"]["clipFrame"], 0);
    adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 9,
            "requestId": "playback-once-play",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 2_000_000,
            "command": { "kind": "play" }
        }))
        .expect("once playback should start");
    let once_ended = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 9,
            "requestId": "playback-once-ended",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 2_666_667,
            "command": { "kind": "sample" }
        }))
        .expect("once playback should settle its terminal pose");
    assert_eq!(once_ended["playback"]["status"], "paused");
    assert_eq!(once_ended["playback"]["clipFrame"], 3);
    assert_eq!(once_ended["playback"]["ended"], true);
    let once_replayed = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": 9,
            "requestId": "playback-once-replay",
            "expectedProjectHash": project_hash,
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "nowMicroseconds": 3_000_000,
            "command": { "kind": "play" }
        }))
        .expect("playing an ended once clip should restart it");
    assert_eq!(once_replayed["playback"]["status"], "playing");
    assert_eq!(once_replayed["playback"]["clipFrame"], 0);
    assert_eq!(once_replayed["playback"]["ended"], false);

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

#[test]
fn checked_quality_report_compares_named_source_and_voxel_poses() {
    let prepared = prepare_project_conversion(&root(), DEFAULT_PROJECT_FILE)
        .expect("checked animated source should produce quality evidence");
    let quality = prepared
        .quality
        .as_ref()
        .expect("ordinary conversion preparation should retain its quality readout");
    assert_eq!(quality.clips.len(), 3);
    assert!(quality.palette_stable);
    assert_eq!(quality.silhouette_resolution, 32);
    for clip in &quality.clips {
        assert!(!clip.frames.is_empty());
        assert!(clip.palette_stable);
        assert!(clip.loop_seam_source_continuity > 0.0);
        assert!(clip.loop_seam_voxel_continuity > 0.0);
        for frame in &clip.frames {
            assert!(!frame.source_timestamps_microseconds.is_empty());
            assert!(frame.source_voxel_silhouette_jaccard > 0.0);
            assert!(frame.normalized_extent_error.is_finite());
            assert!(frame.normalized_foot_anchor_error.is_finite());
            assert!(!frame.material_slots.is_empty());
        }
    }
}

fn projected_instance_frame(response: &serde_json::Value) -> u64 {
    response["projection"]["ops"]
        .as_array()
        .expect("projection operations should be an array")
        .iter()
        .find_map(|operation| match operation["op"].as_str() {
            Some("createVoxelObjectInstance") => operation["instance"]["frame"].as_u64(),
            Some("setVoxelObjectFrame") => operation["frame"].as_u64(),
            _ => None,
        })
        .expect("projection should create or advance the canonical voxel-object instance")
}

#[test]
fn high_fidelity_conversion_is_deterministic_and_much_denser() {
    let prepared = prepare_project_conversion(&root(), HIGH_FIDELITY_PROJECT_FILE)
        .expect("high-fidelity animated source should convert");
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
    assert_eq!(candidate.asset.grid.cell_size, 0.03125);
    assert_eq!(candidate.asset.grid.pivot, [47.5, 0.0, 47.5]);
    assert_eq!(candidate.sampled_frames, 16);
    assert_eq!(candidate.stored_frames, 15);
    assert_eq!(candidate.clips.len(), 3);
    assert_eq!(candidate.aggregate_voxels, 168_907);

    let baseline = prepare_project_conversion(&root(), DEFAULT_PROJECT_FILE)
        .expect("baseline animated source should convert");
    assert!(
        candidate.aggregate_voxels >= 10 * baseline.prepared.candidate().aggregate_voxels,
        "4x linear grid should hold at least 10x the baseline voxels"
    );
}

#[test]
fn high_fidelity_object_loads_plays_and_projects() {
    let evidence = verify_runtime_project(&root(), HIGH_FIDELITY_PROJECT_FILE)
        .expect("high-fidelity voxel object should load through the runtime");

    assert_eq!(evidence.frame_count, 15);
    assert_eq!(evidence.clip_count, 3);
    assert_eq!(evidence.unique_mesh_count, 14);
    assert_eq!(evidence.resolved_voxels, 158_178);
    assert_eq!(evidence.projection_operation_count, 3);
    assert_eq!(evidence.defined_voxel_objects, 1);
    assert_eq!(evidence.created_voxel_instances, 1);
    assert_eq!(evidence.playback_samples.len(), 5);
    assert!(
        evidence
            .playback_samples
            .iter()
            .all(|sample| sample.voxel_count > 10_000),
        "every sampled high-fidelity pose should exceed ten thousand voxels"
    );
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
fn format_study_prices_candidate_encodings_against_real_corpus() {
    let study = run_format_study(&root(), HIGH_FIDELITY_PROJECT_FILE)
        .expect("format study should admit the checked high-fidelity object");

    assert_eq!(study.unique_mesh_count, 14);
    assert_eq!(study.meshes.len(), 14);
    assert!(study.canonical_object_bytes > 12_000_000);

    // The packed stream envelope is exact: 4/3 of raw LE bytes plus envelope.
    let raw: usize = study
        .meshes
        .iter()
        .map(|mesh| {
            (usize::try_from(mesh.vertices).unwrap() * 6 + usize::try_from(mesh.indices).unwrap())
                * 4
        })
        .sum();
    let expected_packed: usize = study
        .meshes
        .iter()
        .map(|mesh| {
            (usize::try_from(mesh.vertices).unwrap() * 6 + usize::try_from(mesh.indices).unwrap())
                * 4
        })
        .map(|bytes| bytes.div_ceil(3) * 4 + 96)
        .sum();
    assert_eq!(study.streams.packed_base64_bytes, expected_packed);
    assert!(raw > 0);
    assert!(study.streams.binary_reference_bytes < study.streams.packed_base64_bytes);
    assert!(study.streams.binary_reference_bytes < study.streams.expanded_json_bytes);

    // Flipbook frames genuinely deform: most vertex data changes pose-to-pose,
    // index topology barely does. The harness must report that honestly.
    let delta = study.delta.expect("flipbook should produce delta evidence");
    assert!(delta.average_changed_vertex_fraction > 0.5);
    assert!(delta.average_changed_index_fraction < 0.05);
    assert!(delta.binary_savings_fraction > 0.25);
    // Timing passes are recorded, not thresholded (machine-specific).
    assert!(study.timing.expanded_json_parse_microseconds > 0);
    assert!(study.timing.packed_base64_bytes_decoded > 0);
    assert!(!study.interpretation_limits.is_empty());
}
