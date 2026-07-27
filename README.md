# Rusty Engine Voxels

`rusty-engine-voxels` is the dedicated downstream Studio project for voxel conversion, animated
voxel-object playback, quality experiments, and future voxel-specific feature proof. It depends on
an exact public Rusty Engine revision and does not inspect `../rusty-engine` or
`../rusty-engine-demo` during ordinary work.

The first checked experiment converts Kenney's CC0 retro character GLB into a canonical voxel
object with `idle`, `run`, and `jump` flipbook clips. The same project then admits the serialized
object through `voxel-object-runtime`, samples explicit playback times, and builds a complete
shared-renderer frame through `render-projection`.

## First experiment

The checked Kenney source contains 1,029 vertices, 1,604 triangles, and the `idle`, `run`, and
`jump` clips. At a 24 × 36 × 24 conversion grid, 0.125-unit cells, and six samples per second, the
experiment:

- sampled 16 source poses and stored 15 runtime frames;
- deduplicated those frames to 14 unique voxel meshes;
- produced 9,650 aggregate conversion voxels in a 656,537-byte canonical object;
- loaded the object with all three clips through the strict runtime admission path; and
- projected one selected `run` frame as a real shared-renderer voxel-object instance.

This replaces the initial 12 × 18 × 12, 0.25-unit proof. Its four `run` poses contained
135–145 voxels each; the finer artifact contains 603–616 per pose, giving the deformation roughly
four times as many occupied cells with which to express motion.

The exact identities, per-clip counts, and explicit-time playback samples are checked in
`evidence/initial-animated-voxel-report.json`. Machine-specific timings are evidence, not pass/fail
thresholds.

## Commands

```bash
# Rebuild the canonical object and update its content-addressed project reference.
cargo run --locked --bin voxel-lab -- convert

# Strictly load, admit, play, and project the checked object.
cargo run --locked --bin voxel-lab -- load

# Run all deterministic and integration checks.
./scripts/verify.sh

# Validate this adapter against the pinned Studio protocol decoder.
./scripts/verify-studio.sh

# Launch the pinned Rusty Engine Studio with this project's adapter.
./scripts/studio.sh

# Bind the same Studio project for trusted-LAN use.
./scripts/studio.sh --host 0.0.0.0 --port 4310

# Or let den-serve own the process and LAN address.
den-serve up rusty-engine-voxels -repo /home/dev/rusty-engine-voxels
```

`studio.sh` checks out the exact provider revision from `engine-source.json` into the ignored
`.studio-cache`, installs its locked Studio workspace, builds this repository's Rust adapter, and
prints an auto-open URL for the checked voxel-lab project. The project can therefore use Studio
without an operational sibling checkout or a copy of Studio source.

The Studio project file is
`content/projects/voxel-lab.project.json`. The adapter owns this schema and supports opening,
reading, inspecting sources, preparing/previewing/applying/discarding voxel-object conversions,
attaching transformed voxel-object instances, and transiently playing reopened applied instances.
Other protocol-9 operations fail with a typed
unsupported-operation rejection rather than a generic command tunnel.

Applied playback retains the admitted object and renderer projector for the open project. After the
initial complete frame, ordinary animation samples carry one `setVoxelObjectFrame` operation; Studio
waits for that pose to reach the shared renderer and then displays it for its authored duration before
requesting the next virtual-time sample. Slow authoring hosts therefore reduce playback speed without
skipping stored poses, and `once` visibly settles on its final frame.

Conversion publication is content-addressed and idempotent. Rebuilding identical source and
settings reuses the same canonical object and does not increment the project revision. A changed
result is written immutably first; the project document then atomically moves its reference.

## Ownership

- This repository owns project content, paths, material colors, scene instances, experiment
  schedules, and collision policy.
- Rusty Engine owns source parsing, animation deformation, voxelization, canonical object bytes,
  runtime admission, explicit-time playback, mesh construction, and renderer-neutral frames.
- Studio owns transient forms, candidate selection, sampling cadence, scrubbing controls, and
  presentation; Rust owns applied-instance clip timing and frame selection.

The initial source asset and license live together under
`content/sources/kenney-retro-character/`.

See `docs/design.md` for the project/provider/Studio boundary and the intended shape of future
experiments.
