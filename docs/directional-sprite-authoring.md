# Directional sprite authoring (experimental)

`voxel-kit-lab sprite-inspect` is a bounded review aid for inspecting a
directional sprite sheet. The directional lab also has an explicit, downstream
pixel-column voxelizer described below. Neither path is an Engine API or a
production image-to-voxel converter: the layout is the authority, every
direction/frame names a source rectangle or is explicitly `null`, and depth is
authored rather than inferred.

## Layout document

The document is a project-relative JSON file with `schemaVersion: 1`:

```json
{
  "schemaVersion": 1,
  "id": "enemy-reference",
  "source": {
    "path": "local/sprites/enemy.png",
    "background": {
      "colorKey": [0, 255, 255, 255],
      "colorKeys": [[149, 177, 200, 255]]
    }
  },
  "directions": ["front", "right", "back", "left"],
  "actions": [{
    "id": "idle",
    "name": "Idle",
    "frames": [{
      "id": "idle-0",
      "name": "Idle 0",
      "views": [
        { "direction": "front", "rect": { "x": 10, "y": 20, "width": 40, "height": 64 }, "anchor": { "x": 20, "y": 62 } },
        { "direction": "right", "rect": null },
        { "direction": "back", "rect": { "x": 120, "y": 20, "width": 40, "height": 64 } },
        { "direction": "left", "rect": { "x": 180, "y": 20, "width": 40, "height": 64 } }
      ]
    }]
  }]
}
```

`colorKey` and `colorKeys` are exact RGBA values only. They are useful for
pixel-art sheets with a cyan cell fill and a different page background; no
tolerance, palette inference, recoloring, mirroring, or blur is performed.
The uncertain source sheet, layout, and generated comparison output live below
ignored `local/`; the authored voxel inputs are provenance-safe files under
`content/`.

## Inspect and compare

From the repository root:

```bash
cargo run --locked --bin voxel-kit-lab -- sprite-inspect \
  --spec local/directional-sprite-test/zombieman.layout.json \
  --out local/directional-sprite-test/inspection \
  --action idle
```

The command validates source dimensions, path boundaries, duplicate/missing
direction labels, empty/out-of-bounds/overlapping rectangles, cell count, and
output size before publishing. It emits:

- `layout.normalized.json` with source dimensions/hash, covered/unused area,
  normalized direction ordering, and explicit missing-view diagnostics;
- nearest-neighbor RGBA PNG crops under `crops/`;
- `contact-sheet.svg`, a human/agent-readable grid with shared card scale,
  direction/action/frame labels, source coordinates, anchor and ground guides,
  and an explicit `MISSING` card when a view is absent.

An authored voxel render can be placed beside each crop without changing the
layout parser:

```bash
cargo run --locked --bin voxel-kit-lab -- sprite-inspect \
  --spec local/sprites/enemy.layout.json \
  --out local/sprites/review-with-voxel \
  --action attack --frame attack-0 \
  --comparison local/voxel-renders/attack-0.svg
```

The comparison input is presentation-only. It never becomes a canonical
voxel asset or hidden-depth authority.

## Dense pixel-column voxelization

The checked directional experiment uses the local `zombieman` sheet only as an
uncertain visual reference. Its authored voxelization spec is
`content/characters/directional-sentinel/voxelization.json`; the source PNG and
layout remain under ignored `local/` and are identified by an exact source
hash. Regeneration therefore requires that local source to be present. This is
deliberately not a production provenance claim.

Run the explicit downstream compiler with:

```bash
cargo run --locked --bin voxel-kit-lab -- directional-voxelize \
  --spec content/characters/directional-sentinel/voxelization.json \
  --out content/voxel-objects \
  --report evidence/directional-sprite-voxelization.json
```

Each opaque sprite pixel becomes a column of 24 voxels. The experiment uses a
0.01 m cell and the `[64, 64, 24]` project carrier resolution, producing
12,072–24,528 voxels per animation frame across the four explicit cardinal
views and two source frames. The four view mappings are explicit; no view is
mirrored, synthesized, or assigned hidden depth by the compiler. The small
palette rule is also intentionally local and inspectable, not a segmentation
algorithm.

