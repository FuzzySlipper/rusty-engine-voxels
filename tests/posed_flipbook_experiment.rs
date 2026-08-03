//! Posed flipbook experiment: an authored pose-spec document compiles the
//! knight kit into a canonical, Studio-loadable voxel object — no rig, no
//! mesh conversion. Proves validation, determinism (the debug build
//! reproduces the pinned hash the release CLI published), Engine admission,
//! immutable publication, and that the checked knight-flipbook project
//! resolves the object through the runtime path Studio uses.

use std::path::{Path, PathBuf};

use rusty_engine_voxels::flipbook::{
    compile_posed_flipbook, publish_compiled_flipbook, FlipbookCompileSettings,
};
use rusty_engine_voxels::kit::load_kit;
use rusty_engine_voxels::posed::{assemble_pose_spec, downsample_rough_frame, load_pose_spec};
use rusty_engine_voxels::pose::RasterSettings;
use rusty_engine_voxels::project::{read_bounded, safe_join};
use rusty_engine_voxels::runtime::load_runtime_project;
use voxel_object_runtime::{admit_voxel_object_json, VoxelObjectRuntimeLimits};

const SPEC: &str = "content/characters/knight/poses/walk.poses.json";
const PROJECT: &str = "content/projects/knight-flipbook.project.json";
const PINNED_CONTENT_HASH: &str =
    "sha256:55a5ddbd4da96a8a3cafce52c2adb71d7eaf44ee00b0c035b705c72075788aec";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn compile_checked_spec() -> rusty_engine_voxels::flipbook::CompiledFlipbook {
    let root = root();
    let spec = load_pose_spec(&root, SPEC).expect("checked pose spec loads");
    let kit = load_kit(&root, &spec.kit).expect("checked knight kit loads");
    spec.validate(&kit).expect("checked pose spec validates");
    let frames = assemble_pose_spec(&kit, &spec, &RasterSettings::default())
        .expect("pose spec assembles");
    assert_eq!(frames.len(), 4);
    let frames = frames
        .iter()
        .map(|frame| downsample_rough_frame(frame, spec.cell_downsample))
        .collect::<Vec<_>>();
    // Ground contact: no downsampled pose sinks meaningfully below the plane.
    for frame in &frames {
        let (min, _) = frame.bounds().expect("frame bounds");
        assert!(
            min[1] >= -4,
            "pose sinks below ground: {:?}",
            frame.bounds()
        );
    }
    let spec_bytes =
        read_bounded(&safe_join(&root, SPEC).unwrap(), 256 * 1024, "pose spec").expect("spec bytes");
    let settings = FlipbookCompileSettings {
        asset_id: format!("voxel-object/posed-{}", spec.id),
        clip_id: spec.clip_id.clone(),
        clip_name: spec.clip_name.clone(),
        source_path: SPEC.to_owned(),
        chunk_size: 16,
        anchors: Vec::new(),
        body_collision: None,
        hit_regions: Vec::new(),
    };
    let cell_size = kit.convention.voxel_size_meters * f64::from(spec.cell_downsample);
    compile_posed_flipbook(&kit, &frames, &settings, &spec_bytes, cell_size)
        .expect("posed flipbook compiles")
}

