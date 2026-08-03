#!/usr/bin/env python3
"""Pack a .gltf + .bin pair into a geometry-only .glb for voxel conversion.

The engine voxel converter maps material *slots* to a flat palette and never
decodes images, so images/textures/samplers and material texture references
are stripped to keep the packed artifact lean. Buffer 0 is embedded as the
GLB BIN chunk. Prints world-space bounds (node transforms applied) so grid
planning can match the real model size.

`--exclude-nodes REGEX` drops every node whose name matches (with its whole
subtree), which removes Sketchfab `pasted__*` duplicate geometry.

`--multiply-positions SCALAR` scales all POSITION accessor values in place.
Use an exact power of two so f32 coordinates stay precise. This works around
the Engine importer's absolute degenerate-triangle threshold
(`area_squared <= f64::EPSILON` on squared area), which rejects small but
valid triangles when a model's world units are tiny (rusty-engine task
pending). Prefer a power of two such as 128.
"""

import argparse
import json
import re
import struct


def load_gltf(gltf_path):
    with open(gltf_path) as f:
        return json.load(f)


def strip_textures(doc):
    for key in ("images", "textures", "samplers"):
        doc.pop(key, None)
    texture_keys = (
        "baseColorTexture",
        "metallicRoughnessTexture",
        "normalTexture",
        "occlusionTexture",
        "emissiveTexture",
    )
    for material in doc.get("materials", []):
        pbr = material.get("pbrMetallicRoughness", {})
        for key in texture_keys:
            pbr.pop(key, None)
            material.pop(key, None)
        for ext in material.get("extensions", {}).values():
            if isinstance(ext, dict):
                for key in list(ext):
                    if key.endswith("Texture"):
                        ext.pop(key, None)


def exclude_nodes(doc, pattern):
    """Remove nodes (and their subtrees) whose name matches `pattern`."""
    nodes = doc.get("nodes", [])
    drop = {
        index
        for index, node in enumerate(nodes)
        if pattern.search(node.get("name") or "")
    }
    if not drop:
        return
    # Expand the drop set over descendants.
    changed = True
    while changed:
        changed = False
        for index, node in enumerate(nodes):
            if index in drop:
                continue
            if any(child in drop for child in node.get("children", [])):
                pass
        for index in list(drop):
            for child in nodes[index].get("children", []):
                if child not in drop:
                    drop.add(child)
                    changed = True
    # Reindex: keep nodes not dropped.
    keep = [index for index in range(len(nodes)) if index not in drop]
    remap = {old: new for new, old in enumerate(keep)}
    doc["nodes"] = [nodes[index] for index in keep]
    for node in doc["nodes"]:
        if "children" in node:
            node["children"] = [remap[c] for c in node["children"] if c in remap]
            if not node["children"]:
                del node["children"]
    for scene in doc.get("scenes", []):
        scene["nodes"] = [remap[n] for n in scene.get("nodes", []) if n in remap]
    # Drop skins/animations referencing removed nodes (not expected here).
    doc.pop("skins", None)
    doc.pop("animations", None)


def mat4_identity():
    return [
        1.0, 0.0, 0.0, 0.0,
        0.0, 1.0, 0.0, 0.0,
        0.0, 0.0, 1.0, 0.0,
        0.0, 0.0, 0.0, 1.0,
    ]


def mat4_mul(a, b):
    out = [0.0] * 16
    for row in range(4):
        for col in range(4):
            out[col * 4 + row] = sum(
                a[k * 4 + row] * b[col * 4 + k] for k in range(4)
            )
    return out


def mat4_from_trs(translation, rotation, scale):
    x, y, z, w = rotation
    sx, sy, sz = scale
    xx, yy, zz = x * x, y * y, z * z
    xy, xz, yz = x * y, x * z, y * z
    wx, wy, wz = w * x, w * y, w * z
    return [
        (1.0 - 2.0 * (yy + zz)) * sx, (2.0 * (xy + wz)) * sx, (2.0 * (xz - wy)) * sx, 0.0,
        (2.0 * (xy - wz)) * sy, (1.0 - 2.0 * (xx + zz)) * sy, (2.0 * (yz + wx)) * sy, 0.0,
        (2.0 * (xz + wy)) * sz, (2.0 * (yz - wx)) * sz, (1.0 - 2.0 * (xx + yy)) * sz, 0.0,
        translation[0], translation[1], translation[2], 1.0,
    ]


def node_local_matrix(node):
    if "matrix" in node:
        return node["matrix"]
    return mat4_from_trs(
        node.get("translation", [0.0, 0.0, 0.0]),
        node.get("rotation", [0.0, 0.0, 0.0, 1.0]),
        node.get("scale", [1.0, 1.0, 1.0]),
    )


def mat4_transform_point(m, p):
    return [
        m[0] * p[0] + m[4] * p[1] + m[8] * p[2] + m[12],
        m[1] * p[0] + m[5] * p[1] + m[9] * p[2] + m[13],
        m[2] * p[0] + m[6] * p[1] + m[10] * p[2] + m[14],
    ]


