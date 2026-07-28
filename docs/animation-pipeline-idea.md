# Baked Voxel Character Animation Pipeline

**Status:** Implementation proposal
**Version:** 0.1
**Primary model:** Canonical voxel parts + proxy-rig pose + deterministic assembly + agent cleanup
**Runtime output:** Rig-free voxel flip-book animation

## 1. Purpose

Build a production pipeline that converts character motion into stylized, frame-based voxel animation without shipping a skeletal rig at runtime.

The pipeline separates:

* **Character identity:** canonical voxel parts, proportions, colors, equipment, and silhouette rules
* **Motion:** proxy-rig transforms derived from authored animation, motion capture, or multiview video
* **Pose-specific interpretation:** joint fusion, controlled deformation, silhouette adjustment, and artistic cleanup
* **Runtime representation:** immutable voxel frames with anchors, timing, and optional collision metadata

The canonical exploded character remains the source of truth. Motion sources describe where the parts should move, not what those parts should become.

---

## 2. Goals

The system should:

1. Produce visually stable voxel flip-book animations from ordinary rigged motion.
2. Reuse one verified character representation across all animations.
3. Allow video-generated motion to be fitted to a simple proxy rig.
4. Give agents structured, bounded voxel-editing operations rather than unrestricted file access.
5. Preserve provenance so every final voxel can be traced to a canonical part or a generated seam operation.
6. Detect temporal boiling, proportion drift, disconnected geometry, and palette instability automatically.
7. Permit regeneration after modifying a canonical part without manually rebuilding every animation frame.
8. eliminate runtime skinning, live bones, blend trees, and deformation bugs.

## 3. Non-goals

The initial system does not attempt to provide:

* General-purpose video-to-3D reconstruction
* Photorealistic anatomy
* Smooth runtime skeletal animation
* Runtime inverse kinematics
* Arbitrary procedural deformation
* Pixel-perfect reproduction of generated video
* Fully automatic final-art approval

The intended result is a stylized interpretation with strong consistency, not an exact volumetric scan.

---

# 4. System Architecture

```text
Character references
        ↓
Canonical exploded voxel kit
        ↓
Canonical neutral assembly
        ↓
Proxy rig definition
        ↓
Motion source
  ├── authored animation
  ├── mocap
  └── multiview generated video
        ↓
Proxy-rig animation
        ↓
Pose sampling
        ↓
Per-frame part transformation
        ↓
Conservative voxel rasterization
        ↓
Deterministic joint fusion
        ↓
Structural validation
        ↓
Agent geometry cleanup
        ↓
Multiview visual review
        ↓
Temporal consistency review
        ↓
Human exception review
        ↓
Compiled voxel flip-book
```

The pipeline should behave like an asset compiler. Every stage receives versioned inputs and produces reproducible outputs.

---

# 5. Recommended Technology Boundary

## Rust core

Rust should own:

* Sparse voxel representation
* Transform and rasterization
* Socket assembly
* Joint bridging
* Morphological cleanup
* Connectivity analysis
* Provenance tracking
* Structural validation
* Temporal metrics
* Compiled runtime format
* Deterministic application of agent edit operations

## TypeScript orchestration

TypeScript should own:

* Pipeline job orchestration
* Build graph and caching
* Agent task creation
* Render-review coordination
* Human review interface
* Artifact browsing
* Build reports
* Prompt and model-provider integration

## Render service

The render service may use the existing engine renderer. It should expose a deterministic headless interface:

```text
render-frame
render-turntable
render-id-pass
render-depth-pass
render-difference
```

The compiler should not require Blender after source assets have been imported.

---

# 6. Coordinate and Scale Conventions

Choose one convention and enforce it across all stages.

Recommended defaults:

* Right-handed coordinates
* Y axis is up
* Character faces negative Z in neutral orientation
* Integer voxel coordinates
* One canonical voxel size per character
* Character origin at ground-center beneath the pelvis
* Local part origins placed at their primary parent joint
* Rotations represented as normalized quaternions
* Transforms applied in parent-to-child rig order

Each character package must declare:

```json
{
  "coordinate_system": "right_handed_y_up",
  "forward_axis": "-Z",
  "voxel_size_meters": 0.04,
  "ground_y": 0,
  "neutral_facing": [0, 0, -1]
}
```

