# Dark Knight source

Packed geometry-only GLB derived from the Sketchfab download in
`/home/stash/mesh-resources/characters/dark-knight/` (CC-BY-4.0, see the
adjacent `LICENSE.txt`). Credit: "Fearsome Dark Knight Spiked Armor Massive
Sword"
(https://sketchfab.com/3d-models/fearsome-dark-knight-spiked-armor-massive-sword-0d959333dd5d4fe5a702ea111dbb5fe9)
by Pigcraft (https://sketchfab.com/s8819296).

Packing (`scripts/pack-glb.py`):

- strips images/textures/samplers — the voxel converter maps material slots to
  a flat palette and never decodes images;
- embeds `scene.bin` as the GLB BIN chunk.

Kept geometry: 6 nodes, 2 mesh primitives sharing one material, ~37,700
triangles. World-space span after node transforms: 0.2496 × 0.9977 × 0.7536
source units (Y-up; the wide Z span is the carried sword).
