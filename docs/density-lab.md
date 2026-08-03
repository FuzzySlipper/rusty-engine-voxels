# Static-mesh density lab

**Question:** how much voxel detail can the current pipeline bake for a
complicated character — tens of thousands, or hundreds of thousands of voxels?

**Harness:** `voxel-density-lab run --spec SPEC [--report REPORT]` drives the
Engine's *static* conversion path (`import_mesh_source` +
`plan_static_voxel_object_conversion`) over a declarative spec of bakes —
whole models at a ladder of grid resolutions, or individually selected mesh
pieces (`meshPrimitive: "node/N"`). Each bake publishes a content-addressed
canonical object (into the ignored `.density-cache/objects/`), admits it
through `voxel-object-runtime`, projects it through `render-projection`, and
records either full metrics or a structured failure (stage + Engine
diagnostic). `src/density.rs`; integration coverage in
`tests/density_experiment.rs`.

**Corpus:** two CC-BY Sketchfab knights, checked as geometry-only GLBs with
licenses adjacent (`content/sources/bulky-knight/`,
`content/sources/dark-knight/`). `scripts/pack-glb.py` strips textures (the
converter maps material slots to a flat palette and never decodes images),
drops Sketchfab `pasted__*` duplicate subtrees, and can uniformly rescale
POSITION data (see finding 1). The checked knights are 30,095 triangles
(bulky, after dedup) and 37,734 triangles (dark) — 19–24× the retro
character's 1,604.

## Results (Engine rev c027548)

Whole-model ladder, surface mode, aspect-matched grids, constant ~1.7-unit
character world height (debug build, timings are evidence not thresholds):

| Source | Height cells | Resolution | Voxels | Work | Artifact | Convert | Admit | Mesh payload | Silhouette |
|---|---|---|---|---|---|---|---|---|---|
| bulky | 128 | 38×128×34 | 11,295 | 191,827 | 0.69 MB | 1.1 s | 0.2 s | 0.7 MB | 0.75 |
| bulky | 256 | 76×256×67 | **55,453** | 676,455 | 3.10 MB | 4.0 s | 1.0 s | 3.7 MB | 0.97 |
| dark | 128 | 32×128×97 | 29,912 | 273,671 | 1.62 MB | 2.1 s | 0.5 s | 2.7 MB | 0.65 |
| dark | 256 | 64×256×193 | **128,982** | 914,938 | 7.39 MB | 7.5 s | 2.6 s | 12.0 MB | 0.97 |
| either | ≥384 | — | **fails at plan: `MAX_CONVERSION_RESOLUTION_AXIS = 256`** (finding 2) |

Per-piece bakes of the bulky knight (`meshPrimitive: node/4|5|7` → armour in
two material primitives + axe), each independently Contain-fit to its own
grid:

| Piece | Resolution | Voxels | Work | Silhouette |
|---|---|---|---|---|
| armour (lambert8) | 128×256×128 | 101,998 | 1,040,927 | 0.96 |
| armour (lambert9) | 128×256×128 | 67,403 | 441,478 | 0.97 |
| axe | 128×256×128 | 35,532 | 1,646,384 | 0.93 |
| **aggregate** | | **204,933** | | |
| axe (coarse) | 64×128×64 | 8,313 | 263,078 | **0.66** |

Evidence: `evidence/density/bulky-knight-ladder.json`,
`evidence/density/dark-knight-ladder.json`,
`evidence/density/bulky-knight-pieces.json`,
`evidence/density/bulky-knight-smoke.json` (checked, test-pinned).

## What the numbers say

- **Tens of thousands per character is comfortable today.** 55–129k voxels
  at 256 cells tall converts in seconds, admits in ~1–3 s, projects, and
  holds 0.97 silhouette fidelity. Work usage stays under 1M of the 10M
  per-frame budget; artifacts stay under 8 MB of the 64 MB cap.