Changing voxel scale should require a new canonical-kit version.

---

# 7. Character Package Layout

```text
characters/
  rifleman/
    character.json
    palette.json

    parts/
      head.vxl
      torso.vxl
      pelvis.vxl
      left_upper_arm.vxl
      left_lower_arm.vxl
      right_upper_arm.vxl
      right_lower_arm.vxl
      left_upper_leg.vxl
      left_lower_leg.vxl
      right_upper_leg.vxl
      right_lower_leg.vxl
      rifle.vxl
      backpack.vxl

    rig/
      proxy.glb
      rig-map.json
      id-colors.json

    reference/
      neutral/
        front.png
        side.png
        back.png
        three-quarter.png
      generated-video/
        source.mp4
        camera-layout.json

    animations/
      walk/
        motion.glb
        clip.json
      fire-standing/
        motion.glb
        clip.json

    builds/
      walk/
        frames/
        renders/
        reports/
        patches/

    output/
      rifleman.vxa
```

A standard format such as glTF 2.0 is suitable for the proxy rig and animation. Canonical voxel parts need a custom sidecar even when using `.vox`, because ordinary voxel formats do not retain sockets, provenance, or deformation rules.

---

# 8. Canonical Exploded Voxel Kit

## 8.1 Default part breakdown

The recommended humanoid kit is:

* Head
* Torso
* Pelvis
* Left and right upper arms
* Left and right lower arms
* Left and right upper legs
* Left and right lower legs
* Optional hands and feet
* Equipment as separate parts
* Large clothing elements as separate parts when they require independent motion

A coarser six-part representation may be supported, but splitting elbows and knees greatly reduces pose-reconstruction work.

## 8.2 Part record

```json
{
  "id": "left_lower_arm",
  "version": 3,
  "pivot": [2, 11, 1],
  "bounds": {
    "min": [-3, -10, -3],
    "max": [4, 12, 4]
  },
  "parent_bone": "lower_arm.L",
  "palette_groups": ["coat", "glove"],
  "symmetry_partner": "right_lower_arm",
  "deformation_budget": {
    "max_length_change": 0.04,
    "max_volume_change": 0.08,
    "allow_joint_compression": true
  },
  "sockets": [
    {
      "id": "elbow",
      "position": [1, 10, 0],
      "forward": [0, 1, 0],
      "radius": 2.5,
      "mate": "left_upper_arm.elbow"
    },
    {
      "id": "wrist",
      "position": [0, -9, 0],
      "forward": [0, -1, 0],
      "radius": 2.0
    }
  ]
}
```

## 8.3 Voxel provenance

Each canonical voxel should possess a stable identity:

```rust
struct CanonicalVoxelId {
    part_id: PartId,
    voxel_id: u32,
}
```

A posed voxel should retain:

```rust
enum VoxelOrigin {
    Canonical(CanonicalVoxelId),
    JointBridge {
        socket_a: SocketId,
        socket_b: SocketId,
    },
    CleanupGenerated {
        operation_id: OperationId,
    },
}
```

This provenance allows the compiler to distinguish intentional character structure from frame-specific filler.

## 8.4 Identity invariants

Each part may declare invariants such as:

* Fixed weapon length
* Fixed head dimensions
* Minimum limb thickness
* Palette groups that cannot be replaced
* Regions that may not be removed
* Regions that may deform near joints
* Approximate volume range
* Required socket positions

The canonical kit is authoritative whenever motion references disagree with it.

---

# 9. Canonical Neutral Assembly

Every character must have one approved assembled neutral pose.

This artifact serves as:

* Proportion baseline
* Palette baseline
* Gameplay-distance preview
* Default attachment reference
* Character identity reference
* Regression-test target

The neutral assembly should be generated from the exploded kit rather than maintained as an independent model.

The compiler must verify that rebuilding the neutral assembly from the parts produces the approved result within declared tolerances.

---

# 10. Proxy Rig

The proxy rig is deliberately simple. It exists to describe motion, not final geometry.

## 10.1 Requirements

The proxy must contain:

* Stable bone names
* Parent hierarchy
* Bind transforms
* Joint limits
* Ground and root bones
* Part-to-bone mapping
* Optional contact markers
* Optional weapon or equipment bones

The proxy can be a mannequin, colored block figure, or low-detail mesh.

