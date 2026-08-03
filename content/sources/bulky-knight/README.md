# Bulky Knight source

Packed geometry-only GLB derived from the Sketchfab download in
`/home/stash/mesh-resources/characters/bulky-knight/` (CC-BY-4.0, see the
adjacent `LICENSE.txt`). Credit: "Bulky Knight"
(https://sketchfab.com/3d-models/bulky-knight-002a90cbf12941b792f9685546a7502c)
by Arthur Krut (https://sketchfab.com/OptiCube).

Packing (`scripts/pack-glb.py --exclude-nodes '^pasted__'`):

- strips images/textures/samplers — the voxel converter maps material slots to
  a flat palette and never decodes images;
- drops the `pasted__*` duplicate subtrees (a second armour and axe variant
  overlapping the kept `Armour_LP` + `Axe_LP` geometry);
- embeds `scene.bin` as the GLB BIN chunk.

Kept geometry: 8 nodes, 3 mesh primitives (`Armour_LP` in two material
primitives plus `Axe_LP`), ~30,100 triangles. World-space span after node
transforms: 0.0494 × 0.0724 × 0.0371 source units (Y-up).
