use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusty_engine_voxels::adapter::StudioAdapter;
use rusty_engine_voxels::DEFAULT_PROJECT_FILE;
use serde_json::{json, Value};

const PROTOCOL_VERSION: u64 = 14;
const DIRECTIONAL_ATLAS_PNG: &[u8] = include_bytes!("../content/textures/directional-atlas.png");
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
            &directional_texture_path(&project),
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
            &directional_texture_path(&project),
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
        &directional_texture_path(&project),
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
            &directional_texture_path(&project),
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
    let atlas_texture_hash = reopened["voxelSurfaceAuthoring"]["textures"][0]["contentHash"]
        .as_str()
        .expect("atlas texture hash")
        .to_owned();
    let atlas_material_hash = reopened["voxelSurfaceAuthoring"]["materials"][0]["contentHash"]
        .as_str()
        .expect("atlas material hash")
        .to_owned();
    let replaced = restarted
        .dispatch(upsert_request(
            "atlas-to-repeat",
            &atlas_project_hash,
            &directional_texture_path(&project),
            Some(&atlas_texture_hash),
            Some(&atlas_material_hash),
            repeat_mapping(),
        ))
        .expect("atlas surface should be replaceable by repeat");
    let replaced_project = &replaced["project"];
    let replaced_hash = project_hash(replaced_project);
    assert_eq!(
        replaced_project["voxelSurfaceAuthoring"]["materials"][0]["mapping"]["kind"],
        "repeat"
    );
    assert_eq!(
        replaced_project["voxelSurfaceAuthoring"]["textures"][0]["wrap"],
        "repeat"
    );
    assert_eq!(
        replaced_project["voxelSurfaceAuthoring"]["atlases"],
        json!([])
    );
    assert_eq!(
        replaced_project["voxelObjectAuthoring"]["instances"][0]["instance"]["materialOverrides"],
        json!([{ "materialSlot": 1, "materialAssetId": "material/checker" }])
    );
    let source_path = replaced_project["voxelSurfaceAuthoring"]["textures"][0]["sourcePath"]
        .as_str()
        .expect("replacement texture path");
    assert_eq!(
        fs::read(project.root.join(source_path)).expect("published replacement texture"),
        DIRECTIONAL_ATLAS_PNG
    );

    let mut replacement_reopen = StudioAdapter::default();
    let replacement_readout = open(&mut replacement_reopen, &project, "replacement-reopen");
    assert_eq!(project_hash(&replacement_readout), replaced_hash);
    assert_eq!(
        replacement_readout["voxelSurfaceAuthoring"]["atlases"],
        json!([])
    );
    assert_eq!(
        replacement_readout["voxelSurfaceAuthoring"]["materials"][0]["mapping"]["kind"],
        "repeat"
    );
    assert_eq!(
        replacement_readout["voxelObjectAuthoring"]["instances"][0]["instance"]
            ["materialOverrides"],
        json!([{ "materialSlot": 1, "materialAssetId": "material/checker" }])
    );

    let before_remove = project.project_bytes();
    let removal = replacement_reopen.dispatch(json!({
        "type": "removeVoxelSurfaceMaterial",
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": "remove-in-use",
        "expectedProjectHash": replaced_hash,
        "materialAssetId": "material/checker",
        "expectedMaterialContentHash": replacement_readout["voxelSurfaceAuthoring"]["materials"][0]["contentHash"],
        "textureAssetId": "texture/checker",
        "expectedTextureContentHash": replacement_readout["voxelSurfaceAuthoring"]["textures"][0]["contentHash"],
        "atlasAssetId": null,
        "expectedAtlasContentHash": null,
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
            &directional_texture_path(&project),
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

#[test]
fn protocol_14_directional_surface_failures_are_atomic_and_bounded() {
    let project = TempProject::new();
    let mut adapter = StudioAdapter::default();
    let opened = open(&mut adapter, &project, "open-adversarial");
    let original_hash = project_hash(&opened);
    let applied = adapter
        .dispatch(upsert_request(
            "apply-adversarial",
            &original_hash,
            &directional_texture_path(&project),
            None,
            None,
            atlas_mapping(None),
        ))
        .expect("directional atlas should publish");
    let applied_project = &applied["project"];
    let applied_hash = project_hash(applied_project);
    let texture_hash = applied_project["voxelSurfaceAuthoring"]["textures"][0]["contentHash"]
        .as_str()
        .expect("texture hash")
        .to_owned();
    let material_hash = applied_project["voxelSurfaceAuthoring"]["materials"][0]["contentHash"]
        .as_str()
        .expect("material hash")
        .to_owned();
    assert_eq!(
        applied_project["voxelSurfaceAuthoring"]["atlases"][0]["regions"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_vec4_close(
        &applied_project["voxelSurfaceAuthoring"]["materials"][0]["definition"]["style"]
            ["textureTint"],
        [0.75, 0.9, 1.0, 0.8],
    );
    assert_eq!(
        applied_project["voxelSurfaceAuthoring"]["materials"][0]["alphaMode"]["kind"],
        "mask"
    );
    assert!(
        (applied_project["voxelSurfaceAuthoring"]["materials"][0]["alphaMode"]["cutoff"]
            .as_f64()
            .expect("alpha cutoff")
            - 0.4)
            .abs()
            < 1.0e-6
    );

    let scrubbed = adapter
        .dispatch(json!({
            "type": "previewVoxelObjectInstance",
            "protocolVersion": PROTOCOL_VERSION,
            "requestId": "surface-frame-change",
            "expectedProjectHash": applied_hash,
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
        .expect("animated object should retain its surface while changing frames");
    assert!(scrubbed["projection"]["ops"]
        .as_array()
        .expect("projection operations")
        .iter()
        .any(|operation| operation["op"] == "setVoxelObjectFrame"));
    let reopened_after_scrub = open(
        &mut StudioAdapter::default(),
        &project,
        "surface-after-scrub",
    );
    assert_eq!(
        reopened_after_scrub["voxelObjectAuthoring"]["instances"][0]["instance"]
            ["materialOverrides"],
        json!([{ "materialSlot": 1, "materialAssetId": "material/checker" }])
    );

    let baseline_project = project.project_bytes();
    let cases = [
        (
            "missing-source",
            project.root.join("missing-directional.png"),
            atlas_mapping(None),
        ),
        (
            "malformed-reimport",
            write_fixture(&project, "malformed.png", b"not a png"),
            atlas_mapping(None),
        ),
        (
            "oversized-reimport",
            write_fixture(&project, "oversized.png", &vec![0_u8; 16 * 1024 * 1024 + 1]),
            atlas_mapping(None),
        ),
    ];
    for (request_id, source, mapping) in cases {
        let rejected = adapter.dispatch(upsert_request(
            request_id,
            &applied_hash,
            &source,
            Some(&texture_hash),
            Some(&material_hash),
            mapping,
        ));
        assert!(rejected.is_err(), "{request_id} must reject");
        assert_eq!(project.project_bytes(), baseline_project, "{request_id}");
    }

    let mut out_of_bounds = atlas_mapping(None);
    out_of_bounds["regions"][0]["contentMin"] = json!([15, 7]);
    let mut overlapping = atlas_mapping(None);
    overlapping["regions"][1]["contentMin"] = json!([6, 1]);
    let mut bleed = atlas_mapping(None);
    bleed["regions"][0]["padding"] = json!({ "left": 0, "right": 0, "bottom": 0, "top": 0 });
    for (request_id, mapping, filter) in [
        ("out-of-bounds", out_of_bounds, "nearest"),
        ("overlap", overlapping, "nearest"),
        ("linear-bleed", bleed, "linear"),
    ] {
        let mut request = upsert_request(
            request_id,
            &applied_hash,
            &directional_texture_path(&project),
            Some(&texture_hash),
            Some(&material_hash),
            mapping,
        );
        request["filter"] = json!(filter);
        assert!(
            adapter.dispatch(request).is_err(),
            "{request_id} must reject"
        );
        assert_eq!(project.project_bytes(), baseline_project, "{request_id}");
    }

    let stale = adapter.dispatch(upsert_request(
        "stale-revision",
        &original_hash,
        &directional_texture_path(&project),
        Some(&texture_hash),
        Some(&material_hash),
        atlas_mapping(None),
    ));
    assert!(stale.is_err());
    assert_eq!(project.project_bytes(), baseline_project);
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
                    "textureTint": [0.75, 0.9, 1.0, 0.8],
                    "emissionColor": [0.1, 0.25, 0.6, 1.0],
                    "roughness": 0.5,
                    "emissive": 0.35,
                    "uvStrategy": if mapping["kind"] == "atlas" { "atlas" } else { "planar" },
                },
            },
            "alphaMode": { "kind": "mask", "cutoff": 0.4 },
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
        "regions": [
            {
                "id": "warm-arrow",
                "contentMin": [1, 1],
                "contentExtent": [6, 6],
                "padding": { "left": 1, "right": 1, "bottom": 1, "top": 1 },
                "inset": "halfTexel",
            },
            {
                "id": "cool-arrow",
                "contentMin": [9, 1],
                "contentExtent": [6, 6],
                "padding": { "left": 1, "right": 1, "bottom": 1, "top": 1 },
                "inset": "halfTexel",
            }
        ],
        "regionId": "warm-arrow",
        "tileScaleCells": [1.0, 1.0],
        "tileOriginCells": [0.0, 0.0],
    })
}

fn directional_texture_path(project: &TempProject) -> PathBuf {
    project.root.join("content/textures/directional-atlas.png")
}

fn write_fixture(project: &TempProject, name: &str, bytes: &[u8]) -> PathBuf {
    let path = project.root.join(name);
    fs::write(&path, bytes).expect("write rejection fixture");
    path
}

fn assert_vec4_close(value: &Value, expected: [f64; 4]) {
    let actual = value.as_array().expect("four-vector");
    assert_eq!(actual.len(), 4);
    for (actual, expected) in actual.iter().zip(expected) {
        assert!((actual.as_f64().expect("finite vector value") - expected).abs() < 1.0e-6);
    }
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