## 10.2 Body-part ID colors

Each region receives a fixed flat color:

```json
{
  "head": "#FFFF00",
  "torso": "#00FF00",
  "pelvis": "#00FFFF",
  "left_upper_arm": "#8000FF",
  "left_lower_arm": "#FF00FF",
  "right_upper_arm": "#FF8000",
  "right_lower_arm": "#FF0000",
  "left_upper_leg": "#0040FF",
  "left_lower_leg": "#0080FF",
  "right_upper_leg": "#00A080",
  "right_lower_leg": "#00D0A0",
  "weapon": "#FFFFFF"
}
```

The exact colors do not matter as long as they are:

* Maximally distinguishable
* Unlit
* Consistent across animations
* Never reused for unrelated parts

## 10.3 Render passes

For each sampled pose, render:

1. **Beauty pass:** readable shaded proxy or reference character
2. **ID pass:** flat body-region colors
3. **Depth pass:** linear camera-space depth
4. **Normal pass:** optional
5. **Silhouette mask**
6. **Joint overlay:** optional diagnostic pass

Recommended cameras:

* Front
* Back
* Left
* Right
* Front-left three-quarter
* Front-right three-quarter
* Gameplay camera

Orthographic renders are preferable for geometric comparison. Perspective gameplay renders are preferable for artistic review.

---

# 11. Video-to-Proxy Motion Fitting

Generated video is treated as motion evidence, not character geometry.

## 11.1 Input expectations

The strongest input is a synchronized panel containing:

* Front view
* Side view
* Back view
* Optional three-quarter view
* Shared frame timing
* Stable camera directions
* Minimal camera movement
* Neutral background

Camera calibration may be approximate. The system only needs a plausible proxy pose, not exact reconstruction.

## 11.2 Motion-fitting stages

```text
video panels
    ↓
panel extraction
    ↓
frame synchronization
    ↓
2D landmark estimation
    ↓
cross-view landmark association
    ↓
proxy-rig inverse kinematics
    ↓
temporal smoothing
    ↓
contact correction
    ↓
human or agent exception cleanup
    ↓
motion.glb
```

## 11.3 Landmarks

At minimum, extract:

* Head center
* Neck
* Shoulders
* Elbows
* Wrists
* Pelvis
* Hips
* Knees
* Ankles
* Feet
* Weapon endpoints when relevant

The fitter should preserve bone lengths from the proxy rig rather than inferring new lengths per frame.

## 11.4 Fitting objective

Conceptually:

```text
multiview landmark error
+ joint-limit penalty
+ bone-length penalty
+ temporal acceleration penalty
+ foot-sliding penalty
+ root-motion penalty
```

Generated-video inconsistencies are resolved in favor of:

1. Proxy bone lengths
2. Temporal continuity
3. Ground contacts
4. The most legible camera view

After fitting, the original video is retained only as a soft visual reference.

---

# 12. Pose Sampling

The final voxel animation should not necessarily preserve every source frame.

## 12.1 Inputs

* Proxy-rig animation
* Target stepped frame rate
* Required event frames
* Motion-error tolerance
* Contact events

## 12.2 Sampling rules

Always retain:

* First and last clip frames
* Foot-contact changes
* Weapon discharge frames
* Impact frames
* Major anticipation and recovery poses
* Frames containing animation events

Between mandatory frames, reduce the animation using a pose-space error metric based on:

* Bone angular difference
* Root displacement
* Hand and foot displacement
* Weapon orientation
* Silhouette difference

A typical walk cycle might compile to 6–12 voxel poses even when the source animation contains dozens of frames.

Frame durations should be stored independently, allowing uneven holds.

---

# 13. Per-Frame Initial Assembly

For every retained pose:

1. Load canonical parts.
2. Apply bone transforms to each part.
3. Transform sockets and anchors.
4. Rasterize transformed voxels into frame space.
5. Resolve overlapping canonical voxels.
6. Preserve provenance.
7. Record disconnected components before fusion.

## 13.1 Conservative voxel rasterization

Rotating integer voxels directly will create holes and unstable thickness. Instead:

* Treat canonical voxels as occupied cubes.
* Transform each cube into pose space.
* Rasterize conservatively into the target grid.
* Optionally supersample at 2× or 4× resolution.
* Downsample using occupancy thresholds and material voting.