This gives the model a pixel-art-scale silhouette while keeping the result a
bounded 2.5D slab experiment. The overhead Studio view is the acceptance check
for whether the fixed depth reads well; a future relief/depth-mask pass must be
authored locally if the slab is visually insufficient.

To open the bounded authored experiment in the human Studio UI, pass the
project explicitly. The launcher keeps the normal host/adapter path and does
not alter project authority:

```bash
./scripts/studio.sh \\
  --project content/projects/directional-sprite-experiment.project.json
```

The reviewable browser certification uses the exact pinned provider and the
actual shared renderer. Set `RUSTY_STUDIO_PROVIDER_ROOT` to the checkout
resolved by `scripts/studio-provider.sh`, then run:

```bash
export RUSTY_STUDIO_PROJECT_ROOT="$PWD"
export RUSTY_STUDIO_PROJECT_FILE=content/projects/directional-sprite-experiment.project.json
export RUSTY_STUDIO_CAPTURE_ROOT="$PWD/evidence/directional-studio"
export RUSTY_STUDIO_PROVIDER_COMMIT=c02754812d53df5363c9e6475c685c54e532f5e5
export RUSTY_STUDIO_ENGINE_COMMIT=c02754812d53df5363c9e6475c685c54e532f5e5
export RUSTY_STUDIO_ADAPTER_BINARY="$PWD/target/debug/rusty-engine-voxels-studio-adapter"
export RUSTY_STUDIO_SETTINGS_ROOT="$(mktemp -d /tmp/rusty-studio-settings.XXXXXX)"
pnpm --dir "$RUSTY_STUDIO_PROVIDER_ROOT/studio" exec playwright test \
  --config "$PWD/scripts/directional-studio-playwright.config.mjs"
```

It writes 16 canvas PNGs and a manifest under `evidence/directional-studio/`:
all eight explicit frames from the initial perspective camera and after a
primary-button orbit toward overhead. The manifest pairs each PNG and
normalized RGBA hash with the canonical `voxelDataHash`, renderer frame hash,
source direction/frame, voxel count, and object hash. The frame 7 → frame 0
return is checked by explicit playback state and retained as a separate
capture; raster bytes are evidence rather than a cross-machine golden because
browser compositing can vary.

The local experiment uses the repository's licensed Kenney GLB as a renderer
carrier because the current Studio render-resource boundary accepts GLB/PNG/
RMESH source paths; the sprite sheet and the authored voxel poses remain the
actual experiment inputs. The carrier is not used to generate the voxel
frames, and uncertain sheets under `local/` remain non-production.

## Agent recipe

1. Copy an uncertain test sheet into ignored `local/`; do not check it in or
   make a license/provenance claim.
2. Inspect the sheet visually and write rectangles, direction order, actions,
   frame groups, and any grounding anchors by hand. Include unused regions by
   omission; do not force the credit text into a cell.
3. Run `sprite-inspect` and review the contact sheet. Fix layout diagnostics,
   then compare every direction against the same authored voxel render scale.
4. For the dense experiment, run `directional-voxelize` after reviewing the
   layout. Decide depth, palette cleanup, pivot, cell size, and missing-view
   policy in the explicit spec; do not generalize this policy into Engine.
5. Keep generated crops, SVGs, renders, and scratch source material under
   `local/`. The checked directional object and project pointer are the
   reviewable runtime artifact, while its uncertain source remains local-only
   and must not be described as production-ready.

Focused verification is sufficient for this tooling phase:
`cargo test --lib directional`, `cargo check --locked --bin voxel-kit-lab`,
`cargo test --locked --test directional_sprite_layout`, the adapter flipbook
smoke, and a real Studio browser open with the project above. Full Studio CI is
not required unless a shared Studio boundary changes.
