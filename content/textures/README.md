# Textured voxel fixtures

`directional-atlas.png` is a checked, self-authored 16 x 8 RGBA PNG generated
by `scripts/generate-textured-voxel-fixture.mjs`. It contains two asymmetric
6 x 6 content regions, each surrounded by one texel of replicated padding.
The unequal corner, diagonal, horizontal mark, and edge colors make rotation,
mirroring, atlas bleed, and lost repetition visible in framebuffer evidence.

Regenerate it with:

```bash
node scripts/generate-textured-voxel-fixture.mjs
```

The asset is repository-authored test content under the repository MIT license.
