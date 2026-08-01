use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusty_engine_voxels::adapter::StudioAdapter;
use rusty_engine_voxels::model::MAX_JSON_SAFE_ENTITY_ID;
use rusty_engine_voxels::project::{load_project, save_project};
use rusty_engine_voxels::DEFAULT_PROJECT_FILE;
use serde_json::{json, Value};

const STUDIO_PROTOCOL_VERSION: u64 = 14;
const ENTRY_SCENE: &str = "scene/voxel-lab";
const ASSET_ID: &str = "voxel-object/retro-character";
static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "rusty-engine-voxels-protocol-14-{}-{}",
            std::process::id(),
            NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("temporary project root should be created");
        copy_tree(&repository_root().join("content"), &root.join("content"));
        Self { root }
    }

    fn project_bytes(&self) -> Vec<u8> {
        fs::read(self.root.join(DEFAULT_PROJECT_FILE))
            .expect("temporary project bytes should be readable")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn protocol_14_batch_attachment_is_ordered_atomic_and_restart_stable() {
    let project = TempProject::new();
    let mut adapter = StudioAdapter::default();
    let opened = open_project(&mut adapter, &project, "batch-open");
    let initial_hash = project_hash(&opened);
    let initial_revision = opened["project"]["identity"]["sceneRevision"]
        .as_u64()
        .expect("project revision should be an integer");

    let described = adapter
        .dispatch(json!({
            "type": "describe",
            "protocolVersion": STUDIO_PROTOCOL_VERSION,
            "requestId": "batch-describe",
        }))
        .expect("protocol 14 describe should succeed");
    assert_eq!(described["adapter"]["protocolVersion"], 14);
    assert!(described["adapter"]["operations"]
        .as_array()
        .expect("operations should be an array")
        .contains(&json!("attachVoxelObjectInstances")));

    let attached = adapter
        .dispatch(batch_request(
            "batch-attach",
            &initial_hash,
            vec![
                placement("zebra-request-first", ASSET_ID),
                placement("alpha-request-second", ASSET_ID),
            ],
        ))
        .expect("valid protocol 14 batch should attach atomically");
    assert_eq!(attached["type"], "projectMutationApplied");
    assert_eq!(attached["receipt"]["kind"], "voxelObjectInstancesAttached");
    assert_eq!(
        attached["receipt"]["placements"],
        json!([
            {
                "sceneId": ENTRY_SCENE,
                "instanceId": "zebra-request-first",
                "assetId": ASSET_ID,
                "frameKind": "default",
                "ownerEntityId": 2,
            },
            {
                "sceneId": ENTRY_SCENE,
                "instanceId": "alpha-request-second",
                "assetId": ASSET_ID,
                "frameKind": "default",
                "ownerEntityId": 3,
            },
        ])
    );
    assert_eq!(
        attached["project"]["identity"]["sceneRevision"],
        initial_revision + 1
    );
    assert_eq!(
        attached["project"]["voxelObjectAuthoring"]["instances"]
            .as_array()
            .map(Vec::len),
        Some(3)
    );

    let saved = load_project(&project.root, DEFAULT_PROJECT_FILE)
        .expect("published batch should remain a valid project");
    assert_eq!(saved.project.revision, initial_revision + 1);
    assert_eq!(saved.project.instances.len(), 3);
    assert_eq!(
        saved.project.instances[0].instance_id,
        "alpha-request-second"
    );
    assert_eq!(saved.project.instances[0].entity_id, 3);
    assert_eq!(
        saved.project.instances[2].instance_id,
        "zebra-request-first"
    );
    assert_eq!(saved.project.instances[2].entity_id, 2);

    let mut restarted = StudioAdapter::default();
    let reopened = open_project(&mut restarted, &project, "batch-reopen");
    assert_eq!(project_hash(&reopened), project_hash(&attached));
    assert_eq!(
        reopened["project"]["voxelObjectAuthoring"]["instances"],
        attached["project"]["voxelObjectAuthoring"]["instances"]
    );
}

#[test]
fn protocol_13_batch_rejections_leave_canonical_bytes_unchanged() {
    let project = TempProject::new();
    let mut adapter = StudioAdapter::default();
    let opened = open_project(&mut adapter, &project, "rejection-open");
    let hash = project_hash(&opened);
    let before = project.project_bytes();

    let invalid_requests = vec![
        batch_request("empty", &hash, Vec::new()),
        batch_request(
            "over-limit",
            &hash,
            (0..33)
                .map(|index| placement(&format!("over-limit-{index}"), ASSET_ID))
                .collect(),
        ),
        batch_request(
            "duplicate",
            &hash,
            vec![
                placement("duplicate", ASSET_ID),
                placement("duplicate", ASSET_ID),
            ],
        ),
        batch_request(
            "existing-collision",
            &hash,
            vec![placement("retro-character", ASSET_ID)],
        ),
        batch_request(
            "invalid-later-asset",
            &hash,
            vec![
                placement("valid-first", ASSET_ID),
                placement("invalid-second", "voxel-object/missing"),
            ],
        ),
        batch_request(
            "invalid-later-transform",
            &hash,
            vec![
                placement("valid-transform-first", ASSET_ID),
                placement_with(
                    "invalid-transform-second",
                    ASSET_ID,
                    ENTRY_SCENE,
                    [0.0, 0.0, 0.0],
                    Vec::new(),
                ),
            ],
        ),
        batch_request(
            "missing-scene",
            &hash,
            vec![placement_with(
                "missing-scene",
                ASSET_ID,
                "scene/missing",
                [1.0, 1.0, 1.0],
                Vec::new(),
            )],
        ),
        batch_request(
            "material-override",
            &hash,
            vec![placement_with(
                "material-override",
                ASSET_ID,
                ENTRY_SCENE,
                [1.0, 1.0, 1.0],
                vec![json!({ "slot": 0 })],
            )],
        ),
        batch_request(
            "stale",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            vec![placement("stale", ASSET_ID)],
        ),
        {
            let mut request = batch_request(
                "unknown-field",
                &hash,
                vec![placement("unknown-field", ASSET_ID)],
            );
            request["ambientMutation"] = json!(true);
            request
        },
    ];

    for request in invalid_requests {
        let request_id = request["requestId"]
            .as_str()
            .expect("request id should be text")
            .to_owned();
        assert!(
            adapter.dispatch(request).is_err(),
            "{request_id} should reject"
        );
        assert_eq!(
            project.project_bytes(),
            before,
            "{request_id} must not publish partial project bytes"
        );
    }

    let read = adapter
        .dispatch(json!({
            "type": "readProject",
            "protocolVersion": STUDIO_PROTOCOL_VERSION,
            "requestId": "rejection-read",
        }))
        .expect("project should remain readable after all rejections");
    assert_eq!(project_hash(&read), hash);
    assert_eq!(
        read["project"]["voxelObjectAuthoring"]["instances"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn protocol_13_batch_rejects_exhausted_json_safe_owner_identity() {
    let project = TempProject::new();
    let loaded =
        load_project(&project.root, DEFAULT_PROJECT_FILE).expect("temporary project should load");
    let mut exhausted = loaded.project.clone();
    exhausted.instances[0].entity_id = MAX_JSON_SAFE_ENTITY_ID;
    let saved = save_project(&loaded, &exhausted)
        .expect("maximum JSON-safe owner should remain a valid canonical project");
    let before = project.project_bytes();

    let mut adapter = StudioAdapter::default();
    let opened = open_project(&mut adapter, &project, "owner-exhaustion-open");
    assert_eq!(project_hash(&opened), saved.project_hash);
    let rejected = adapter.dispatch(batch_request(
        "owner-exhaustion",
        &saved.project_hash,
        vec![placement("cannot-allocate-owner", ASSET_ID)],
    ));
    assert!(rejected.is_err());
    assert_eq!(project.project_bytes(), before);
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir(destination).expect("copy destination should be created");
    for entry in fs::read_dir(source).expect("copy source should be readable") {
        let entry = entry.expect("copy entry should be readable");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry
            .file_type()
            .expect("copy entry metadata should be readable")
            .is_dir()
        {
            copy_tree(&source_path, &destination_path);
        } else {
            fs::copy(&source_path, &destination_path).expect("copy file should succeed");
        }
    }
}

fn open_project(adapter: &mut StudioAdapter, project: &TempProject, request_id: &str) -> Value {
    adapter
        .dispatch(json!({
            "type": "openProject",
            "protocolVersion": STUDIO_PROTOCOL_VERSION,
            "requestId": request_id,
            "root": project.root,
            "projectFile": DEFAULT_PROJECT_FILE,
        }))
        .expect("temporary project should open")
}

fn project_hash(response: &Value) -> String {
    response["project"]["identity"]["projectHash"]
        .as_str()
        .expect("response should carry a project hash")
        .to_owned()
}

fn batch_request(request_id: &str, expected_project_hash: &str, placements: Vec<Value>) -> Value {
    json!({
        "type": "attachVoxelObjectInstances",
        "protocolVersion": STUDIO_PROTOCOL_VERSION,
        "requestId": request_id,
        "expectedProjectHash": expected_project_hash,
        "placements": placements,
    })
}

fn placement(instance_id: &str, asset_id: &str) -> Value {
    placement_with(
        instance_id,
        asset_id,
        ENTRY_SCENE,
        [1.0, 1.0, 1.0],
        Vec::new(),
    )
}

fn placement_with(
    instance_id: &str,
    asset_id: &str,
    scene_id: &str,
    scale: [f32; 3],
    material_overrides: Vec<Value>,
) -> Value {
    json!({
        "sceneId": scene_id,
        "instance": {
            "instanceId": instance_id,
            "voxelObjectAssetId": asset_id,
            "frame": { "kind": "default" },
            "translation": [0.0, 0.0, 0.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "scale": scale,
            "materialOverrides": material_overrides,
        },
    })
}
