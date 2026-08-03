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

The separate baked-character experiment now has a deterministic exploded-kit
path through pose selection, socket-constrained rigid placement, conservative
rasterization, and first-pass joint fusion. See
[`docs/joint-fusion.md`](docs/joint-fusion.md) and the checked comparison in
`evidence/joint-fusion-study.json`.

That fused schedule now compiles into the canonical Engine voxel-object format
with exact frame timing, pose-following anchors, coarse collision facts,
content-addressed publication, explicit-time playback, and shared-resource
renderer projection. See
[`docs/baked-flipbook-runtime.md`](docs/baked-flipbook-runtime.md) and
`evidence/baked-flipbook-runtime.json`.

Finished frames can also pass through the bounded replayable cleanup DSL before
publication. Its agent bundle, fail-atomic safety policy, accept/revise loop,
and checked rifleman forearm repair are documented in
[`docs/cleanup-loop.md`](docs/cleanup-loop.md), with exact evidence in
`evidence/cleanup-loop.json`.

The finished schedule is then checked across time for per-part shape and
palette drift, canonical identity stability, anchor trajectories, and
joint-localized churn. Deterministic GIF/onion-skin/difference views make the
same facts reviewable by a human. See
[`docs/temporal-consistency.md`](docs/temporal-consistency.md) and
`evidence/temporal-consistency.json`.

The optional final input experiment fits a synchronized four-view video into
that same rigid proxy rather than deriving voxel geometry from pixels. Its
bounded Rust triangulation, fixed bone lengths, contact correction,
Rust-retargeted `motion.glb`, and direct M2–M6 proof are documented in
[`docs/video-motion-fitting.md`](docs/video-motion-fitting.md).

## High-fidelity experiment

The second experiment re-runs the same retro-character conversion on a much finer grid. The project
`content/projects/retro-character-high-fidelity.project.json` keeps the source, pivot anchor,
six-sample-per-second schedule, and clip set identical to the first experiment and changes only the
spatial resolution: a 96 × 144 × 96 grid with 0.03125-unit cells (4× linear, 64× volumetric). The
experiment:

- sampled the same 16 source poses and stored 15 runtime frames (14 unique voxel meshes);
- produced 168,907 aggregate conversion voxels in a 12,758,243-byte canonical object — about 17.5×
  the baseline aggregate, with 10,484–10,508 voxels per sampled `idle` pose instead of ~610;
- loaded the object through the same strict runtime admission path (158,178 resolved voxels); and
- projected a selected `run` frame as a real shared-renderer voxel-object instance.

One adapter change was required: the per-pose output bound derived from the grid product
(96 × 144 × 96 = 1,327,104 cells) exceeded the engine's `MAX_REPRESENTED_VOXELS` cap, so the
project now clamps its `maxOutputVoxels` request to that engine limit. Conversion work
(1,093,918 against the 10,000,000 budget), runtime admission limits, and artifact size all remain
comfortably inside engine bounds.

Exact identities and playback samples are checked in
`evidence/high-fidelity-animated-voxel-report.json`. Rebuild it with:

```bash
cargo run --locked --bin voxel-lab -- verify \
  --project content/projects/retro-character-high-fidelity.project.json \
  --report evidence/high-fidelity-animated-voxel-report.json
```

The two reports now also contain named source/voxel pose comparisons, loop-seam and foot-anchor
readouts, palette stability, phase timings, retained CPU payload estimates, and a 512-swap Rust
projection measurement. At this checked revision the high-fidelity front-silhouette scores exceed
0.90 for all three clips, compared with 0.19–0.45 for the baseline, but its 12.8 MB canonical object
and 34.5 MB unique mesh payload take about 2.6 seconds to admit and mesh in this unoptimized local
run. See [`docs/quality-report.md`](docs/quality-report.md) for the measured tradeoff and explicit
limits.

## Density lab

The `voxel-density-lab` harness bakes complicated static meshes — whole or as
individually selected mesh pieces — through the Engine's static conversion
path and records where density actually tops out. The checked corpus is two
CC-BY Sketchfab knights (30k/38k triangles, 19–24× the retro character).

