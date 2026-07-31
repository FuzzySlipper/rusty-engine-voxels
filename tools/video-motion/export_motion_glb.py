"""Serialize the Rust-owned fitted proxy transforms as a deterministic GLB."""

import argparse
import json
import struct
from pathlib import Path

PARTS = [
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
]


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--motion", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    return parser.parse_args()


class GlbBuilder:
    def __init__(self) -> None:
        self.buffer = bytearray()
        self.views: list[dict] = []
        self.accessors: list[dict] = []

    def accessor(
        self,
        values: list[list[float]] | list[float] | list[int],
        component_type: int,
        value_type: str,
        target: int | None = None,
        include_bounds: bool = False,
    ) -> int:
        self.pad()
        offset = len(self.buffer)
        flat = [
            component
            for value in values
            for component in (value if isinstance(value, list) else [value])
        ]
        if component_type == 5126:
            self.buffer.extend(struct.pack(f"<{len(flat)}f", *flat))
            byte_size = 4
        elif component_type == 5123:
            self.buffer.extend(struct.pack(f"<{len(flat)}H", *flat))
            byte_size = 2
        else:
            raise ValueError(f"unsupported component type {component_type}")
        view = {
            "buffer": 0,
            "byteOffset": offset,
            "byteLength": len(flat) * byte_size,
        }
        if target is not None:
            view["target"] = target
        view_index = len(self.views)
        self.views.append(view)
        accessor = {
            "bufferView": view_index,
            "componentType": component_type,
            "count": len(values),
            "type": value_type,
        }
        if include_bounds:
            width = len(values[0]) if isinstance(values[0], list) else 1
            accessor["min"] = [
                min((value if isinstance(value, list) else [value])[axis] for value in values)
                for axis in range(width)
            ]
            accessor["max"] = [
                max((value if isinstance(value, list) else [value])[axis] for value in values)
                for axis in range(width)
            ]
        accessor_index = len(self.accessors)
        self.accessors.append(accessor)
        return accessor_index

    def pad(self) -> None:
        self.buffer.extend(b"\0" * ((-len(self.buffer)) % 4))


def build_glb(motion: dict) -> bytes:
    if motion["schemaVersion"] != 1 or len(motion["frames"]) < 2:
        raise ValueError("unsupported or empty proxy-motion document")
    builder = GlbBuilder()
    positions = builder.accessor(
        [[0.0, 0.0, 0.0], [0.001, 0.0, 0.0], [0.0, 0.001, 0.0]],
        5126,
        "VEC3",
        34962,
        True,
    )
    indices = builder.accessor([0, 1, 2], 5123, "SCALAR", 34963)
    times = [
        frame["timestampMicroseconds"] / 1_000_000.0 for frame in motion["frames"]
    ]
    time_accessor = builder.accessor(times, 5126, "SCALAR", include_bounds=True)
    nodes = [{"name": f"proxy.{part_id}"} for part_id in PARTS]
    nodes.append({"name": "evidenceCarrier", "mesh": 0})
    samplers = []
    channels = []
    for node_index, part_id in enumerate(PARTS):
        transforms = [
            frame["partTransforms"][part_id] for frame in motion["frames"]
        ]
        translations = builder.accessor(
            [transform["translation"] for transform in transforms], 5126, "VEC3"
        )
        rotations = builder.accessor(
            [transform["rotation"] for transform in transforms], 5126, "VEC4"
        )
        for path, output in [("translation", translations), ("rotation", rotations)]:
            sampler_index = len(samplers)
            samplers.append(
                {"input": time_accessor, "interpolation": "LINEAR", "output": output}
            )
            channels.append(
                {
                    "sampler": sampler_index,
                    "target": {"node": node_index, "path": path},
                }
            )
    builder.pad()
    bind_times = builder.accessor([0.0, 0.000001], 5126, "SCALAR", include_bounds=True)
    bind_translations = builder.accessor(
        [[0.0, 0.0, 0.0], [0.0, 0.0, 0.0]], 5126, "VEC3"
    )
    document = {
        "asset": {"generator": "rusty-engine-voxels video-motion", "version": "2.0"},
        "scene": 0,
        "scenes": [{"name": "fittedMotion", "nodes": list(range(len(nodes)))}],
        "nodes": nodes,
        "meshes": [
            {
                "name": "evidenceCarrier",
                "primitives": [
                    {"attributes": {"POSITION": positions}, "indices": indices, "mode": 4}
                ],
            }
        ],
        "animations": [
            {
                "name": motion["bindClipId"],
                "samplers": [
                    {
                        "input": bind_times,
                        "interpolation": "LINEAR",
                        "output": bind_translations,
                    }
                ],
                "channels": [
                    {"sampler": 0, "target": {"node": 0, "path": "translation"}}
                ],
            },
            {"name": motion["clipId"], "samplers": samplers, "channels": channels},
        ],
        "buffers": [{"byteLength": len(builder.buffer)}],
        "bufferViews": builder.views,
        "accessors": builder.accessors,
    }
    encoded_json = json.dumps(
        document, separators=(",", ":"), sort_keys=True, ensure_ascii=True
    ).encode("utf-8")
    encoded_json += b" " * ((-len(encoded_json)) % 4)
    total_length = 12 + 8 + len(encoded_json) + 8 + len(builder.buffer)
    return b"".join(
        [
            struct.pack("<4sII", b"glTF", 2, total_length),
            struct.pack("<I4s", len(encoded_json), b"JSON"),
            encoded_json,
            struct.pack("<I4s", len(builder.buffer), b"BIN\0"),
            bytes(builder.buffer),
        ]
    )


def main() -> None:
    arguments = parse_arguments()
    motion = json.loads(arguments.motion.read_text(encoding="utf-8"))
    encoded = build_glb(motion)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_bytes(encoded)


main()
