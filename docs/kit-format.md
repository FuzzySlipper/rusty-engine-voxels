# Canonical Exploded Voxel Kit (M1)

The canonical exploded kit is the **source of truth for a character's identity** in the baked
voxel animation pipeline. It is authored once as rigid voxel parts plus a palette and identity
invariants, then assembled into a neutral pose deterministically. Later milestones pose these
stable parts (M2), fuse their joints (M3), and compile frames into the engine's existing voxel
flipbook (M4). This document covers only the M1 format and assembly; see
`baked-voxel-animation-design.md` for the full pipeline.

## Ownership boundary

This module (`src/kit.rs`) owns *authoring intent*: the kit/part formats, validation, deterministic
neutral assembly, and provenance. It deliberately does **not** reproduce engine conversion/runtime
semantics. The assembled neutral frame is a plain coordinate→material map (with provenance) that
later milestones feed into the engine-owned voxel-object format. Nothing here is a new runtime
format or a rig.

## Format

A kit is a single canonical JSON document (schema version 1):

- **`convention`** — coordinate/scale declaration, enforced across all parts: right-handed Y-up,
  forward `-Z`, `voxelSizeMeters`, `groundY`, `neutralFacing`. One convention per character;
  changing voxel scale requires a new kit version.
- **`palette`** — named groups of material slots (`slot`, `displayName`, `color`). Slot ids are
  unique across the whole kit; slot `0` is reserved.
- **`parts[]`** — the rigid pieces. Each part has:
  - `id`, `version`, `pivot` (part-local origin of its primary parent joint),
  - `cells[]` — occupied integer cells (`coordinate`, `materialSlot`), **stored sorted and
    deduplicated** so the cell index is a stable provenance key,
  - `sockets[]` — named attachment points (`position`, `forward`, `radius`, optional `mate` as
    `<partId>.<socketId>`),
  - `paletteGroups`, optional `symmetryPartner`.
- **`invariants`** — identity rules every downstream frame must respect: `minLimbThickness`,
  `protectedParts`, optional `volumeRange`, `requiredSockets`.

Validation (`VoxelKit::validate`) enforces schema version, identity text, sorted/deduped cells,
reserved slot `0`, palette/slot uniqueness, and resolves *all* cross-references — palette groups,
symmetry partners, cell material slots, socket mates, and required sockets — with actionable
error messages naming the offending part/socket.

## Provenance

Every assembled voxel carries a `VoxelOrigin`. In M1 the only variant is
`Canonical(CanonicalVoxelId { part_index, voxel_index })`, where `voxel_index` is the cell's index
in the part's sorted cell list. Later milestones add `JointBridge` and `CleanupGenerated` variants
to the same closed enum, so the compiler can always distinguish intentional character structure
from frame-specific filler.

## Neutral assembly

`assemble_neutral(kit)` deterministically builds the neutral character:

1. **Placement order** — parts are ordered so each part appears after the part it mates to
   (mate-dependency walk; roots with no mated socket first). A mate cycle or a kit with no root is
   an assembly error.
2. **Translation** — a mated part is translated so its mating socket coincides with its mate's
   world socket position. Root parts translate so their pivot sits at the origin/ground reference.
   (M1 is translation-only; orientation during posing is a later milestone.)
3. **Emission** — cells are emitted with earlier-part-wins overlap resolution and per-voxel
   provenance.
4. **Grounding** — the whole assembled frame is shifted so its lowest occupied cell rests on
   `groundY`. Grounding is a property of the finished assembly, not any single part, because limbs
   and equipment may hang below the root part.

The result is byte/content-stable: re-assembling the same kit always produces an identical frame.
`AssembledFrame::fingerprint()` gives a deterministic content hash used to pin regeneration in
tests — an intentional character revision must update the pinned fingerprint and bump the affected
part versions.

## Checked corpus

`content/characters/rifleman/character.json` — a chunky 13-part humanoid (torso, head, pelvis,
upper/lower arms and legs on both sides, rifle, backpack) with a 7-slot palette, ~1,260 canonical
cells, grounding to `y = 0` and stacking correctly (head above torso above pelvis). The
integration test in `tests/kit_experiment.rs` validates it, assembles it twice for determinism,
checks grounding/centroid ordering/provenance, and pins the neutral fingerprint.
