# Rigid-Part Posing and Conservative Rasterization (M2)

M2 turns proxy-rig poses into rough per-frame voxel assemblies by **rigidly transforming the
stable canonical parts** — the step that eliminates the per-frame re-voxelization churn that the
straight mesh→flipbook pipeline produces. This module (`src/pose.rs`) owns the rigid core; see
`baked-voxel-animation-design.md` for the full pipeline.

## The problem this solves

The straight pipeline samples a continuous skinned surface and re-voxelizes it independently every
pose. Two adjacent poses of the *same leg* therefore differ in ~50–70% of occupied cells, not
because the leg moved meaningfully but because a slightly shifted continuous surface aliases
differently into the cell grid. M2 replaces that with **rigid transforms of stable parts**: a
canonical part's cell set is fixed at authoring time, and a pose only changes *where* that fixed
set lands. A part that doesn't move between two poses contributes **zero churn**; a part that
moves changes only because it actually articulated.

## What the engine owns vs what this module owns

- **Engine (`voxel-convert`) owns** GLB import, the animated-model authority (`ImportedAnimatedModel`:
  node hierarchy, base transforms, raw animation channels, skins, clips), and the *mesh-deformation*
  sampling path (`sample_animation_clip_range` → materialized deformed meshes).
- **This module owns** the rigid interpretation of that authority for the exploded kit: evaluating
  per-node world transforms from the *exposed* channels, the part→bone rig map, and conservative
  rasterization of rigid parts.

We deliberately do **not** call the mesh-deformation sampler: its whole purpose is to materialize a
deformed mesh, which is exactly the operation that causes churn. Evaluating node poses from the
exposed channels is *consumption of engine data structures*, not reimplementation of engine
conversion semantics.

## Pose evaluation

`evaluate_node_poses(model, clip_index, time_microseconds)` returns every node's world transform:

1. Seed each node's local transform from its base (`Decomposed` translation/rotation, or a
   decomposed `Matrix` dropping scale — rigid parts ignore scale).
2. Apply raw animation channels (`Translation`/`Rotation` keys) at the explicit time, honoring
   `Step`/`Linear` interpolation (slerp for rotations). Scale and morph-weight channels do not
   affect rigid transforms.
3. Compose world transforms down the scene hierarchy (`parent ∘ local`).

Deterministic for a given model + clip + time; hierarchy cycles are rejected with a typed error.

## Rig map

`rig-map.json` binds each canonical part to exactly one proxy bone (GLB node index), rigid — one
part, one bone, no skinning weights. `RigMap::validate` enforces schema version, that every part is
bound exactly once, and that every bone index resolves to a real scene node. The checked corpus
(`content/characters/rifleman/rig-map.json`) binds the rifleman parts to the matching retro-character
deform bones (Head/Chest/Hips, Arm/ForeArm, UpLeg/Leg, plus RightHand for the rifle and Chest for
the backpack).

## Conservative rasterization

`rasterize_part(part, transform, settings)` turns a rigid-transformed part into frame cells:

- Each source voxel is treated as an occupied cube (center ± 0.5).
- The cube is supersampled (`supersample` per axis; 2 → 8 sub-samples per voxel) and each
  sub-sample is transformed into frame space and binned into a target cell.
- A target cell is occupied if it collects at least `occupancy_threshold` of its sub-samples;
  it inherits the dominant material slot among its covering source voxels (tie-broken by lowest
  slot then lowest voxel index for determinism), and records its source voxel index as provenance.

This is the technical crux: naive per-voxel rotation leaves holes and unstable thickness; the
supersample-then-vote approach keeps rigid parts **hole-free and face-connected**. Same part +
same transform + same settings is bit-identical.

## Corpus and verification

- The rig-map validates against the rifleman kit and the retro-character animated model.
- Pose evaluation is deterministic and non-degenerate; the `run` clip moves at least one bone
  between t=0 and mid-clip.
- Every bound part rasterizes to coherent, volume-stable geometry at both sampled run poses, and
  a run cycle articulates multiple rigid parts (legs/arms) while still parts stay put.
- An unmoved part has exactly zero churn (measured as the symmetric difference of its cell sets).

Later milestones (M3 joint fusion, M6 temporal review) consume these rough assemblies and measure
the resulting churn reduction against the straight-pipeline baseline in
`evidence/churn-study-high-fidelity.json`.