#[test]
fn pose_spec_validation_names_offending_content() {
    let root = root();
    let kit = load_kit(&root, "content/characters/knight/character.json").expect("kit");
    let base = load_pose_spec(&root, SPEC).expect("spec");
    base.validate(&kit).expect("checked spec validates");

    let mut unknown_part = base.clone();
    unknown_part.frames[0].deltas[0].part = "dragon".to_owned();
    let error = unknown_part.validate(&kit).expect_err("unknown delta part");
    assert!(error.contains("dragon"), "{error}");

    let mut multi_level = base.clone();
    multi_level.frames[0]
        .chains
        .push(rusty_engine_voxels::posed::PartChain {
            child: "left_arm".to_owned(),
            parent: "torso".to_owned(),
        });
    let error = multi_level.validate(&kit).expect_err("multi-level chain");
    assert!(error.contains("single-level"), "{error}");

    let mut self_chain = base.clone();
    self_chain.frames[0].chains[0].parent = "left_hand".to_owned();
    let error = self_chain.validate(&kit).expect_err("self chain");
    assert!(error.contains("itself"), "{error}");

    let mut zero_duration = base.clone();
    zero_duration.frames[1].duration_microseconds = 0;
    let error = zero_duration.validate(&kit).expect_err("zero duration");
    assert!(error.contains("duration"), "{error}");

    let mut duplicate_name = base.clone();
    duplicate_name.frames[1].name = duplicate_name.frames[0].name.clone();
    let error = duplicate_name.validate(&kit).expect_err("duplicate frame name");
    assert!(error.contains("more than once"), "{error}");

    let mut bad_downsample = base.clone();
    bad_downsample.cell_downsample = 3;
    let error = bad_downsample.validate(&kit).expect_err("bad downsample");
    assert!(error.contains("cellDownsample"), "{error}");

    let mut bad_clip = base.clone();
    bad_clip.clip_id = "walk".to_owned();
    let error = bad_clip.validate(&kit).expect_err("bad clip id");
    assert!(error.contains("clip/"), "{error}");
}

#[test]
fn posed_flipbook_compiles_publishes_and_loads_in_studio_runtime() {
    let compiled = compile_checked_spec();

    // The pinned hash is the determinism gate: the release CLI published this
    // exact object, and the debug test build reproduces it bit-for-bit.
    assert_eq!(compiled.asset.content_hash, PINNED_CONTENT_HASH);
    assert_eq!(compiled.asset.clips.len(), 1);
    assert_eq!(compiled.asset.clips[0].frames.len(), 4);
    assert_eq!(compiled.asset.clips[0].frames_per_second, 6.25);
    assert!(compiled.canonical_json.len() <= voxel_asset::MAX_VOXEL_OBJECT_ARTIFACT_BYTES);

    // Engine strict admission, the same path the Studio adapter uses at load.
    admit_voxel_object_json(&compiled.canonical_json, VoxelObjectRuntimeLimits::default())
        .expect("compiled object admits");

    // Content-addressed publication is immutable and idempotent.
    let publication_root = root().join("target/tmp/posed-flipbook-publication");
    if publication_root.exists() {
        std::fs::remove_dir_all(&publication_root).expect("remove stale bounded test directory");
    }
    std::fs::create_dir_all(&publication_root).expect("create test directory");
    let first = publish_compiled_flipbook(&publication_root, "objects", &compiled)
        .expect("first publication");
    assert_eq!(first.content_hash, PINNED_CONTENT_HASH);
    let second = publish_compiled_flipbook(&publication_root, "objects", &compiled)
        .expect("identical republication is a no-op");
    assert_eq!(first, second);
    let mut tampered = compiled.clone();
    tampered.canonical_json.push('\n');
    assert!(publish_compiled_flipbook(&publication_root, "objects", &tampered).is_err());
    std::fs::remove_dir_all(&publication_root).expect("remove bounded test directory");

    // The checked Studio project resolves the object through the runtime
    // path the adapter uses for openProject.
    let runtime = load_runtime_project(&root(), PROJECT)
        .expect("knight flipbook project loads through the runtime path");
    assert_eq!(runtime.loaded.project.instances.len(), 1);
    let instance = &runtime.loaded.project.instances[0];
    assert_eq!(instance.instance_id, "knight-posed-walk");
    assert_eq!(instance.voxel_object_asset_id, "voxel-object/posed-knight-walk");
    assert!(
        runtime.objects.contains_key("voxel-object/posed-knight-walk"),
        "the posed flipbook object is admitted and registered"
    );
}
