//! Bounded multiview landmark fitting for the exploded-kit proxy rig.
//!
//! Video and estimator output are evidence. This module owns the admitted
//! cross-view fit: observations are triangulated against explicit cameras,
//! smoothed, projected onto one fixed-length skeleton, and root-corrected
//! during detected foot contacts. The fitted result is an ordinary animated
//! proxy source for M2; no video-derived geometry enters the voxel kit.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use voxel_convert::ImportedAnimatedModel;

use crate::kit::{neutral_part_transforms, VoxelKit};
use crate::pose::{
    derive_bind_transform, evaluate_node_poses, PartBinding, RigMap, RigidTransform,
    RIG_MAP_SCHEMA_VERSION,
};

pub const VIDEO_LANDMARK_SCHEMA_VERSION: u32 = 1;
pub const FITTED_MOTION_SCHEMA_VERSION: u32 = 1;
const EXPECTED_LANDMARKS: usize = 33;
const MIN_VIEWS: usize = 3;
const MAX_VIEWS: usize = 8;
const MAX_FRAMES: usize = 512;
const MIN_WEIGHT: f64 = 0.02;

const JOINTS: [JointDefinition; 15] = [
    JointDefinition::single("head", 0),
    JointDefinition::pair("neck", 11, 12),
    JointDefinition::single("leftShoulder", 11),
    JointDefinition::single("leftElbow", 13),
    JointDefinition::single("leftWrist", 15),
    JointDefinition::single("rightShoulder", 12),
    JointDefinition::single("rightElbow", 14),
    JointDefinition::single("rightWrist", 16),
    JointDefinition::pair("pelvis", 23, 24),
    JointDefinition::single("leftHip", 23),
    JointDefinition::single("leftKnee", 25),
    JointDefinition::single("leftAnkle", 27),
    JointDefinition::pair("leftFoot", 29, 31),
    JointDefinition::single("rightHip", 24),
    JointDefinition::single("rightKnee", 26),
];

const EXTRA_JOINTS: [JointDefinition; 2] = [
    JointDefinition::single("rightAnkle", 28),
    JointDefinition::pair("rightFoot", 30, 32),
];

const BONES: [BoneDefinition; 14] = [
    BoneDefinition::new("pelvis", "leftHip"),
    BoneDefinition::new("leftHip", "leftKnee"),
    BoneDefinition::new("leftKnee", "leftAnkle"),
    BoneDefinition::new("leftAnkle", "leftFoot"),
    BoneDefinition::new("pelvis", "rightHip"),
    BoneDefinition::new("rightHip", "rightKnee"),
    BoneDefinition::new("rightKnee", "rightAnkle"),
    BoneDefinition::new("rightAnkle", "rightFoot"),
    BoneDefinition::new("pelvis", "neck"),
    BoneDefinition::new("neck", "head"),
    BoneDefinition::new("neck", "leftShoulder"),
    BoneDefinition::new("leftShoulder", "leftElbow"),
    BoneDefinition::new("leftElbow", "leftWrist"),
    BoneDefinition::new("neck", "rightShoulder"),
];

const EXTRA_BONES: [BoneDefinition; 2] = [
    BoneDefinition::new("rightShoulder", "rightElbow"),
    BoneDefinition::new("rightElbow", "rightWrist"),
];
const WEAPON_BONE: BoneDefinition = BoneDefinition::new("weaponGrip", "weaponMuzzle");

const PART_IDS: [&str; 13] = [
    "head",
    "torso",
    "pelvis",
    "left_upper_arm",
    "left_lower_arm",
    "right_upper_arm",
    "right_lower_arm",
    "left_upper_leg",
    "left_lower_leg",
    "right_upper_leg",
    "right_lower_leg",
    "rifle",
    "backpack",
];

#[derive(Debug, Clone, PartialEq)]
pub struct VideoMotionError {
    pub code: &'static str,
    pub path: String,
    pub message: String,
}

impl VideoMotionError {
    fn new(code: &'static str, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for VideoMotionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.code, self.path, self.message
        )
    }
}

