# Voxel Lab design

Rusty Engine Voxels is a downstream experiment project, not another engine layer. It gives voxel
work a durable corpus, project schema, Studio adapter, and evidence surface while the reusable
mechanisms remain owned by Rusty Engine.

## Data flow

1. The project names a checked mesh or animated-mesh source, its exact SHA-256, conversion grid,
   material choices, clip sampling schedule, and target voxel-object identity.
2. Rusty Engine imports the source, samples animation poses at explicit times, deforms the mesh,
   voxelizes each sample, deduplicates stored meshes, and produces canonical voxel-object JSON.
3. This project publishes those bytes under a content-addressed file name, then atomically updates
   its small project document. Identical work is a no-op.
4. Rusty Engine's voxel-object runtime independently admits the serialized object and resolves a
   selected default or clip frame.
5. Rusty Engine's renderer-neutral projection defines the material and voxel object and creates the
   selected instance. Studio supplies the Three renderer and authoring UI.
6. Each applied instance is owned by one explicit project entity repeated in Studio hierarchy,
   entity inspection, object readout, and renderer metadata.
7. Studio may retain one disposable applied-instance player in this adapter. Closed commands carry
   explicit time; Rust selects the visible frame while the project's saved initial pose and both
   project/object bytes remain unchanged. The admitted runtime and retained projector are reused for
   the open project, so steady-state samples return only `setVoxelObjectFrame` rather than reloading
   the object and retransmitting every mesh.

The checked object is deliberately separate from both the project document and a transient Studio
conversion candidate. A failed or discarded candidate cannot mutate project authority. A newly
published immutable object is harmless until the project document refers to it.

## Ownership

This repository owns experiment intent: content paths, source identities, sampling choices,
materials, scene instances, transforms, selected frames, collision policy, and recorded results.
It may add new corpora and project-specific comparison tools freely.

Rusty Engine owns reusable mechanics: GLB import, animation evaluation, voxel conversion,
canonical voxel-object encoding, admission limits, playback, mesh construction, and
renderer-neutral diff production. Experiments should expose gaps in those owners rather than copy
their implementations locally.

Studio owns transient forms, filesystem selection, candidate preview, viewport input, sampling
cadence, and visual presentation. The project adapter implements Studio protocol 9 only as an
explicit host boundary; protocol 9 is not the voxel project schema and is not an industry voxel
standard. The adapter owns one transient `VoxelObjectPlayer` session, clears it on open/reread/
mutation/close, and never serializes its posture into this project's durable instance frame.
Studio advances that player one virtual frame only after the shared renderer accepts the previous
pose and its authored duration elapses. `once` settles paused on the terminal pose and Play restarts
from frame zero; repeat and ping-pong continue through the same acknowledgement-paced path.

Project schema 2 assigns every voxel-object instance a stable entity ID. The selected entity owns
the typed Voxel Object capability and its durable initial pose; Studio's entity-inspector controls
only the disposable player posture. This project-specific entity record proves the explicit owner
link without requiring a generic downstream component schema.

## Provider pin

Rust dependencies and `engine-source.json` resolve the same exact public Rusty Engine commit. The
former pins reusable Rust crates; the latter pins the separately isolated Studio/renderer
workspace. Updating the provider is an explicit two-pin change followed by both `scripts/verify.sh`
and `scripts/verify-studio.sh`.

The managed Studio launcher clones that revision into `.studio-cache`. It never inspects a sibling
`rusty-engine` checkout, and the ordinary Rust gate has no Node or browser dependency.

## Future experiments

New experiments should normally add a licensed source corpus, a named project/configuration, a
content-addressed result, focused assertions, and measured evidence. Useful next comparisons
include resolution and surface/fill quality, temporal stability, anchor policy, palette recovery,
clip sampling rates, compression/deduplication behavior, collision representation, and runtime
animation ergonomics.

Promote a mechanism upstream only when the experiment demonstrates that it is reusable Engine
behavior. Keep subjective art-direction defaults and corpus-specific fixes here unless several
concrete consumers prove otherwise.
