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

## The admitted rasterization contract (R6336-8/10/12)

The admitted settings are deliberately bounded to the range where the conservative guarantee is
honest:

- **Volume and connectivity are guaranteed by construction.** `supersample` is admitted only in
  **2..=8**: at supersample 1 each source voxel contributes a single sample, so a rotated thin
  part scatters into diagonally-touching or lost cells — and two distinct voxels can quantize to
  the same target cell with every observed target passing the threshold, leaving no repair
  candidate at all (R6336-12). `occupancy_threshold` is admitted only up to **majority coverage
  (0.5)**: a supermajority threshold is anti-conservative at low supersample — rotated geometry
  rarely covers a supermajority of any cell, so volume collapses below any useful floor — and is
  rejected as **outside the contract** rather than repaired dishonestly.
- **The guarantee mechanism is an injective per-voxel placement, not a fractional floor.** Every
  source voxel owns one distinct output cell: it claims the target cell its transformed center
  bins to, and bin collisions (two voxels quantizing to the same cell) are displaced to the
  nearest unclaimed cell by a deterministic shell search that always lands face-adjacent to the
  occupied set. Combined with the threshold-passing dilation and the connectivity repair, a rigid
  part keeps its **full source volume** (or a conservative dilation) as a **single connected,
  thick body** at every admitted setting — replacing the old `ceil(volume/2)` floor, which still
  accepted losing half a part.
- **Small-cavity preservation is a documented limitation, not an invariant.** Conservative
  dilation (the thing that keeps thin features connected) can fill a small interior hollow when a
  shell is rotated. The contract guarantees volume/connectivity/thickness; it does not guarantee
  that a small enclosed cavity survives. This is recorded honestly rather than claimed as a
  property, and a dedicated test documents the edge while still asserting the volume/connectivity
  guarantee that does hold.

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


## Round-3507 review corrections (bind transform, topology, schedule/assembly)

The first review round (3507) corrected three things and redirected one upstream:

- **Pose evaluation ownership (R6336-1, consumed upstream).** The original local
  `evaluate_node_poses` both duplicated and observably diverged from Engine animation semantics
  (CubicSpline tangent handling, scale/morph policy, duration rejection, base-scale dropping). The
  narrow Engine node-pose provider seam landed as rusty-engine #6348 (approved at exact SHA
  `a867fa9c`) and is now consumed: this module's evaluator is a thin adapter over
  `evaluate_clip_node_poses`, and the divergent local channel evaluator is deleted. The Engine pin
  advanced to `a867fa9c` to take the seam. Equivalence regressions prove out-of-range times are
  rejected (not clamped), the adapter's rigid poses match the Engine seam up to the admitted
  uniform scale, and a one-axis-stretched transform is rejected as non-uniform while the real
  rig is admitted.
- **Admitted rigid-scale policy.** The Engine affine poses are admitted to rigid placement under an
  explicit policy. The retro-character rig is uniformly scaled by ~100 with only floating-point
  jitter between axes, which the Engine's *strict* per-axis uniformity check (absolute
  `1e-6 * max(1, scale)` tolerance) rejects at 100x. Our admitted policy uses the Engine admission
  where it accepts and otherwise re-checks *uniform* scale against a **relative** tolerance
  (`1e-3` of mean scale) — still rejecting truly non-uniform scale, shear, singular axes, and
  reflections — then decomposes the affine matrix by that admitted uniform scale (translation
  divided by scale), so a ~100x rig returns to cell units. Scale is decomposed deliberately via the
  admitted `uniform_scale`, never silently dropped.
- **Bind transform (R6336-2).** `PartBinding` now carries an explicit `bindTransform` (rotation +
  translation) from the part's pivot frame into the bone's bind frame, validated as finite with a
  unit quaternion. Rasterization composes `bone_pose ∘ bindTransform ∘ part_local`, so parts land
  spatially aligned on their bones. A neutral-reconstruction test proves that at bind pose the
  transformed parts reconstruct the M1 assembled rifleman (correct vertical extent, coherent
  composition), not a pile of overlapping parts.
- **Topology-preserving rasterization (R6336-4).** The rasterizer no longer emits only
  threshold-passing cells: it emits the high-confidence body, then adds sub-threshold cells that
  are required to hold the part's face-connectivity (best-bridge-first, deterministic). Combined
  with dual-grid binning (nearest cell-center), a rigid part now stays a single connected component
  even for thin features — verified by the exact 2-cell-bar regression and a corpus-scale BFS check
  over every rifleman part at two run poses.
- **Pose selection + rough assembly (R6336-3).** `select_pose_schedule` keeps first/last and
  event frames, reduces the rest under a pose-space error budget, and emits independent per-frame
  durations (poses strictly within `[0, duration)`, the final pose holding to the clip end).
  `assemble_rough_frame` merges all bound parts for a selected pose into one rough frame with
  canonical part/voxel provenance and `needs_fusion` flags on joint/overlap regions, so M3 sees
  exactly what to fuse rather than the whole frame.

## The pose-selection contract (R6336-7/9/11)

`select_pose_schedule` guarantees **both** the hard frame cap and the error budget — neither is
best-effort:

- **Hard anchors** are the first pose, the true tail of the native timeline (Last), and every
  caller-authored mandatory timestamp. If those alone exceed `max_frames`, selection fails with a
  typed impossibility rather than silently dropping a mandatory anchor.
- **The complete error-bounded schedule is staged up front**: the hard anchors plus the minimal
  greedy subdivisions that bring every retained interval within `error_budget`. If the staged
  schedule does not fit under `max_frames`, selection fails with a typed impossibility naming the
  required frame count — never an overflow of the cap, and never a partial schedule whose
  intervals silently exceed the budget.
- **Feasibility floor**: a budget below the clip's maximum indivisible adjacent-tick pose error is
  unsatisfiable at tick resolution and is rejected with a typed impossibility naming that floor.
- **Event frames are best-effort**: they fill only slots the staged schedule did not need, and an
  event is kept only where splitting its interval keeps both halves within the budget, so every
  returned interval measures within budget.