The same source part and transform must always produce identical results.

## 13.2 Overlap resolution

When parts overlap:

1. Prefer voxels marked as structurally protected.
2. Prefer outer-surface material when the overlap is visible.
3. Use parent-part priority near sockets.
4. Preserve canonical provenance where possible.
5. Record discarded origins for diagnostics.

---

# 14. Deterministic Joint Fusion

Joint fusion should solve the obvious mechanical problems before involving an LLM.

## 14.1 Socket bridge

For each paired socket:

* Calculate the transformed socket centers.
* Generate a capsule, tapered cylinder, or ellipsoid between them.
* Restrict generated voxels to a local joint bounding region.
* Inherit material from nearby canonical voxels.
* Avoid filling intentional negative space.

## 14.2 Standard cleanup passes

Run configurable operations such as:

* Fill one-voxel cavities
* Remove isolated single voxels
* Bridge components separated by one voxel
* Enforce minimum limb thickness
* Trim deep interpenetration
* Repair socket neighborhoods
* Preserve declared holes
* Restore ground contact
* Normalize weapon thickness

Every generated voxel must be labeled with its generating operation.

---

# 15. Agent Edit Contract

Agents should never rewrite the voxel file directly. They should return bounded operations through an edit DSL.

## 15.1 Agent input bundle

For one frame, provide:

```text
frame.vxl
frame-metadata.json
canonical-part summaries
current metrics
structural warnings
style-rules.md
front/back/side renders
ID-pass renders
difference overlays
previous approved frame
next rough frame
proxy transforms
reference-video frames, when available
```

Use a three-frame temporal window by default:

```text
previous frame
current frame
next frame
```

This is enough to detect most boiling without overloading the reviewer.

## 15.2 Agent response

```json
{
  "diagnostics": [
    {
      "code": "LEFT_FOREARM_THIN",
      "region": "left_lower_arm",
      "views": ["left", "front_left"],
      "summary": "Forearm loses two voxels of thickness near the elbow."
    }
  ],
  "operations": [
    {
      "op": "thicken_region",
      "region": "left_lower_arm",
      "bbox": {
        "min": [-7, 29, -3],
        "max": [-2, 36, 3]
      },
      "axis": "local_x",
      "amount": 1,
      "preserve_silhouette_views": ["front"]
    }
  ],
  "expected_effects": [
    "Restore canonical forearm thickness",
    "Preserve frontal silhouette"
  ]
}
```

The agent provides concise diagnostics, not hidden reasoning.

## 15.3 Initial edit operations

The first DSL version should support:

* `add_voxel`
* `remove_voxel`
* `move_voxel`
* `fill_box`
* `clear_box`
* `replace_material`
* `bridge_regions`
* `thicken_region`
* `thin_region`
* `copy_canonical_region`
* `restore_from_previous_frame`
* `restore_from_next_frame`
* `smooth_local_surface`
* `carve_local_surface`
* `enforce_connectivity`
* `shift_component`
* `set_anchor`

Each operation must specify an affected bounding box.

## 15.4 Safety rules

The compiler rejects operations that:

* Affect undeclared regions
* Exceed a maximum voxel count
* Violate protected-region rules
* Change weapon dimensions beyond tolerance
* Remove required anchors
* Introduce invalid palette entries
* Produce disconnected required components
* Modify previous or next frames implicitly

Rejected patches are returned with machine-readable errors.

---

# 16. Multiview Review Loop

After applying a patch:

1. Re-run structural validation.
2. Re-render canonical views.
3. Recalculate visual metrics.
4. Compare against the pre-patch state.
5. Accept, reject, or request a revised patch.

Recommended limit:

* One deterministic pass
* Up to three agent geometry passes
* One temporal pass
* Human review only when gates remain unresolved

The system should avoid endless polish loops. A patch is accepted when it clears hard gates and meaningfully improves the weighted review score without introducing new regressions.

---

# 17. Visual Metrics

Visual metrics are advisory rather than absolute because stylization may intentionally diverge from the proxy.

Useful measurements include:

* Silhouette intersection-over-union by view
* Part-ID region overlap
* Bounding-box agreement
* Limb endpoint displacement
* Weapon orientation difference
* Ground-contact error
* Screen-space thickness of protected features
* Palette-region coverage
* Gameplay-distance readability