impl std::error::Error for VideoMotionError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LandmarkDocument {
    schema_version: u32,
    source: SourceEvidence,
    estimator: EstimatorEvidence,
    coordinate_system: CoordinateSystem,
    cameras: Vec<Camera>,
    frames: Vec<LandmarkFrame>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SourceEvidence {
    path: String,
    sha256: String,
    derived_video_path: String,
    derived_video_sha256: String,
    clip: String,
    frames_per_second: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EstimatorEvidence {
    package: String,
    package_version: String,
    model_url: String,
    model_sha256: String,
    model_variant: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CoordinateSystem {
    kind: String,
    target: [f64; 3],
    ortho_scale: f64,
    view_pixels: [u32; 2],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Camera {
    id: String,
    panel: [u32; 4],
    position: [f64; 3],
    right: [f64; 3],
    up: [f64; 3],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LandmarkFrame {
    frame_index: u32,
    timestamp_microseconds: u64,
    views: Vec<ViewObservation>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ViewObservation {
    view_id: String,
    observation_kind: ObservationKind,
    landmarks: Vec<Landmark>,
    weapon_endpoint_kind: WeaponEndpointKind,
    weapon_endpoints: WeaponEndpoints,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ObservationKind {
    Detected,
    InterpolatedDetectionGap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
enum WeaponEndpointKind {
    InferredFromRightHandAxis,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Landmark {
    x: f64,
    y: f64,
    visibility: f64,
    presence: f64,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WeaponEndpoints {
    grip: ImagePoint,
    muzzle: ImagePoint,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImagePoint {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FittedMotion {
    pub schema_version: u32,
    pub source_video_sha256: String,
    pub landmark_model_sha256: String,
    pub clip_id: String,
    pub frames_per_second: u32,
    pub bone_lengths: BTreeMap<String, f64>,
    pub contact_corrections: Vec<ContactCorrection>,
    pub frames: Vec<FittedFrame>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContactCorrection {
    pub frame_index: u32,
    pub feet: Vec<String>,
    pub root_translation: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FittedFrame {
    pub frame_index: u32,
    pub timestamp_microseconds: u64,
    pub joints: BTreeMap<String, [f64; 3]>,
}

#[derive(Debug, Clone, Copy)]
struct JointDefinition {
    id: &'static str,
    indices: [usize; 2],
    count: usize,
}

impl JointDefinition {
    const fn single(id: &'static str, index: usize) -> Self {
        Self {
            id,
            indices: [index, index],
            count: 1,
        }
    }

    const fn pair(id: &'static str, left: usize, right: usize) -> Self {
        Self {
            id,
            indices: [left, right],
            count: 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BoneDefinition {
    parent: &'static str,
    child: &'static str,
}

impl BoneDefinition {
    const fn new(parent: &'static str, child: &'static str) -> Self {
        Self { parent, child }
    }

    fn id(self) -> String {
        format!("{}>{}", self.parent, self.child)
    }
}

pub fn fit_multiview_landmarks_json(json: &str) -> Result<FittedMotion, VideoMotionError> {
    let document: LandmarkDocument = serde_json::from_str(json).map_err(|error| {
        VideoMotionError::new("videoMotion.invalidJson", "$", error.to_string())
    })?;
    validate_document(&document)?;
    let cameras = document
        .cameras
        .iter()
        .map(|camera| (camera.id.as_str(), camera))
        .collect::<BTreeMap<_, _>>();
    let mut raw_frames = Vec::with_capacity(document.frames.len());
    for frame in &document.frames {
        let mut joints = BTreeMap::new();
        for definition in JOINTS.iter().chain(EXTRA_JOINTS.iter()) {
            let points = definition.indices[..definition.count]
                .iter()
                .map(|index| triangulate_landmark(&document, frame, &cameras, *index))
                .collect::<Result<Vec<_>, _>>()?;
            joints.insert(definition.id.to_owned(), average(&points));
        }
        joints.insert(
            "weaponGrip".to_owned(),
            triangulate_weapon_endpoint(&document, frame, &cameras, |value| value.grip)?,
        );
        joints.insert(
            "weaponMuzzle".to_owned(),
            triangulate_weapon_endpoint(&document, frame, &cameras, |value| value.muzzle)?,
        );
        raw_frames.push(joints);
    }
    let smoothed = smooth_frames(&raw_frames);
    let bone_lengths = fixed_lengths(&smoothed)?;
    let projected = smoothed
        .iter()
        .map(|frame| project_fixed_lengths(frame, &bone_lengths))
        .collect::<Result<Vec<_>, _>>()?;
    let (frames, contact_corrections) = correct_contacts(&projected);
    Ok(FittedMotion {
        schema_version: FITTED_MOTION_SCHEMA_VERSION,
        source_video_sha256: document.source.derived_video_sha256,
        landmark_model_sha256: document.estimator.model_sha256,
        clip_id: "fitted_run".to_owned(),
        frames_per_second: document.source.frames_per_second,
        bone_lengths,
        contact_corrections,
        frames: document
            .frames
            .iter()
            .zip(frames)
            .map(|(source, joints)| FittedFrame {
                frame_index: source.frame_index,
                timestamp_microseconds: source.timestamp_microseconds,
                joints,
            })
            .collect(),
    })
}

fn validate_document(document: &LandmarkDocument) -> Result<(), VideoMotionError> {
    if document.schema_version != VIDEO_LANDMARK_SCHEMA_VERSION {
        return Err(VideoMotionError::new(
            "videoMotion.unsupportedSchema",
            "schemaVersion",
            format!("expected {VIDEO_LANDMARK_SCHEMA_VERSION}"),
        ));
    }
    if document.frames.len() < 2 || document.frames.len() > MAX_FRAMES {
        return Err(VideoMotionError::new(
            "videoMotion.frameQuota",
            "frames",
            format!("must contain 2..={MAX_FRAMES} frames"),
        ));
    }
    if document.cameras.len() < MIN_VIEWS || document.cameras.len() > MAX_VIEWS {
        return Err(VideoMotionError::new(
            "videoMotion.viewQuota",
            "cameras",
            format!("must contain {MIN_VIEWS}..={MAX_VIEWS} cameras"),
        ));
    }
    if document.coordinate_system.kind != "orthographicBlenderWorld"
        || !document.coordinate_system.ortho_scale.is_finite()
        || document.coordinate_system.ortho_scale <= 0.0
        || document.coordinate_system.view_pixels.contains(&0)
        || !finite3(document.coordinate_system.target)
    {
        return Err(VideoMotionError::new(
            "videoMotion.invalidCalibration",
            "coordinateSystem",
            "orthographic calibration is invalid",
        ));
    }
    if document.source.frames_per_second == 0 || document.source.frames_per_second > 240 {
        return Err(VideoMotionError::new(
            "videoMotion.invalidTiming",
            "source.framesPerSecond",
            "must be within 1..=240",
        ));
    }
    for (field, value) in [
        ("source.path", document.source.path.as_str()),
        (
            "source.derivedVideoPath",
            document.source.derived_video_path.as_str(),
        ),
        ("source.sha256", document.source.sha256.as_str()),
        (
            "source.derivedVideoSha256",
            document.source.derived_video_sha256.as_str(),
        ),
        ("source.clip", document.source.clip.as_str()),
        ("estimator.package", document.estimator.package.as_str()),
        (
            "estimator.packageVersion",
            document.estimator.package_version.as_str(),
        ),
        ("estimator.modelUrl", document.estimator.model_url.as_str()),
        (
            "estimator.modelSha256",
            document.estimator.model_sha256.as_str(),
        ),
        (
            "estimator.modelVariant",
            document.estimator.model_variant.as_str(),
        ),
    ] {
        if value.is_empty() {
            return Err(VideoMotionError::new(
                "videoMotion.missingProvenance",
                field,
                "must not be empty",
            ));
        }
    }
    let mut camera_ids = BTreeSet::new();
    for (index, camera) in document.cameras.iter().enumerate() {
        if camera.id.is_empty()
            || !camera_ids.insert(camera.id.as_str())
            || !finite3(camera.position)
            || !finite3(camera.right)
            || !finite3(camera.up)
            || norm(camera.right) < 0.99
            || norm(camera.up) < 0.99
            || dot(camera.right, camera.up).abs() > 1.0e-6
            || camera.panel[2] == 0
            || camera.panel[3] == 0
        {
            return Err(VideoMotionError::new(
                "videoMotion.invalidCamera",
                format!("cameras[{index}]"),
                "camera identity, panel, or basis is invalid",
            ));
        }
    }
    for (frame_index, frame) in document.frames.iter().enumerate() {
        if frame.frame_index as usize != frame_index
            || frame.views.len() != document.cameras.len()
            || (frame_index > 0
                && frame.timestamp_microseconds
                    <= document.frames[frame_index - 1].timestamp_microseconds)
        {
            return Err(VideoMotionError::new(
                "videoMotion.invalidFrame",
                format!("frames[{frame_index}]"),
                "frame indices, timing, and camera cardinality must be exact",
            ));
        }
        let mut view_ids = BTreeSet::new();
        for (view_index, view) in frame.views.iter().enumerate() {
            if !camera_ids.contains(view.view_id.as_str())
                || !view_ids.insert(view.view_id.as_str())
                || view.landmarks.len() != EXPECTED_LANDMARKS
            {
                return Err(VideoMotionError::new(
                    "videoMotion.invalidObservation",
                    format!("frames[{frame_index}].views[{view_index}]"),
                    "view identity and 33-landmark cardinality must be exact",
                ));
            }
            if view.observation_kind == ObservationKind::InterpolatedDetectionGap
                && (frame_index == 0 || frame_index + 1 == document.frames.len())
            {
                return Err(VideoMotionError::new(
                    "videoMotion.invalidInterpolation",
                    format!("frames[{frame_index}].views[{view_index}]"),
                    "interpolated detections cannot occur at clip endpoints",
                ));
            }
            for (landmark_index, landmark) in view.landmarks.iter().enumerate() {
                if ![
                    landmark.x,
                    landmark.y,
                    landmark.visibility,
                    landmark.presence,
                ]
                .iter()
                .all(|value| value.is_finite())
                    || !(0.0..=1.0).contains(&landmark.visibility)
                    || !(0.0..=1.0).contains(&landmark.presence)
                {
                    return Err(VideoMotionError::new(
                        "videoMotion.invalidLandmark",
                        format!(
                            "frames[{frame_index}].views[{view_index}].landmarks[{landmark_index}]"
                        ),
                        "landmark values must be finite and confidence must be within [0, 1]",
                    ));
                }
            }
            for (endpoint, point) in [
                ("grip", view.weapon_endpoints.grip),
                ("muzzle", view.weapon_endpoints.muzzle),
            ] {
                if !point.x.is_finite()
                    || !point.y.is_finite()
                    || !(-0.25..=1.25).contains(&point.x)
                    || !(-0.25..=1.25).contains(&point.y)
                {
                    return Err(VideoMotionError::new(
                        "videoMotion.invalidWeaponEndpoint",
                        format!(
                            "frames[{frame_index}].views[{view_index}].weaponEndpoints.{endpoint}"
                        ),
                        "weapon endpoint must be finite and near the calibrated view",
                    ));
                }
            }
            let WeaponEndpointKind::InferredFromRightHandAxis = view.weapon_endpoint_kind;
        }
    }
    Ok(())
}

fn triangulate_landmark(
    document: &LandmarkDocument,
    frame: &LandmarkFrame,
    cameras: &BTreeMap<&str, &Camera>,
    landmark_index: usize,
) -> Result<[f64; 3], VideoMotionError> {
    triangulate_observations(
        document,
        frame.views.iter().map(|view| {
            let landmark = view.landmarks[landmark_index];
            let confidence = (landmark.visibility * landmark.presence).clamp(MIN_WEIGHT, 1.0);
            let interpolation_weight = if view.observation_kind == ObservationKind::Detected {
                1.0
            } else {
                0.5
            };
            (
                cameras[view.view_id.as_str()],
                landmark.x,
                landmark.y,
                confidence * interpolation_weight,
            )
        }),
        format!("frames[{}].landmark[{landmark_index}]", frame.frame_index),
    )
}

fn triangulate_weapon_endpoint(
    document: &LandmarkDocument,
    frame: &LandmarkFrame,
    cameras: &BTreeMap<&str, &Camera>,
    endpoint: impl Fn(WeaponEndpoints) -> ImagePoint,
) -> Result<[f64; 3], VideoMotionError> {
    triangulate_observations(
        document,
        frame.views.iter().map(|view| {
            let point = endpoint(view.weapon_endpoints);
            (cameras[view.view_id.as_str()], point.x, point.y, 1.0)
        }),
        format!("frames[{}].weaponEndpoint", frame.frame_index),
    )
}

fn triangulate_observations<'a>(
    document: &LandmarkDocument,
    observations: impl Iterator<Item = (&'a Camera, f64, f64, f64)>,
    path: String,
) -> Result<[f64; 3], VideoMotionError> {
    let mut normal = [[0.0; 3]; 3];
    let mut rhs = [0.0; 3];
    for (camera, x, y, weight) in observations {
        let horizontal = (x - 0.5) * document.coordinate_system.ortho_scale
            + dot(document.coordinate_system.target, camera.right);
        let vertical = (0.5 - y) * document.coordinate_system.ortho_scale
            + dot(document.coordinate_system.target, camera.up);
        accumulate_equation(&mut normal, &mut rhs, camera.right, horizontal, weight);
        accumulate_equation(&mut normal, &mut rhs, camera.up, vertical, weight);
    }
    solve_3x3(normal, rhs).ok_or_else(|| {
        VideoMotionError::new(
            "videoMotion.degenerateFit",
            path,
            "camera equations are singular",
        )
    })
}

fn accumulate_equation(
    normal: &mut [[f64; 3]; 3],
    rhs: &mut [f64; 3],
    axis: [f64; 3],
    value: f64,
    weight: f64,
) {
    for row in 0..3 {
        rhs[row] += weight * axis[row] * value;
        for column in 0..3 {
            normal[row][column] += weight * axis[row] * axis[column];
        }
    }
}

fn solve_3x3(mut matrix: [[f64; 3]; 3], mut rhs: [f64; 3]) -> Option<[f64; 3]> {
    for pivot in 0..3 {
        let best = (pivot..3).max_by(|left, right| {
            matrix[*left][pivot]
                .abs()
                .total_cmp(&matrix[*right][pivot].abs())
        })?;
        if matrix[best][pivot].abs() < 1.0e-9 {
            return None;
        }
        matrix.swap(pivot, best);
        rhs.swap(pivot, best);
        let divisor = matrix[pivot][pivot];
        for value in &mut matrix[pivot][pivot..] {
            *value /= divisor;
        }
        rhs[pivot] /= divisor;
        let pivot_row = matrix[pivot];
        for row in 0..3 {
            if row == pivot {
                continue;
            }
            let factor = matrix[row][pivot];
            for (value, pivot_value) in matrix[row][pivot..].iter_mut().zip(&pivot_row[pivot..]) {
                *value -= factor * pivot_value;
            }
            rhs[row] -= factor * rhs[pivot];
        }
    }
    Some(rhs)
}

fn smooth_frames(frames: &[BTreeMap<String, [f64; 3]>]) -> Vec<BTreeMap<String, [f64; 3]>> {
    frames
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            frame
                .keys()
                .map(|id| {
                    let previous = &frames[index.saturating_sub(1)][id];
                    let current = &frame[id];
                    let following = &frames[(index + 1).min(frames.len() - 1)][id];
                    (
                        id.clone(),
                        [
                            (previous[0] + 2.0 * current[0] + following[0]) / 4.0,
                            (previous[1] + 2.0 * current[1] + following[1]) / 4.0,
                            (previous[2] + 2.0 * current[2] + following[2]) / 4.0,
                        ],
                    )
                })
                .collect()
        })
        .collect()
}

fn fixed_lengths(
    frames: &[BTreeMap<String, [f64; 3]>],
) -> Result<BTreeMap<String, f64>, VideoMotionError> {
    all_bones()
        .map(|bone| {
            let mut values = frames
                .iter()
                .map(|frame| distance(frame[bone.parent], frame[bone.child]))
                .collect::<Vec<_>>();
            values.sort_by(f64::total_cmp);
            let length = values[values.len() / 2];
            if !length.is_finite() || length < 1.0e-4 {
                return Err(VideoMotionError::new(
                    "videoMotion.invalidBoneLength",
                    bone.id(),
                    "median fitted bone length is degenerate",
                ));
            }
            Ok((bone.id(), length))
        })
        .collect()
}

fn project_fixed_lengths(
    raw: &BTreeMap<String, [f64; 3]>,
    lengths: &BTreeMap<String, f64>,
) -> Result<BTreeMap<String, [f64; 3]>, VideoMotionError> {
    let mut fitted = BTreeMap::from([
        ("pelvis".to_owned(), raw["pelvis"]),
        ("weaponGrip".to_owned(), raw["weaponGrip"]),
    ]);
    for bone in all_bones() {
        let parent = fitted.get(bone.parent).copied().ok_or_else(|| {
            VideoMotionError::new(
                "videoMotion.invalidSkeleton",
                bone.id(),
                "parent was not projected before child",
            )
        })?;
        let direction = subtract(raw[bone.child], raw[bone.parent]);
        let unit = normalize(direction).ok_or_else(|| {
            VideoMotionError::new(
                "videoMotion.invalidSkeleton",
                bone.id(),
                "observed bone direction is degenerate",
            )
        })?;
        fitted.insert(
            bone.child.to_owned(),
            add(parent, scale(unit, lengths[&bone.id()])),
        );
    }
    Ok(fitted)
}

fn all_bones() -> impl Iterator<Item = BoneDefinition> {
    BONES
        .iter()
        .chain(EXTRA_BONES.iter())
        .copied()
        .chain(std::iter::once(WEAPON_BONE))
}

fn correct_contacts(
    frames: &[BTreeMap<String, [f64; 3]>],
) -> (Vec<BTreeMap<String, [f64; 3]>>, Vec<ContactCorrection>) {
    let ground = frames
        .iter()
        .flat_map(|frame| [frame["leftFoot"][2], frame["rightFoot"][2]])
        .fold(f64::INFINITY, f64::min);
    let height = frames
        .iter()
        .map(|frame| frame["head"][2] - ground)
        .fold(0.0, f64::max)
        .max(1.0e-3);
    let height_threshold = ground + height * 0.08;
    let speed_threshold = height * 0.09;
    let mut anchors: BTreeMap<&str, [f64; 3]> = BTreeMap::new();
    let mut corrected = Vec::with_capacity(frames.len());
    let mut receipts = Vec::with_capacity(frames.len());
    for (index, frame) in frames.iter().enumerate() {
        let contacts = ["leftFoot", "rightFoot"]
            .into_iter()
            .filter(|foot| {
                let point = frame[*foot];
                let speed = if index == 0 {
                    0.0
                } else {
                    let previous = frames[index - 1][*foot];
                    ((point[0] - previous[0]).powi(2) + (point[1] - previous[1]).powi(2)).sqrt()
                };
                point[2] <= height_threshold && speed <= speed_threshold
            })
            .collect::<Vec<_>>();
        anchors.retain(|foot, _| contacts.contains(foot));
        for foot in &contacts {
            anchors.entry(foot).or_insert(frame[*foot]);
        }
        let mut correction = [0.0; 3];
        if !contacts.is_empty() {
            for foot in &contacts {
                let anchor = anchors[foot];
                let point = frame[*foot];
                correction[0] += anchor[0] - point[0];
                correction[1] += anchor[1] - point[1];
                correction[2] += ground - point[2];
            }
            for value in &mut correction {
                *value /= contacts.len() as f64;
            }
        }
        corrected.push(
            frame
                .iter()
                .map(|(id, point)| (id.clone(), add(*point, correction)))
                .collect(),
        );
        receipts.push(ContactCorrection {
            frame_index: index as u32,
            feet: contacts.into_iter().map(str::to_owned).collect(),
            root_translation: correction,
        });
    }
    (corrected, receipts)
}

/// Build the M2 rig map for a fitted GLB whose node names are
/// `proxy.<canonical-part-id>`.
pub fn fitted_motion_rig_map(
    kit: &VoxelKit,
    model: &ImportedAnimatedModel,
) -> Result<RigMap, VideoMotionError> {
    model
        .clips
        .iter()
        .position(|clip| clip.name == "fitted_run")
        .ok_or_else(|| {
            VideoMotionError::new(
                "videoMotion.missingClip",
                "model.clips",
                "fitted_run clip is missing",
            )
        })?;
    let bind_clip_index = model
        .clips
        .iter()
        .position(|clip| clip.name == "proxy_bind")
        .ok_or_else(|| {
            VideoMotionError::new(
                "videoMotion.missingClip",
                "model.clips",
                "proxy_bind clip is missing",
            )
        })?;
    let bind_poses = evaluate_node_poses(model, bind_clip_index, 0).map_err(|error| {
        VideoMotionError::new("videoMotion.invalidGlbPose", "model", error.to_string())
    })?;
    let neutral = neutral_part_transforms(kit).map_err(|error| {
        VideoMotionError::new("videoMotion.invalidKit", "kit", error.to_string())
    })?;
    let names = model
        .scene
        .nodes
        .iter()
        .filter_map(|node| {
            node.source_node_name
                .as_deref()
                .map(|name| (name, node.source_node_index))
        })
        .collect::<BTreeMap<_, _>>();
    let mut bindings = Vec::with_capacity(PART_IDS.len());
    for part_id in PART_IDS {
        let node_index = names
            .get(format!("proxy.{part_id}").as_str())
            .copied()
            .ok_or_else(|| {
                VideoMotionError::new(
                    "videoMotion.missingProxyNode",
                    format!("model.nodes.proxy.{part_id}"),
                    "fitted proxy node is missing",
                )
            })?;
        let bind_pose = bind_poses.get(&node_index).copied().ok_or_else(|| {
            VideoMotionError::new(
                "videoMotion.missingProxyPose",
                format!("model.nodes[{node_index}]"),
                "fitted proxy pose is missing",
            )
        })?;
        let (rotation, translation) = neutral.get(part_id).copied().ok_or_else(|| {
            VideoMotionError::new(
                "videoMotion.missingKitPart",
                format!("kit.parts.{part_id}"),
                "neutral transform is missing",
            )
        })?;
        let neutral_transform = RigidTransform {
            rotation,
            translation: translation.map(|value| value as f64),
        };
        bindings.push(PartBinding {
            part_id: part_id.to_owned(),
            bone_node_index: node_index,
            bind_transform: derive_bind_transform(bind_pose, neutral_transform),
        });
    }
    let rig_map = RigMap {
        schema_version: RIG_MAP_SCHEMA_VERSION,
        bindings,
    };
    rig_map.validate(kit, model).map_err(|error| {
        VideoMotionError::new("videoMotion.invalidRigMap", "rigMap", error.to_string())
    })?;
    Ok(rig_map)
}

fn average(points: &[[f64; 3]]) -> [f64; 3] {
    let mut sum = [0.0; 3];
    for point in points {
        sum = add(sum, *point);
    }
    scale(sum, 1.0 / points.len() as f64)
}

fn finite3(value: [f64; 3]) -> bool {
    value.iter().all(|component| component.is_finite())
}

fn dot(left: [f64; 3], right: [f64; 3]) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn norm(value: [f64; 3]) -> f64 {
    dot(value, value).sqrt()
}

fn distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    norm(subtract(left, right))
}

fn normalize(value: [f64; 3]) -> Option<[f64; 3]> {
    let length = norm(value);
    (length > 1.0e-9).then(|| scale(value, 1.0 / length))
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

#[cfg(test)]
mod tests {
    use super::{solve_3x3, BoneDefinition};

    #[test]
    fn solves_independent_camera_equations() {
        let result = solve_3x3(
            [[2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 4.0]],
            [4.0, 9.0, 16.0],
        )
        .expect("solution");
        assert_eq!(result, [2.0, 3.0, 4.0]);
    }

    #[test]
    fn bone_identity_is_stable() {
        assert_eq!(BoneDefinition::new("pelvis", "neck").id(), "pelvis>neck");
    }
}
