# Knight source

Original Sketchfab GLB (textures embedded, unused by the voxel converter —
it maps material slots to a flat palette and never decodes images). Identity
and license come from the GLB's own `asset.extras` metadata (CC-BY-4.0, see
the adjacent `LICENSE.txt`). Credit: "Knight"
(https://sketchfab.com/3d-models/knight-d62d60d5e4304d2cb08b2b1678ae4215) by
danielgobr481 (https://sketchfab.com/danielgobr481).

Geometry: 8 named mesh nodes (`Armor`, `Helmet`, `Sword`, `Pants`, `Cloth`,
`L.hand`, `Pillum`, `R.hand`), 22,841 vertices, no skins or clips. World-space
span after node transforms: 86.8 × 192.6 × 81.5 source units (Y-up, feet at
y ≈ -100.5, ground reference for the kit bake).

This is the source for the first mesh-derived exploded kit
(`content/characters/knight/`), baked by `voxel-kit-lab`
(`docs/kit-bake.md`).
