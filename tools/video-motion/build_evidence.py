"""Encode the panel video and extract strict MediaPipe landmark evidence."""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path

import cv2
import mediapipe as mp
from mediapipe.tasks import python
from mediapipe.tasks.python import vision
from PIL import Image

FPS = 24
FRAME_COUNT = 16
VIEW_SIZE = 256
PANEL_SIZE = 512
MODEL_SHA256 = "5134a3aad27a58b93da0088d431f366da362b44e3ccfbe3462b3827a839011b1"
MODEL_URL = (
    "https://storage.googleapis.com/mediapipe-models/pose_landmarker/"
    "pose_landmarker_full/float16/1/pose_landmarker_full.task"
)
VIEWS = [
    {
        "id": "front",
        "panel": [0, 0, VIEW_SIZE, VIEW_SIZE],
        "position": [0.0, -7.0, 1.8],
        "right": [1.0, 0.0, 0.0],
        "up": [0.0, 0.0, 1.0],
    },
    {
        "id": "side",
        "panel": [VIEW_SIZE, 0, VIEW_SIZE, VIEW_SIZE],
        "position": [7.0, 0.0, 1.8],
        "right": [0.0, 1.0, 0.0],
        "up": [0.0, 0.0, 1.0],
    },
    {
        "id": "back",
        "panel": [0, VIEW_SIZE, VIEW_SIZE, VIEW_SIZE],
        "position": [0.0, 7.0, 1.8],
        "right": [-1.0, 0.0, 0.0],
        "up": [0.0, 0.0, 1.0],
    },
    {
        "id": "three_quarter",
        "panel": [VIEW_SIZE, VIEW_SIZE, VIEW_SIZE, VIEW_SIZE],
        "position": [5.0, -5.0, 1.8],
        "right": [2**-0.5, 2**-0.5, 0.0],
        "up": [0.0, 0.0, 1.0],
    },
]


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--rendered", type=Path, required=True)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--video", type=Path, required=True)
    parser.add_argument("--video-label", required=True)
    parser.add_argument("--landmarks", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def compose_panels(rendered: Path, output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    for frame_index in range(FRAME_COUNT):
        panel = Image.new("RGB", (PANEL_SIZE, PANEL_SIZE))
        for view in VIEWS:
            source = rendered / view["id"] / f"frame-{frame_index:03}.png"
            with Image.open(source) as image:
                panel.paste(image.convert("RGB"), (view["panel"][0], view["panel"][1]))
        panel.save(output / f"panel-{frame_index:03}.png", compress_level=9)


def encode_video(panels: Path, output: Path) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-fflags",
            "+bitexact",
            "-framerate",
            str(FPS),
            "-i",
            str(panels / "panel-%03d.png"),
            "-map_metadata",
            "-1",
            "-c:v",
            "ffv1",
            "-level",
            "3",
            "-g",
            "1",
            "-slicecrc",
            "1",
            "-pix_fmt",
            "bgr0",
            "-flags",
            "+bitexact",
            str(output),
        ],
        check=True,
    )


def detector(model: Path) -> vision.PoseLandmarker:
    return vision.PoseLandmarker.create_from_options(
        vision.PoseLandmarkerOptions(
            base_options=python.BaseOptions(model_asset_path=str(model)),
            running_mode=vision.RunningMode.IMAGE,
            num_poses=1,
            min_pose_detection_confidence=0.2,
            min_pose_presence_confidence=0.2,
            min_tracking_confidence=0.2,
        )
    )


def inferred_weapon_endpoints(landmarks: list[dict]) -> dict:
    elbow = landmarks[14]
    wrist = landmarks[16]
    dx = wrist["x"] - elbow["x"]
    dy = wrist["y"] - elbow["y"]
    length = (dx * dx + dy * dy) ** 0.5
    if length < 1.0e-6:
        raise RuntimeError("right hand axis is degenerate")
    return {
        "grip": {"x": wrist["x"], "y": wrist["y"]},
        "muzzle": {
            "x": wrist["x"] + dx / length * 0.18,
            "y": wrist["y"] + dy / length * 0.18,
        },
    }