Measured: tens of thousands of voxels per character is comfortable (55k/129k
at 256 cells tall, 0.97 silhouette fidelity, seconds to convert and admit);
hundreds of thousands is only reachable piece-wise (205k aggregate across the
bulky knight's three armour/axe pieces), because the Engine caps conversion
grids at 256 cells per axis. The knights' native static meshes carry no rigs
or clips, so the animated experiments remain the retro character's domain.
Findings (absolute-epsilon triangle rejection, the 256-axis grid cap, and the
missing shared-scale multi-piece seam) feed upstream tasks. See
[`docs/density-lab.md`](docs/density-lab.md) and `evidence/density/`.

```bash
cargo run --locked --bin voxel-density-lab -- run \
  --spec content/density/dark-knight-ladder.spec.json \
  --report evidence/density/dark-knight-ladder.json
cargo test --locked --test density_experiment
```

## Voxel mesh data plane

`voxel-lab format-study` prices the checked corpus's unique flipbook meshes against candidate
payload encodings (expanded JSON, packed base64, binary reference, mesh-delta) plus parse/decode
timings, producing checked evidence for the upstream voxel data-plane decision (rusty-engine #6331).
See `docs/design.md` for findings and `evidence/format-study-{baseline,high-fidelity}.json` for the
numbers.

The resulting implementation keeps canonical voxel-object JSON unchanged and moves derived mesh
streams into deterministic `packedStreamsLeV1` resources. This adapter atomically publishes those
resources into the ignored `.studio-cache` and sends compact content-addressed manifests through
Studio protocol 14. The checked high-fidelity open fell from 54,564,714 bytes of projection JSON to
a roughly 24.7 KiB control response plus 11,712,856 raw resource bytes with the current greedy
mesher. The original before/after and Node/Chromium parse observations remain in
`evidence/mesh-data-plane.json` with their exact historical Engine revision.

```bash
cargo run --locked --bin voxel-lab -- format-study \
  --project content/projects/retro-character-high-fidelity.project.json \
  --report evidence/format-study-high-fidelity.json
```

## Textured voxel evidence

The checked `content/textures/directional-atlas.png` fixture has two asymmetric
6 x 6 regions with replicated one-texel padding. Its unequal corners, diagonal
and horizontal marks, and edge colors make rotation, mirroring, repetition,
and atlas bleed visible. The public Engine mesher produces
`evidence/textured-voxel-surfaces.json` from that exact PNG. The report records
stream hashes and byte counts for a 48 x 32 wall, a 48 x 1 x 32 floor, all six
faces of a negative-coordinate cell, mixed material groups, and two resident
adjacent chunks meshed independently across a continuous absolute tile seam.
Both large surfaces retain the untextured baseline's six quads, 24 vertices,
and 36 indices.

```bash
node scripts/generate-textured-voxel-fixture.mjs --check
cargo run --locked --bin textured-voxel-evidence -- --check
```

Protocol-14 adapter tests use the same checked asset for repeat and two-region
atlas publication, replacement, animated-frame continuity, and fresh-adapter
reopen. They also reject malformed, missing, oversized, stale, overlapping,
out-of-bounds, insufficiently padded, and failed-reimport inputs before project
publication. This repository's dispatch-only `studio-browser` job accepts one
exact reverse-provider revision that pins this consumer and runs its real
Chromium integration. That consumer-owned path records deterministic visible
repeat/atlas/reopen pixels, retained resource counts, replacement lifecycle,
and final cleanup through the Engine renderer without duplicating the renderer
or atlas parser here.

## Commands

```bash
# Check the canonical Engine source against all Rust and lockfile projections.
./scripts/engine-revision check

# Prove an exact public revision and preview the bounded carrier diff.
./scripts/engine-revision update <40-character-public-sha> --dry-run

# Prepare the same validated revision update in the working tree.
./scripts/engine-revision update <40-character-public-sha>

# Rebuild the canonical object and update its content-addressed project reference.
cargo run --locked --bin voxel-lab -- convert

# Strictly load, admit, play, and project the checked object.
cargo run --locked --bin voxel-lab -- load

# Run all deterministic and integration checks.
./scripts/verify.sh

# Validate this adapter against the pinned Studio protocol decoder.
./scripts/verify-studio.sh

# After an Engine reverse-pin descendant exists, run the consumer-owned real
# Chromium certification against that exact provider revision.
./scripts/verify-studio-browser.sh <40-character-reverse-provider-commit>

# Launch the pinned Rusty Engine Studio with this project's adapter.
./scripts/studio.sh

# Bind the same Studio project for trusted-LAN use.
./scripts/studio.sh --host 0.0.0.0 --port 4310

# Or let den-serve own the process and LAN address.
den-serve up rusty-engine-voxels -repo /home/dev/rusty-engine-voxels
```

`engine-source.json` is the only authored Engine identity. The update command projects it into the
twelve direct Rust dependencies and the generated Cargo lockfile without rewriting historical
evidence, protocol compatibility, or prose. See
[`docs/engine-revision-updates.md`](docs/engine-revision-updates.md) for the failure, dry-run, and
rollback contract.

`studio.sh` checks out the exact provider revision from `engine-source.json` into the ignored
`.studio-cache`, installs its locked Studio workspace, builds this repository's Rust adapter, and
prints an auto-open URL for the checked voxel-lab project. The project can therefore use Studio
without an operational sibling checkout or a copy of Studio source.

The Studio project file is
`content/projects/voxel-lab.project.json`. The adapter owns this schema and supports opening,
reading, inspecting sources, preparing/previewing/applying/discarding voxel-object conversions,
attaching transformed voxel-object instances, authoring repeat or atlas-backed PNG voxel surfaces,
and transiently playing reopened applied instances. Runtime textures are deliberately separate from
the mesh image used by conversion sampling. Other protocol-14 operations fail with a typed
unsupported-operation rejection rather than a generic command tunnel.

Applied playback retains the admitted object and renderer projector for the open project. After the
initial complete frame, ordinary animation samples carry one `setVoxelObjectFrame` operation; Studio
waits for that pose to reach the shared renderer and then displays it for its authored duration before
requesting the next virtual-time sample. Slow authoring hosts therefore reduce playback speed without
skipping stored poses, and `once` visibly settles on its final frame.

The baseline project's saved pose is the object's static default frame. Clip selection is transient,
and its downstream collision choice stays pinned to that default frame while visible poses change.
Missing or corrupt object files fail project open rather than producing an empty renderer resource.

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
