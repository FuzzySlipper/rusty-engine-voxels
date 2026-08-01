use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusty_engine_voxels::adapter::StudioAdapter;
use rusty_engine_voxels::DEFAULT_PROJECT_FILE;
use serde_json::{json, Value};

const PROTOCOL_VERSION: u64 = 14;
const CHECKER_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 244, 34, 127, 138, 0, 0, 0, 15, 73, 68, 65, 84, 120, 156, 99, 248, 207, 0, 68, 255, 25,
    26, 0, 16, 121, 3, 126, 153, 113, 48, 89, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
static NEXT_TEMP_ROOT: AtomicU64 = AtomicU64::new(1);

struct TempProject {
    root: PathBuf,
}

impl TempProject {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "rusty-engine-voxels-surface-{}-{}",
            std::process::id(),
            NEXT_TEMP_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).expect("temporary root");
        copy_tree(&repository_root().join("content"), &root.join("content"));
        fs::write(root.join("checker.png"), CHECKER_PNG).expect("texture fixture");
        Self { root }
    }

    fn project_bytes(&self) -> Vec<u8> {
        fs::read(self.root.join(DEFAULT_PROJECT_FILE)).expect("project bytes")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn protocol_14_surface_closure_is_strict_atomic_and_restart_stable() {
    let project = TempProject::new();
    let mut adapter = StudioAdapter::default();
    let opened = open(&mut adapter, &project, "open");
    let original_hash = project_hash(&opened);

    let repeated = adapter
        .dispatch(upsert_request(
            "repeat",
            &original_hash,
            &project.root.join("checker.png"),
            None,
            None,
            repeat_mapping(),
        ))
        .expect("repeat surface should publish");
    let repeated_project = &repeated["project"];
    let repeat_hash = project_hash(repeated_project);
    let texture_hash = repeated_project["voxelSurfaceAuthoring"]["textures"][0]["contentHash"]
        .as_str()
        .expect("texture hash")
        .to_owned();
    let material_hash = repeated_project["voxelSurfaceAuthoring"]["materials"][0]["contentHash"]
        .as_str()
        .expect("material hash")
        .to_owned();
    assert_eq!(repeated["receipt"]["kind"], "voxelSurfaceMaterialUpserted");
    assert_eq!(
        repeated_project["voxelSurfaceAuthoring"]["materials"][0]["mapping"]["kind"],
        "repeat"
    );
    assert_eq!(
        repeated_project["voxelObjectAuthoring"]["instances"][0]["instance"]["materialOverrides"],
        json!([{ "materialSlot": 1, "materialAssetId": "material/checker" }])
    );
    assert_eq!(
        repeated_project["textureResources"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert!(repeated_project["projection"]["ops"]
        .as_array()
        .expect("projection ops")
        .iter()
        .any(|operation| operation["op"] == "defineTexture"));

    let before_stale = project.project_bytes();
    let stale = adapter
        .dispatch(upsert_request(
            "stale",
            &original_hash,
            &project.root.join("checker.png"),
            Some(&texture_hash),
            Some(&material_hash),
            repeat_mapping(),
        ))
        .expect_err("stale project hash must reject");
    assert!(format!("{stale:?}").contains("project.staleHash"));
    assert_eq!(project.project_bytes(), before_stale);

    let mut overlapping = atlas_mapping(None);
    overlapping["regions"]
        .as_array_mut()
        .expect("atlas regions")
        .push(json!({
            "id": "overlap",
            "contentMin": [0, 0],
            "contentExtent": [1, 1],
            "padding": { "left": 0, "right": 0, "bottom": 0, "top": 0 },
            "inset": "halfTexel",
        }));
    let before_invalid_atlas = project.project_bytes();
    let invalid_atlas = adapter.dispatch(upsert_request(
        "invalid-atlas",
        &repeat_hash,
        &project.root.join("checker.png"),
        Some(&texture_hash),
        Some(&material_hash),
        overlapping,
    ));
    assert!(invalid_atlas.is_err());
    assert_eq!(project.project_bytes(), before_invalid_atlas);

    let atlas = adapter
        .dispatch(upsert_request(
            "atlas",
            &repeat_hash,
            &project.root.join("checker.png"),
            Some(&texture_hash),
            Some(&material_hash),
            atlas_mapping(None),
        ))
        .expect("atlas surface should publish");
    let atlas_project = &atlas["project"];
    let atlas_project_hash = project_hash(atlas_project);
    assert_eq!(
        atlas_project["voxelSurfaceAuthoring"]["materials"][0]["mapping"]["kind"],
        "atlas"
    );
    assert_eq!(
        atlas_project["voxelSurfaceAuthoring"]["atlases"][0]["regions"][0]["inset"],
        "halfTexel"
    );

    let mut restarted = StudioAdapter::default();
    let reopened = open(&mut restarted, &project, "reopen");
    assert_eq!(project_hash(&reopened), atlas_project_hash);
    assert_eq!(
        reopened["voxelSurfaceAuthoring"]["materials"][0]["mapping"]["kind"],
        "atlas"
    );
    let before_remove = project.project_bytes();
    let removal = restarted.dispatch(json!({
        "type": "removeVoxelSurfaceMaterial",
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": "remove-in-use",
        "expectedProjectHash": atlas_project_hash,
        "materialAssetId": "material/checker",
        "expectedMaterialContentHash": reopened["voxelSurfaceAuthoring"]["materials"][0]["contentHash"],
        "textureAssetId": "texture/checker",
        "expectedTextureContentHash": reopened["voxelSurfaceAuthoring"]["textures"][0]["contentHash"],
        "atlasAssetId": "sprite-sheet/checker",
        "expectedAtlasContentHash": reopened["voxelSurfaceAuthoring"]["atlases"][0]["contentHash"],
    }));
    assert!(
        format!("{:?}", removal.expect_err("in-use removal must reject"))
            .contains("surface.materialInUse")
    );
    assert_eq!(project.project_bytes(), before_remove);
}

#[test]
fn protocol_14_reopen_rejects_texture_drift_without_project_mutation() {
    let project = TempProject::new();
    let mut adapter = StudioAdapter::default();
    let opened = open(&mut adapter, &project, "open-drift");
    let applied = adapter
        .dispatch(upsert_request(
            "apply-drift",
            &project_hash(&opened),
            &project.root.join("checker.png"),
            None,
            None,
            repeat_mapping(),
        ))
        .expect("surface should publish");
    let source_path = applied["project"]["voxelSurfaceAuthoring"]["textures"][0]["sourcePath"]
        .as_str()
        .expect("texture path");
    let project_bytes = project.project_bytes();
    fs::write(project.root.join(source_path), b"not a png").expect("corrupt texture fixture");
    let mut restarted = StudioAdapter::default();
    let rejected = restarted.dispatch(json!({
        "type": "openProject",
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": "open-corrupt",
        "root": project.root,
        "projectFile": DEFAULT_PROJECT_FILE,
    }));
    assert!(rejected.is_err());
    assert_eq!(project.project_bytes(), project_bytes);
}

fn upsert_request(
    request_id: &str,
    expected_project_hash: &str,
    texture_path: &Path,
    expected_texture_hash: Option<&str>,
    expected_material_hash: Option<&str>,
    mapping: Value,
) -> Value {
    json!({
        "type": "upsertVoxelSurfaceMaterial",
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": request_id,
        "expectedProjectHash": expected_project_hash,
        "textureAssetId": "texture/checker",
        "expectedTextureContentHash": expected_texture_hash,
        "textureSource": { "scope": "host", "path": texture_path },
        "filter": "nearest",
        "material": {
            "materialAssetId": "material/checker",
            "expectedMaterialContentHash": expected_material_hash,
            "definition": {
                "authority": {
                    "solid": false,
                    "collidable": false,
                    "occludes": false,
                    "structuralClass": "decorative",
                },
                "style": {
                    "color": [1.0, 1.0, 1.0, 1.0],
                    "texture": null,
                    "textureTint": [1.0, 1.0, 1.0, 1.0],
                    "emissionColor": [0.0, 0.0, 0.0, 1.0],
                    "roughness": 0.5,
                    "emissive": 0.0,
                    "uvStrategy": if mapping["kind"] == "atlas" { "atlas" } else { "planar" },
                },
            },
            "alphaMode": { "kind": "opaque" },
            "mapping": mapping,
        },
        "assignment": {
            "sceneId": "scene/voxel-lab",
            "instanceId": "retro-character",
            "materialSlot": 1,
        },
    })
}

fn repeat_mapping() -> Value {
    json!({
        "kind": "repeat",
        "tileScaleCells": [2.0, 1.0],
        "tileOriginCells": [0.25, -0.5],
    })
}

fn atlas_mapping(expected_hash: Option<&str>) -> Value {
    json!({
        "kind": "atlas",
        "atlasAssetId": "sprite-sheet/checker",
        "expectedAtlasContentHash": expected_hash,
        "regions": [{
            "id": "all",
            "contentMin": [0, 0],
            "contentExtent": [2, 1],
            "padding": { "left": 0, "right": 0, "bottom": 0, "top": 0 },
            "inset": "halfTexel",
        }],
        "regionId": "all",
        "tileScaleCells": [1.0, 1.0],
        "tileOriginCells": [0.0, 0.0],
    })
}

fn open(adapter: &mut StudioAdapter, project: &TempProject, request_id: &str) -> Value {
    let mut request = json!({
        "type": "openProject",
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": request_id,
        "root": project.root,
        "projectFile": DEFAULT_PROJECT_FILE,
    });
    adapter.dispatch(request.take()).expect("open project")["project"].clone()
}

fn project_hash(project: &Value) -> String {
    project["identity"]["projectHash"]
        .as_str()
        .expect("project hash")
        .to_owned()
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("destination directory");
    for entry in fs::read_dir(source).expect("source directory") {
        let entry = entry.expect("directory entry");
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&source_path, &destination_path);
        } else {
            fs::copy(source_path, destination_path).expect("copy file");
        }
    }
}
