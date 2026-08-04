# Directional sprite authoring (experimental)

`voxel-kit-lab sprite-inspect` is a bounded review aid for manually turning a
directional sprite sheet into authored voxel poses. It is not an image-to-voxel
converter, a depth estimator, or a sprite runtime. The layout is the authority:
every direction/frame either names a source rectangle or is explicitly
`null`.

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

To open the bounded authored experiment in the human Studio UI, pass the
project explicitly. The launcher keeps the normal host/adapter path and does
not alter project authority:

```bash
./scripts/studio.sh \\
  --project content/projects/directional-sprite-experiment.project.json
```

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
4. Manually edit a small voxel kit/pose-spec or explicit flipbook. Decide
   depth, palette cleanup, symmetry, pivot, cell size, and missing-view policy
   in authored data; do not add a converter.
5. Keep generated crops, SVGs, renders, and scratch specs under `local/`.
   The canonical authored character, poses, voxel object, and project live in
   `content/characters/directional-sentinel/`, `content/voxel-objects/`, and
   `content/projects/directional-sprite-experiment.project.json`; uncertain
   sprite sources and derived comparisons remain local-only.

Focused verification is sufficient for this tooling phase:
`cargo test --lib directional`, `cargo check --locked --bin voxel-kit-lab`,
`cargo test --locked --test directional_sprite_layout`, the adapter flipbook
smoke, and a real Studio browser open with the project above. Full Studio CI is
not required unless a shared Studio boundary changes.
