# Mesh→Kit Bake (voxel-kit-lab)

`voxel-kit-lab bake` authors a canonical exploded voxel kit (`docs/kit-format.md`)
from a real mesh — the step that frees kit authoring from hand-placed cells.
The first product is the **knight kit**: 11 parts, 167,962 voxels, ~133× the
hand-authored rifleman's cell count, built from a complicated Sketchfab
character (22.8k vertices, 8 named mesh pieces, CC-BY).

## How it works

```text
kit-spec.json (parts = node slices + voxel-space regions + pivots/sockets)
        │
        ▼  Engine static conversion, one bake per source node
        │  (import_mesh_source + plan_static_voxel_object_conversion,
        │   meshPrimitive node/N, each at its cap-limited max rate)
        ▼  Downstream re-registration into one shared kit lattice
        │  (exact inverse of the bake's Contain/Centered mapping,
        │   volume-argmax per axis, all scales ≥ 1 → upsampling only)
        ▼  Part composition in voxel space
        │  (region predicates split armor → torso/arms/legs, pants → legs;
        │   earlier parts win contested cells; per-slice palette overrides)
        ▼  Kit JSON: parts + pivots + mated sockets + palette + invariants
        ▼  validate → assemble_neutral → fingerprint + evidence
```

Ownership: the Engine owns mesh import and triangle voxelization; this
repository owns kit composition (regions, re-raster, pivots, sockets,
palette, invariants). The re-raster is deterministic and volume-exact per
axis; separately baked pieces register to sub-cell accuracy (ceil effects
≤0.5% and the sword's cap-limited rate are absorbed by the upsampling
re-raster). rusty-engine #6590 would let the Engine do this natively with a
shared envelope — until then the downstream re-raster is the supported path.

## The knight kit (checked: `content/characters/knight/`)

Source: "Knight" by danielgobr481 (CC-BY-4.0), checked as
`content/sources/knight/knight.glb` with license adjacent.

Bakes (all within Engine caps — max 972k of 10M work, ≤256 cells/axis):

| Node | Resolution | Cells/unit | Voxels |
|---|---|---|---|
| Armor | 131×256×83 | 1.4925 | 96,410 |
| Pants | 83×79×41 | 1.4922 | 20,419 |
| Helmet | 34×55×41 | 1.5114 | 8,891 |
| Cloth | 86×64×57 | 1.4908 | 21,323 |
| L.hand | 16×22×26 | 1.5112 | 2,137 |
| R.hand | 22×27×19 | 1.4970 | 2,213 |
| Sword | 17×256×55 | 1.3190 | 17,261 |
| Pillum | 128×13×82 | 1.4906 | 2,167 |

Kit lattice: 1.5114 cells/unit (the maximum achieved rate), character 292
cells tall at 6.285 mm cells (1.85 m). Parts (armor split into
torso/arms/legs by region; pants folded into legs; weapons mated to hands):

| Part | Cells | Notes |
|---|---|---|
| torso | 43,731 | armor y≥-45 minus arm regions |
| left_arm / right_arm | 14,827 / 13,917 | armor y≥10, \|x\|≥20 |
| left_leg / right_leg | 22,034 / 22,217 | armor y<-45 + pants half |
| helmet | 8,424 | mated at neck |
| cloth | 19,533 | tunic, mated at waist |
| left_hand / right_hand | 2,065 / 2,078 | mated at wrists |
| sword | 17,125 | tip clipped at ground plane |
| pillum | 2,011 | mated at right hand |

Contested cells resolve by part order (earlier wins): torso wins 5,802
self-overlapping slice duplicates, cloth loses 1,790 cells to armor in the
armpit region, helmet loses 467 collar cells to the armor neck. Every baked
cell is assigned (0 unassigned). Assembly fingerprint pinned in
`tests/kit_bake_experiment.rs`; full readout in
`evidence/kit-bake-knight.json`.

Regenerate:

```bash
cargo run --locked --bin voxel-kit-lab -- bake \
  --spec content/characters/knight/kit-spec.json \
  --out content/characters/knight/character.json \
  --report evidence/kit-bake-knight.json
```

## Spec format

`kit-spec.json` schema v1: one static `mesh/...` source (SHA-256 + license
pinned), `characterHeightMeters` + `groundYSource` (convention), a
`targetCellsPerUnit` bake rate (each node clamps to its 256-axis cap), the
palette with per-slice overrides, parts with `pivotWorld` + slices
(`node` + optional axis-aligned `region` in source space), and socket pairs
(`parts: [parent, child]` + `world` + `forward` + `radiusSource` — the
parent side becomes a free attachment point, the child declares the mate, so
assembly has exactly one root).

The manual-pivot experiment that poses this kit by hand-authored rotations
lives in `tests/kit_pivot_experiment.rs` (evidence:
`evidence/kit-pivot-knight.json`) and is written up in
[`docs/kit-pivoting.md`](kit-pivoting.md).
