# Baked flipbook runtime

Milestone M4 closes the authoring-to-runtime seam for the exploded-kit
character experiment. The downstream compiler in `src/flipbook.rs` consumes a
complete M3 fused schedule and emits one canonical Rusty Engine schema-1 voxel
object. The object contains complete immutable frames, exact authored
durations, named local anchors, and coarse local collision facts. It introduces
no runtime rig, skinning model, scheduler, or alternate animation format.

## Authority path

1. M2 selects a bounded pose schedule and M3 fuses every pose into one complete
   voxel frame.
2. `compile_flipbook` reruns socket-constrained part placement for each selected
   pose and resolves named facts from an explicit part pivot, socket, or source
   voxel.
3. Rusty Engine `voxel-asset` validates and content-hashes the complete object.
   Engine's exact float-roundtrip JSON contract preserves arbitrary authored
   transforms across encode and strict decode.
4. `publish_compiled_flipbook` writes immutable canonical bytes under the
   object's content hash. Repeating identical work is a no-op; an occupied hash
   path with different bytes rejects without replacement.
5. `voxel-object-runtime` admits the stored bytes and owns explicit-time
   once/repeat/pause/resume behavior. `render-projection` defines one shared
   character-type resource and ordinary pose changes emit only
   `setVoxelObjectFrame`.

Anchors and collision primitives use the canonical object's local voxel-cell
coordinate system. Schema 1 is right-handed Y-up. Capsule `halfHeight` excludes
the caps: its cylindrical endpoints are `center.y +/- halfHeight` and its full
Y extent is `2 * (halfHeight + radius)`.

## Checked rifleman result

`tests/flipbook_experiment.rs` builds the real rifleman `run` schedule from the
checked Kenney source and proves compilation, strict serialized admission,
content-addressed publication, playback, and renderer-neutral projection. The
checked result has:

- 20 stored clip frames and 21 runtime frames including the default;
- 20 unique meshes with 3,358,680 bytes of deterministic Rust mesh arrays;
- eight moving anchors and two hit regions on every clip frame;
- one shared renderer definition for two instances; and
- one 89-byte steady-state frame operation with no mesh resource payload.

The exact identities and counts are in
`evidence/baked-flipbook-runtime.json`. Payload counts exclude allocator
overhead and are structural evidence, not timing or performance thresholds.
The coarse collision metadata is intentionally not installed into a world
collision service by this experiment; downstream games remain responsible for
selecting and applying those facts.