- **Hundreds of thousands per character is only reachable piece-wise.** The
  three armour/axe pieces alone aggregate 205k voxels — each within caps —
  because each piece fills its own grid instead of inheriting the whole
  character's aspect ratio. That is exactly the exploded-kit architecture:
  bake parts dense, assemble into frames that may exceed 256 cells (frame
  coordinates are bounded to ±1,000,000; the 256 cap is on the *conversion
  grid*, not the format). The straight whole-mesh conversion cannot get
  there (finding 2).
- **Thin pieces starve at low resolution.** The axe reads at 256 cells
  (0.93) but collapses at 128 (0.66). Kit authoring needs per-part
  resolution budgets, matching the design's `minLimbThickness` concern.

## Findings fed back as tasks

1. **Import rejects small-but-valid triangles (rusty-engine).**
   `validate_triangles` rejects `area_squared <= f64::EPSILON` — an absolute
   threshold on *squared* area. The bulky knight at native scale (0.07 units
   tall) has a legitimate triangle with area ≈ 7.1e-9 (area² ≈ 2.0e-16 <
   2.2e-16) and its whole import fails with
   `conversion.invalidGeometry at source.triangles[219]`. Any model with
   small world units can hit this. Local workaround:
   `scripts/pack-glb.py --multiply-positions 128` (power-of-two, f32-exact).
   Fix belongs upstream (relative degeneracy test, or skip-with-diagnostic).

2. **Conversion grid cap blocks >~130k voxels/character (rusty-engine).**
   `MAX_CONVERSION_RESOLUTION_AXIS = 256` /
   `MAX_CONVERSION_CELLS = 16,777,216` rejects every rung above 256 cells
   tall at plan time. Both knights pin at ~55k/~129k voxels there. A
   decision is needed upstream: raise the cap, or support first-class tiled
   baking (one frame composed from sub-volume conversions). The 10M
   work/frame budget extrapolates to ~1M surface voxels, in line with
   `MAX_REPRESENTED_VOXELS`; caps rise together or not at all.

3. **No shared-scale multi-piece bake (rusty-engine).** Each static bake
   derives its own Contain fit from its own bounds, so separately baked
   pieces do not share a scale and cannot be reassembled into one character.
   The animated path already computes one source-space envelope across all
   frames; exposing explicit source bounds (or a multi-request plan) for
   static conversion would unlock bake-pieces-then-assemble without
   downstream transform surgery.

4. **Follow-ups here (rusty-engine-voxels).** A mesh→kit authoring tool
   (bake exploded-kit parts from mesh pieces, blocked on finding 3); a
   rigged+animated complex character for the full animated vertical slice
   (the checked knights are static meshes with no skins or clips).

## Reproduce

```bash
cargo run --locked --bin voxel-density-lab -- run \
  --spec content/density/bulky-knight-ladder.spec.json \
  --report evidence/density/bulky-knight-ladder.json
cargo run --locked --bin voxel-density-lab -- run \
  --spec content/density/dark-knight-ladder.spec.json \
  --report evidence/density/dark-knight-ladder.json
cargo run --locked --bin voxel-density-lab -- run \
  --spec content/density/bulky-knight-pieces.spec.json \
  --report evidence/density/bulky-knight-pieces.json
cargo test --locked --test density_experiment
```

Spec schema (`content/density/*.spec.json`): one static `mesh/...` source
with pinned SHA-256 and license path; a list of bakes with optional
`meshPrimitive` (`node/N` or `group/N`), grid `resolution`/`cellSize`/
`chunkSize`/`pivot`. Sources are repacked with:

```bash
python3 scripts/pack-glb.py --exclude-nodes '^pasted__' --multiply-positions 128 \
  <scene.gltf> <scene.bin> content/sources/bulky-knight/bulky-knight.glb
python3 scripts/pack-glb.py \
  /home/stash/mesh-resources/characters/dark-knight/scene.gltf \
  /home/stash/mesh-resources/characters/dark-knight/scene.bin \
  content/sources/dark-knight/dark-knight.glb
```
