use std::collections::BTreeMap;
use std::path::PathBuf;

use rusty_engine::voxel_convert;
use rusty_engine_voxels::assemble::socket_constrained_part_placements;
use rusty_engine_voxels::kit::{load_kit, neutral_part_transforms};
use rusty_engine_voxels::pose::{RigMap, RigidTransform};
use rusty_engine_voxels::video_motion::FittedMotion;
use serde::Serialize;
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};

const ROTATION_GAIN: f64 = 0.25;
const TRANSLATION_GAIN: f64 = 0.25;
const METERS_TO_CELLS: f64 = 25.0;

const PARTS: [(&str, &str, &str, &str, bool); 13] = [
    ("head", "head", "neck", "head", false),
    ("torso", "pelvis", "pelvis", "neck", false),
    ("pelvis", "pelvis", "pelvis", "neck", false),
    (
        "left_upper_arm",
        "leftShoulder",
        "leftShoulder",
        "leftElbow",
        true,
    ),
    (
        "left_lower_arm",
        "leftElbow",
        "leftElbow",
        "leftWrist",
        true,
    ),
    (
        "right_upper_arm",
        "rightShoulder",
        "rightShoulder",
        "rightElbow",
        true,
    ),
    (
        "right_lower_arm",
        "rightElbow",
        "rightElbow",
        "rightWrist",
        true,
    ),
    ("left_upper_leg", "leftHip", "leftHip", "leftKnee", true),
    ("left_lower_leg", "leftKnee", "leftKnee", "leftAnkle", true),
    ("right_upper_leg", "rightHip", "rightHip", "rightKnee", true),
    (
        "right_lower_leg",
        "rightKnee",
        "rightKnee",
        "rightAnkle",
        true,
    ),
    ("rifle", "weaponGrip", "weaponGrip", "weaponMuzzle", true),
    ("backpack", "torsoMidpoint", "pelvis", "neck", false),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyMotion {
    schema_version: u32,
    source_video_sha256: String,
    landmark_model_sha256: String,
    calibration_source_sha256: String,
    clip_id: String,
    bind_clip_id: String,
    frames: Vec<ProxyFrame>,
    retarget_policy: RetargetPolicy,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyFrame {
    frame_index: u32,
    timestamp_microseconds: u64,
    part_transforms: BTreeMap<String, RigidTransform>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RetargetPolicy {
    meters_to_cells: f64,
    rotation_gain: f64,
    translation_gain: f64,
    calibration_clip: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let root = PathBuf::from(arguments.next().ok_or("missing repository root")?);
    let fitted_path = PathBuf::from(arguments.next().ok_or("missing fitted motion")?);
    let output = PathBuf::from(arguments.next().ok_or("missing proxy output")?);
    if arguments.next().is_some() {
        return Err("expected <repository-root> <fitted-motion> <proxy-output>".into());
    }
    let fitted: FittedMotion = serde_json::from_str(&std::fs::read_to_string(fitted_path)?)?;
    let kit_path = "content/characters/rifleman/character.json";
    let rig_path = "content/characters/rifleman/rig-map.json";
    let source_path = "content/sources/kenney-retro-character/character-medium.glb";
    let kit = load_kit(&root, kit_path)?;
    let rig_map: RigMap = serde_json::from_str(&std::fs::read_to_string(root.join(rig_path))?)?;
    let source_bytes = std::fs::read(root.join(source_path))?;
    let imported = import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: "mesh-animation/retro-character".to_owned(),
        asset_version: 1,
        source_path: source_path.to_owned(),
        format: MeshSourceFormat::Glb,
        source_bytes,
        expected_source_sha256: None,
        mesh_primitive: None,
    })?;
    let clip_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .ok_or("run clip is missing")?;
    let placements =
        socket_constrained_part_placements(&kit, &rig_map, &imported.model, clip_index, 0)?;
    let neutral = neutral_part_transforms(&kit)?;
    let calibration = placements
        .into_iter()
        .map(|(part_id, placement)| {
            let (rotation, translation) = neutral[&part_id];
            let neutral_transform = RigidTransform {
                rotation,
                translation: translation.map(|value| value as f64),
            };
            (part_id, placement.then(neutral_transform.inverse()))
        })
        .collect::<BTreeMap<_, _>>();
    let first = &fitted.frames[0].joints;
    let first_observations = PARTS
        .iter()
        .map(|part| (part.0, observed_transform(part, first)))
        .collect::<BTreeMap<_, _>>();
    let frames = fitted
        .frames
        .iter()
        .map(|frame| {
            let part_transforms = PARTS
                .iter()
                .map(|part| {
                    let observed = observed_transform(part, &frame.joints);
                    let baseline = first_observations[part.0];
                    let delta_rotation =
                        quat_mul(observed.rotation, quat_conjugate(baseline.rotation));
                    let calibrated = calibration[part.0];
                    (
                        part.0.to_owned(),
                        RigidTransform {
                            rotation: quat_mul(delta_rotation, calibrated.rotation),
                            translation: [
                                calibrated.translation[0]
                                    + (observed.translation[0] - baseline.translation[0])
                                        * TRANSLATION_GAIN,
                                calibrated.translation[1]
                                    + (observed.translation[1] - baseline.translation[1])
                                        * TRANSLATION_GAIN,
                                calibrated.translation[2]
                                    + (observed.translation[2] - baseline.translation[2])
                                        * TRANSLATION_GAIN,
                            ],
                        },
                    )
                })
                .collect();
            ProxyFrame {
                frame_index: frame.frame_index,
                timestamp_microseconds: frame.timestamp_microseconds,
                part_transforms,
            }
        })
        .collect();
    let proxy = ProxyMotion {
        schema_version: 1,
        source_video_sha256: fitted.source_video_sha256,
        landmark_model_sha256: fitted.landmark_model_sha256,
        calibration_source_sha256: imported.model.source_sha256,
        clip_id: "fitted_run".to_owned(),
        bind_clip_id: "proxy_bind".to_owned(),
        frames,
        retarget_policy: RetargetPolicy {
            meters_to_cells: METERS_TO_CELLS,
            rotation_gain: ROTATION_GAIN,
            translation_gain: TRANSLATION_GAIN,
            calibration_clip: "run@0us".to_owned(),
        },
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, serde_json::to_string_pretty(&proxy)? + "\n")?;
    Ok(())
}

fn observed_transform(
    part: &(&str, &str, &str, &str, bool),
    joints: &BTreeMap<String, [f64; 3]>,
) -> RigidTransform {
    let position = if part.1 == "torsoMidpoint" {
        scale(add(joints["pelvis"], joints["neck"]), 0.5)
    } else {
        joints[part.1]
    };
    let rotation = if part.4 {
        scaled_rotation(rotation_from_negative_y(subtract(
            joints[part.3],
            joints[part.2],
        )))
    } else {
        [0.0, 0.0, 0.0, 1.0]
    };
    RigidTransform {
        rotation,
        translation: scale(blender_to_gltf(position), METERS_TO_CELLS),
    }
}

fn blender_to_gltf(value: [f64; 3]) -> [f64; 3] {
    [value[0], value[2], -value[1]]
}

fn rotation_from_negative_y(direction: [f64; 3]) -> [f64; 4] {
    let direction = normalize(blender_to_gltf(direction));
    let dot = -direction[1];
    if dot < -0.999_999 {
        return [1.0, 0.0, 0.0, 0.0];
    }
    quat_normalize([-direction[2], 0.0, direction[0], 1.0 + dot])
}

fn scaled_rotation(quaternion: [f64; 4]) -> [f64; 4] {
    let vector_length =
        (quaternion[0].powi(2) + quaternion[1].powi(2) + quaternion[2].powi(2)).sqrt();
    if vector_length < 1.0e-9 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    let angle = 2.0 * vector_length.atan2(quaternion[3]) * ROTATION_GAIN;
    let sine = (angle / 2.0).sin();
    [
        quaternion[0] / vector_length * sine,
        quaternion[1] / vector_length * sine,
        quaternion[2] / vector_length * sine,
        (angle / 2.0).cos(),
    ]
}

fn quat_conjugate(value: [f64; 4]) -> [f64; 4] {
    [-value[0], -value[1], -value[2], value[3]]
}

fn quat_mul(left: [f64; 4], right: [f64; 4]) -> [f64; 4] {
    let (lx, ly, lz, lw) = (left[0], left[1], left[2], left[3]);
    let (rx, ry, rz, rw) = (right[0], right[1], right[2], right[3]);
    quat_normalize([
        lw * rx + lx * rw + ly * rz - lz * ry,
        lw * ry - lx * rz + ly * rw + lz * rx,
        lw * rz + lx * ry - ly * rx + lz * rw,
        lw * rw - lx * rx - ly * ry - lz * rz,
    ])
}

fn quat_normalize(value: [f64; 4]) -> [f64; 4] {
    let length = value
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt();
    value.map(|component| component / length)
}

fn normalize(value: [f64; 3]) -> [f64; 3] {
    let length = value
        .iter()
        .map(|component| component * component)
        .sum::<f64>()
        .sqrt();
    assert!(
        length > 1.0e-9,
        "validated fitted segment is non-degenerate"
    );
    value.map(|component| component / length)
}

fn add(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] + right[0], left[1] + right[1], left[2] + right[2]]
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn scale(value: [f64; 3], factor: f64) -> [f64; 3] {
    [value[0] * factor, value[1] * factor, value[2] * factor]
}