def extract_landmarks(video: Path, model: Path) -> list[dict]:
    capture = cv2.VideoCapture(str(video))
    if not capture.isOpened():
        raise RuntimeError(f"could not open {video}")
    detectors = {view["id"]: detector(model) for view in VIEWS}
    frames = []
    try:
        frame_index = 0
        while True:
            available, frame = capture.read()
            if not available:
                break
            if frame.shape[:2] != (PANEL_SIZE, PANEL_SIZE):
                raise RuntimeError(f"unexpected panel dimensions {frame.shape[:2]}")
            observations = []
            for view in VIEWS:
                x, y, width, height = view["panel"]
                crop = cv2.cvtColor(
                    frame[y : y + height, x : x + width], cv2.COLOR_BGR2RGB
                )
                result = detectors[view["id"]].detect(
                    mp.Image(image_format=mp.ImageFormat.SRGB, data=crop)
                )
                landmarks = None
                if len(result.pose_landmarks) == 1:
                    landmarks = [
                        {
                            "x": point.x,
                            "y": point.y,
                            "visibility": point.visibility,
                            "presence": point.presence,
                        }
                        for point in result.pose_landmarks[0]
                    ]
                    if len(landmarks) != 33:
                        raise RuntimeError("Pose Landmarker did not return 33 landmarks")
                elif result.pose_landmarks:
                    raise RuntimeError(
                        f"{view['id']} frame {frame_index} produced "
                        f"{len(result.pose_landmarks)} poses"
                    )
                observations.append(
                    {
                        "viewId": view["id"],
                        "observationKind": "detected" if landmarks else "missing",
                        "landmarks": landmarks,
                        "weaponEndpointKind": "inferredFromRightHandAxis",
                        "weaponEndpoints": (
                            inferred_weapon_endpoints(landmarks) if landmarks else None
                        ),
                    }
                )
            frames.append(
                {
                    "frameIndex": frame_index,
                    "timestampMicroseconds": round(frame_index * 1_000_000 / FPS),
                    "views": observations,
                }
            )
            frame_index += 1
    finally:
        capture.release()
        for item in detectors.values():
            item.close()
    if len(frames) != FRAME_COUNT:
        raise RuntimeError(f"decoded {len(frames)} frames; expected {FRAME_COUNT}")
    interpolate_isolated_detection_gaps(frames)
    return frames


def interpolate_isolated_detection_gaps(frames: list[dict]) -> None:
    for view_index, view in enumerate(VIEWS):
        missing = [
            frame_index
            for frame_index, frame in enumerate(frames)
            if frame["views"][view_index]["landmarks"] is None
        ]
        if len(missing) > 1:
            raise RuntimeError(f"{view['id']} has more than one missing detection: {missing}")
        if not missing:
            continue
        frame_index = missing[0]
        if frame_index == 0 or frame_index + 1 == len(frames):
            raise RuntimeError(f"{view['id']} has an endpoint detection gap")
        previous = frames[frame_index - 1]["views"][view_index]["landmarks"]
        following = frames[frame_index + 1]["views"][view_index]["landmarks"]
        if previous is None or following is None:
            raise RuntimeError(f"{view['id']} has adjacent detection gaps")
        frames[frame_index]["views"][view_index] = {
            "viewId": view["id"],
            "observationKind": "interpolatedDetectionGap",
            "landmarks": [
                {key: (left[key] + right[key]) / 2.0 for key in left}
                for left, right in zip(previous, following, strict=True)
            ],
            "weaponEndpoints": frames[frame_index]["views"][view_index][
                "weaponEndpoints"
            ]
            or {
                endpoint: {
                    axis: (
                        frames[frame_index - 1]["views"][view_index][
                            "weaponEndpoints"
                        ][endpoint][axis]
                        + frames[frame_index + 1]["views"][view_index][
                            "weaponEndpoints"
                        ][endpoint][axis]
                    )
                    / 2.0
                    for axis in ["x", "y"]
                }
                for endpoint in ["grip", "muzzle"]
            },
            "weaponEndpointKind": "inferredFromRightHandAxis",
        }


def main() -> None:
    arguments = parse_arguments()
    if sha256(arguments.model) != MODEL_SHA256:
        raise RuntimeError("Pose Landmarker model hash does not match the pinned model")
    panels = arguments.rendered / "panels"
    compose_panels(arguments.rendered, panels)
    encode_video(panels, arguments.video)
    frames = extract_landmarks(arguments.video, arguments.model)
    document = {
        "schemaVersion": 1,
        "source": {
            "path": str(arguments.source.relative_to(arguments.root)),
            "sha256": sha256(arguments.source),
            "derivedVideoPath": arguments.video_label,
            "derivedVideoSha256": sha256(arguments.video),
            "clip": "run",
            "framesPerSecond": FPS,
        },
        "estimator": {
            "package": "mediapipe",
            "packageVersion": "0.10.35",
            "modelUrl": MODEL_URL,
            "modelSha256": MODEL_SHA256,
            "modelVariant": "pose_landmarker_full.float16.1",
        },
        "coordinateSystem": {
            "kind": "orthographicBlenderWorld",
            "target": [0.0, 0.0, 1.8],
            "orthoScale": 4.8,
            "viewPixels": [VIEW_SIZE, VIEW_SIZE],
        },
        "cameras": VIEWS,
        "frames": frames,
    }
    arguments.landmarks.parent.mkdir(parents=True, exist_ok=True)
    arguments.landmarks.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


main()