The proxy ID pass is especially useful for detecting:

* Left/right swaps
* Hidden or missing limbs
* Incorrect overlap order
* Torso engulfing an arm
* Weapon merging into the body

---

# 18. Temporal Consistency

Temporal validation should operate both numerically and visually.

## 18.1 Per-region metrics

Track by frame:

* Voxel count
* Bounding dimensions
* Centroid
* Principal axes
* Palette histogram
* Surface area
* Connected-component count
* Distance between sockets
* Screen-space silhouette area

Compare these values with neighboring frames and canonical tolerances.

## 18.2 Provenance consistency

Canonical voxels should move according to their source part unless deliberately replaced.

Flag:

* Canonical voxel identities appearing and disappearing rapidly
* Large synthetic seam regions
* Material changes unrelated to visibility
* Equipment dimensions changing between frames
* Head or torso volume drift

## 18.3 Anchor trajectories

Track anchors such as:

* Head center
* Chest
* Pelvis
* Feet
* Hands
* Muzzle
* Weapon grip
* Effect origin

Anchor trajectories should follow the proxy rig unless an explicit correction is recorded.

## 18.4 Flicker renders

Generate:

* Alternating-frame GIF or video
* Three-frame onion-skin overlay
* Per-pixel temporal difference heat map
* Silhouette-edge motion view
* Palette flicker map

These often reveal defects that are inconspicuous in isolated renders.

---

# 19. Style Rules

Each character or project should provide machine-readable stylistic constraints.

Example:

```json
{
  "minimum_feature_thickness": 2,
  "maximum_isolated_component_size": 0,
  "preferred_diagonal_pattern": "two_one_stair",
  "head_exaggeration": 1.12,
  "hand_exaggeration": 1.2,
  "weapon_exaggeration": 1.08,
  "allow_single_voxel_highlights": true,
  "allow_single_voxel_geometry": false,
  "palette_limit": 24,
  "silhouette_priority_views": [
    "gameplay",
    "front",
    "side"
  ]
}
```

Rules should describe the desired visual language rather than merely preventing errors.

The cleanup process is allowed to diverge from the proxy in order to satisfy these rules.

---

# 20. Hard and Soft Validation Gates

## Hard failures

A frame cannot compile when:

* A required body region is missing
* Required geometry is disconnected
* A protected region was removed
* An anchor is absent
* The frame exceeds runtime bounds
* An invalid material appears
* Provenance data is malformed
* Weapon dimensions exceed hard tolerance
* The root or ground plane is invalid

## Soft warnings

A frame may compile with warnings for:

* Silhouette mismatch
* Unusual volume change
* Excess synthetic joint voxels
* Palette histogram drift
* Minor foot sliding
* Marginal feature thickness
* High visual difference from neighboring frames

Human approval can waive soft warnings with an attached note.

---

# 21. Build Provenance

Every compiled frame should have a derivation record:

```json
{
  "character": "rifleman",
  "canonical_kit_version": 3,
  "animation": "walk",
  "source_clip_hash": "sha256:...",
  "source_time_seconds": 0.4167,
  "proxy_pose_hash": "sha256:...",
  "compiler_version": "0.1.0",
  "deterministic_passes": [
    "conservative_rasterize",
    "socket_bridge_v2",
    "small_cavity_fill"
  ],
  "agent_patches": [
    "patch-0031.json",
    "patch-0032.json"
  ],
  "human_approval": {
    "status": "approved",
    "note": null
  }
}
```

A canonical-part change should invalidate only dependent frames.

Build caching should key off:

* Part versions
* Proxy pose
* Style rules
* Compiler version
* Agent patch sequence

---

# 22. Runtime Output

The compiled runtime asset contains no skeleton.

```rust
struct VoxelAnimation {
    clips: Vec<VoxelClip>,
    palettes: Vec<Palette>,
}

struct VoxelClip {
    id: ClipId,
    loop_mode: LoopMode,
    frames: Vec<VoxelFrameRef>,
}

struct VoxelFrameRef {
    asset: VoxelAssetId,
    duration_ms: u16,
    root_delta: Vec3,
    anchors: Vec<FrameAnchor>,
    collision: Option<CollisionFrame>,
    events: Vec<AnimationEvent>,
}
```

Each voxel frame contains:

* Compressed voxel occupancy
* Palette or material indices
* Pivot
* Bounds
* Optional normal/AO metadata
* Per-frame anchors
* Optional coarse collision primitives

The runtime player only selects frames based on elapsed clip time.

---

# 23. Collision and Attachments

Do not derive gameplay collision directly from visible voxels unless the game specifically needs it.

Prefer:

* Capsule or box body collision
* A few per-frame hit regions
* Stable collision across neighboring poses
* Explicit weapon or effect anchors
* Optional tagged damage regions

Useful anchors include:

* `muzzle`
* `right_hand`
* `left_hand`
* `head`
* `chest`
* `pelvis`
* `left_foot`
* `right_foot`
* `weapon_socket`
* `effect_origin`

These preserve practical rig functionality without retaining a runtime hierarchy.

---

# 24. Human Review Interface

The review UI should display:

* Current voxel frame
* Canonical neutral character
* Exploded canonical parts
* Proxy beauty and ID views
* Previous and next frames
* Structural warnings
* Agent diagnostics
* Applied patch history
* Flicker preview
* Provenance inspection for selected voxels

Useful actions:

* Approve
* Reject patch
* Re-run agent review
* Restore canonical region
* Copy region from neighbor
* Mark intentional deviation
* Promote correction into a canonical-part revision

That final action is important: repeated frame corrections may indicate that the canonical source itself should change.

---

# 25. Initial Vertical Slice

Implement one narrow test:

## Character

A chunky humanoid rifleman with:

* Twelve body parts
* Rifle
* Hat or helmet
* Backpack
* Limited palette

## Source

* One multiview generated walk video
* One fitted proxy-rig walk
* One approved canonical exploded kit

## Output

* Eight stepped walk poses
* Four canonical review cameras
* Runtime flip-book playback
* Muzzle and hand anchors

## Required automatic checks

* Connectivity
* Ground contact
* Head dimensions
* Rifle dimensions
* Limb volume drift
* Palette stability
* Neighbor-frame silhouette difference
* Anchor trajectory continuity

## Success criteria

The prototype succeeds when:

1. The walk reads correctly from arbitrary gameplay angles.
2. The character remains recognizably identical across all eight frames.
3. Regenerating a frame is deterministic.
4. Modifying one canonical part propagates through the clip.
5. The agent fixes at least one meaningful geometry defect through the edit DSL.
6. Human cleanup is localized rather than equivalent to resculpting each frame.
7. The runtime requires no rig or skinning system.

---

# 26. Suggested Implementation Order

## Milestone 1: Canonical assembly

* Define voxel and part formats
* Implement pivots and sockets
* Build neutral character from exploded parts
* Add provenance
* Add headless turntable rendering

## Milestone 2: Rig pose conversion

* Import glTF skeleton and animation
* Map parts to bones
* Transform and conservatively rasterize parts
* Produce rough per-frame assemblies

## Milestone 3: Deterministic fusion

* Socket bridging
* Overlap resolution
* Connectivity repair
* Minimum-thickness rules
* Structural validation

## Milestone 4: Runtime flip-book

* Compile voxel frames
* Store timing and anchors
* Play animation without a rig
* Validate arbitrary camera views

## Milestone 5: Agent cleanup

* Define patch DSL
* Build frame-review bundles
* Apply and validate bounded patches
* Add multiview re-render loop

## Milestone 6: Temporal review

* Add per-region metrics
* Add provenance drift detection
* Add flicker and onion-skin renders
* Add neighboring-frame repair operations

## Milestone 7: Video motion fitting

* Parse multiview panel video
* Extract landmarks
* Fit the proxy rig
* Smooth motion and correct contacts
* Retain source video as soft reference

Video fitting comes last because the rest of the pipeline can first be validated with an ordinary rigged animation. That cleanly separates failures in motion extraction from failures in voxel generation.

---

# 27. Governing Principle

The system should always preserve this hierarchy:

```text
Canonical kit defines character identity.
Proxy rig defines pose.
Reference video informs motion and silhouette.
Deterministic tools perform routine conversion.
Agents perform bounded interpretation and cleanup.
Humans resolve only consequential ambiguity.
Runtime receives immutable compiled frames.
```

A new animation is therefore not a new voxel character. It is a new arrangement and controlled deformation of an already verified one.
