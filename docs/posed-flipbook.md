# Posed flipbook (rig-free kit animation in Studio)

The posed flipbook closes the loop the manual pivoting experiment
(`docs/kit-pivoting.md`) opened: poses are **authored data**, compiled into a
canonical Engine voxel object that Rusty Studio loads, scrubs, and plays like
any other voxel-object clip. No rig, no skinning, no mesh conversion — the
source is a canonical exploded kit (`docs/kit-format.md`) plus per-part pivot
rotations. Playback is the Engine's explicit-time complete-frame switching, so
motion is harsh pose-to-pose jumps by construction; nothing interpolates.

## Pipeline

```text
poses.json (frames = per-part euler deltas + single-level chains + durations)
        │
        ▼  assemble_placed_frame (rig-free rigid rasterization, src/assemble.rs)
        │  one complete rough frame per pose, cumulative explicit times
        ▼  optional cellDownsample (power-of-two lattice binning, majority slot)
        ▼  compile_posed_flipbook (src/flipbook.rs)
        │  one clip, per-frame durations, +X run-length encoded sparse runs
        ▼  publish_compiled_flipbook (immutable content-addressed bytes)
        ▼  content/voxel-objects/posed-<id>-<hash>.voxel-object.json
        ▼  Studio project file references the object + one instance
```

## Pose-spec format (schema v1)

`content/characters/knight/poses/walk.poses.json` is the checked example:

- `id` — pose-set identity; the compiled asset id is `voxel-object/posed-<id>`.
- `kit` — project-relative path of the canonical kit JSON.
- `cellDownsample` — optional `1 | 2 | 4 | 8` lattice downsample applied to
  every assembled frame (the object's cell size grows by the same factor).
  Kit-scale characters exceed the Engine's 64 MiB artifact bound at full
  resolution: the 292-cell knight at factor 1 compiled to ~70 MiB and was
  rejected; factor 2 lands at ~15 MiB with a ~146-cell character — retro
  high-fidelity scale.
- `clipId` / `clipName` — the single output clip.
- `frames[]` — `name`, `durationMicroseconds`, `deltas[]` (per-part euler
  degrees about the part's own pivot, X then Y then Z), `chains[]`
  (`child` inherits `parent`'s own delta as a rotation about the parent's
  neutral pivot; single-level only, so hands/weapons follow arms).

Validation (`PoseSpecDocument::validate`) names the offending frame and part
for unknown parts, duplicate deltas/chains, self-chains, multi-level chains,
bad durations, and duplicate frame names.

## The knight walk (checked)

Four frames at 6.25 fps (walk_a 180 ms, pass 140 ms, walk_b 180 ms, pass
140 ms), authored from the pivot experiment's validated deltas:

- 46.6k–48.1k voxels/frame (from 168k canonical cells at downsample 2),
  ~14.5k sparse runs/frame after +X run-length encoding;
- 15,307,255-byte artifact (64 MiB bound), 189,629 total voxels
  (16.7 Mi bound), 5 runtime frames (8,193 bound);
- asset `voxel-object/posed-knight-walk`, content hash pinned in
  `tests/posed_flipbook_experiment.rs`;
- evidence with ASCII review renders: `evidence/kit-poses-knight.json`.

Regenerate:

```bash
cargo run --locked --release --bin voxel-kit-lab -- poses \
  --spec content/characters/knight/poses/walk.poses.json \
  --out content/voxel-objects \
  --report evidence/kit-poses-knight.json
```

Use `--release`: debug assembly is tens of seconds per kit-scale pose.

## Viewing in Studio

The checked project is `content/projects/knight-flipbook.project.json` — one
instance of the posed object at the origin, palette materials bound from the
kit. Launch Studio normally:

```bash
./scripts/studio.sh
```

then open the same URL it prints but with the project query parameter swapped:

```text
http://<host>:4310/?root=<repo>&project=content%2Fprojects%2Fknight-flipbook.project.json
```

The voxel-object inspector scrubs and plays `clip/walk` through the existing
`previewVoxelObjectInstance` path (verified by
`scripts/posed-flipbook-smoke.mjs`, run the same way as
`scripts/verify-studio.sh`).

Two schema notes, honestly: project schema v3 requires a well-formed
`conversion` block even when unused, so the knight project carries the knight
GLB's identity there as schema furniture — the registered object comes from
the pose pipeline, not a mesh conversion. Because the GLB is static, the
block declares no clips (`clips: []`, empty `defaultClip`); project
validation was relaxed to admit exactly that case, since Studio's shared
renderer rejects advertised clips the source does not contain. And the
project file itself is a hand-authored checked artifact (like the two
existing projects); the adapter hash-validates every referenced object at
open.

## Caveats

- Frames are **rough**: rotating limbs tear seams at part boundaries (~40k
  fusion candidates per pose at full resolution). M3 fusion is rig-bound and
  out of scope here; at downsample 2 the tears shrink but are not gone. This
  is the honest state of the concept test.
- Downsampled frames drop per-part provenance and fusion flags (majority-slot
  binning is the only authority).
- Anchors, hit regions, and collision facts are not compiled (the posed path
  has no placements to resolve them against); instance collision policy is
  `none`.
- The `conversion` block is unused for this project; driving an actual mesh
  conversion from it is not a supported workflow. Use the voxel-lab projects
  for conversion work.
- Verified in Studio: the project opens with no rejections, all four frames
  scrub to distinct runtime frames with `setVoxelObjectFrame` projection ops
  (`evidence/posed-flipbook-studio-smoke.json`), and frames 0 and 2 render
  visibly different poses (`evidence/posed-flipbook-frame0.png`,
  `evidence/posed-flipbook-frame2.png`).