def world_bounds(doc):
    nodes = doc.get("nodes", [])
    meshes = doc.get("meshes", [])
    accessors = doc.get("accessors", [])
    mins = [float("inf")] * 3
    maxs = [float("-inf")] * 3

    def walk(index, parent):
        node = nodes[index]
        world = mat4_mul(parent, node_local_matrix(node))
        if "mesh" in node:
            for primitive in meshes[node["mesh"]].get("primitives", []):
                pos = primitive.get("attributes", {}).get("POSITION")
                if pos is None:
                    continue
                acc = accessors[pos]
                lo, hi = acc.get("min"), acc.get("max")
                if not lo or not hi:
                    continue
                for i in range(8):
                    corner = [
                        lo[0] if i & 1 else hi[0],
                        lo[1] if i & 2 else hi[1],
                        lo[2] if i & 4 else hi[2],
                    ]
                    p = mat4_transform_point(world, corner)
                    for axis in range(3):
                        mins[axis] = min(mins[axis], p[axis])
                        maxs[axis] = max(maxs[axis], p[axis])
        for child in node.get("children", []):
            walk(child, world)

    scene = doc.get("scenes", [])[doc.get("scene", 0)]
    for root in scene.get("nodes", []):
        walk(root, mat4_identity())
    return mins, maxs


def multiply_positions(doc, blob, scalar):
    """Scale every float32 POSITION accessor value in place (power-of-two
    scalars keep f32 coordinates exact). Returns the mutated blob."""
    data = bytearray(blob)
    for mesh in doc.get("meshes", []):
        for primitive in mesh.get("primitives", []):
            pos = primitive.get("attributes", {}).get("POSITION")
            if pos is None:
                continue
            accessor = doc["accessors"][pos]
            if accessor.get("componentType") != 5126 or accessor.get("type") != "VEC3":
                raise SystemExit("POSITION accessors must be float32 VEC3")
            view = doc["bufferViews"][accessor["bufferView"]]
            stride = view.get("byteStride") or 12
            offset = view.get("byteOffset", 0) + accessor.get("byteOffset", 0)
            for i in range(accessor["count"]):
                base = offset + i * stride
                for k in range(3):
                    (value,) = struct.unpack_from("<f", data, base + k * 4)
                    struct.pack_into("<f", data, base + k * 4, value * scalar)
            if accessor.get("min"):
                accessor["min"] = [v * scalar for v in accessor["min"]]
            if accessor.get("max"):
                accessor["max"] = [v * scalar for v in accessor["max"]]
    return bytes(data)


def pack(gltf_path, bin_path, glb_path, exclude_pattern, multiply):
    doc = load_gltf(gltf_path)
    strip_textures(doc)
    if exclude_pattern is not None:
        exclude_nodes(doc, exclude_pattern)
    with open(bin_path, "rb") as f:
        blob = f.read()
    buffers = doc.get("buffers", [])
    if len(buffers) != 1:
        raise SystemExit(f"expected exactly one buffer, found {len(buffers)}")
    buffers[0] = {"byteLength": len(blob)}
    if multiply is not None:
        blob = multiply_positions(doc, blob, multiply)
    mins, maxs = world_bounds(doc)
    json_chunk = json.dumps(doc, separators=(",", ":")).encode()
    json_chunk += b" " * ((4 - len(json_chunk) % 4) % 4)
    bin_chunk = blob + b"\x00" * ((4 - len(blob) % 4) % 4)
    total = 12 + 8 + len(json_chunk) + 8 + len(bin_chunk)
    with open(glb_path, "wb") as f:
        f.write(struct.pack("<III", 0x46546C67, 2, total))
        f.write(struct.pack("<II", len(json_chunk), 0x4E4F534A))
        f.write(json_chunk)
        f.write(struct.pack("<II", len(bin_chunk), 0x004E4942))
        f.write(bin_chunk)
    print(f"wrote {glb_path} ({total} bytes)")
    print(f"nodes kept: {len(doc.get('nodes', []))}")
    print(f"world bounds min: {[round(v, 4) for v in mins]}")
    print(f"world bounds max: {[round(v, 4) for v in maxs]}")
    span = [maxs[i] - mins[i] for i in range(3)]
    print(f"world span: {[round(v, 4) for v in span]}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gltf")
    parser.add_argument("bin")
    parser.add_argument("glb")
    parser.add_argument("--exclude-nodes", default=None, help="regex of node names to drop")
    parser.add_argument("--multiply-positions", type=float, default=None,
                        help="uniform scalar applied to POSITION data (prefer powers of two)")
    args = parser.parse_args()
    pattern = re.compile(args.exclude_nodes) if args.exclude_nodes else None
    pack(args.gltf, args.bin, args.glb, pattern, args.multiply_positions)


if __name__ == "__main__":
    main()
