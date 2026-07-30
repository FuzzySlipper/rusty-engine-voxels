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
   selected instance. Large mesh streams are packed into deterministic content-addressed resources;
   this adapter publishes them atomically under the ignored `.studio-cache/render-resources`
   directory and returns only their bounded manifest beside the control frame. Studio supplies the
   Three renderer and authoring UI.
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

`engine-source.json` is the sole authored Rusty Engine identity. The six direct Rust dependencies,
their generated Cargo lock entries, runtime evidence readout, and managed Studio checkout all
derive from that exact public commit. `scripts/engine-revision check` is the common strict validator;
`scripts/engine-revision update <sha>` prepares a bounded projection change through a disposable
worktree. It does not infer or rewrite Studio protocol compatibility, historical evidence, or prose.
Those remain intentional work followed by both `scripts/verify.sh` and `scripts/verify-studio.sh`.

The managed Studio launcher clones that revision into `.studio-cache`. It never inspects a sibling
`rusty-engine` checkout, and the ordinary Rust gate has no Node or browser dependency.

The complete operator and rollback contract is in
[`engine-revision-updates.md`](engine-revision-updates.md).

## Future experiments

New experiments should normally add a licensed source corpus, a named project/configuration, a
content-addressed result, focused assertions, and measured evidence. Useful next comparisons
include resolution and surface/fill quality, temporal stability, anchor policy, palette recovery,
clip sampling rates, compression/deduplication behavior, collision representation, and runtime
animation ergonomics.

Promote a mechanism upstream only when the experiment demonstrates that it is reusable Engine
behavior. Keep subjective art-direction defaults and corpus-specific fixes here unless several
concrete consumers prove otherwise.

## Voxel mesh data plane

The `format-study` harness (`src/format_study.rs`, `voxel-lab format-study`) measures the checked
corpus against candidate mesh-payload encodings so the upstream voxel data-plane decision (rusty-engine
#6331) starts from corpus evidence rather than preference. Those measurements selected Engine's
`packedStreamsLeV1` presentation resources. Canonical schema-1 voxel-object JSON remains unchanged;
the resource cache is disposable and regenerates from the admitted object.

Open/read, conversion candidate, discard, and applied-instance playback responses carry the
manifest for the exact projection they return. The adapter retains the canonical manifest across
ordinary `setVoxelObjectFrame` patches. It never places paths in the renderer-neutral frame, and it
does not base64-encode bulk bytes into Studio's JSON control channel.

For each project's unique flipbook meshes it reports four shapes — the current expanded JSON number
arrays, a packed base64 typed-array envelope, a binary lower-bound reference, and a mesh-delta
(base-plus-difference) accounting — plus browser-relevant parse/decode timings. Evidence is checked
in `evidence/format-study-{baseline,high-fidelity}.json`.

Findings on the checked corpus, with the harness's stated interpretation limits:

- **Packed typed-array payloads are not a blanket byte win.** Positions/normals here are cell-quantized,
  so many coordinates are small integers that serialize to 1–3 JSON bytes — cheaper than 4 packed bytes
  plus base64's 4/3 overhead. At the baseline grid packed base64 is a wash (1.01× expanded JSON); at the
  high-fidelity grid it is only ~0.84×. Full binary is a consistent ~0.63–0.76× lower bound. The leverage
  of packing is primarily parse cost and payload structure, not raw bytes.
- **Frame-to-frame vertex data barely overlaps.** Across the 14 unique meshes, ~64–67% of vertex
  attribute values change between poses and no mesh is fully shared with the base. Whole-mesh delta
  encoding does not pay for itself under text accounting (net-negative) and saves only ~43–45% under
  binary accounting. Vertex deduplication across frames is a dead end for this corpus.
- **Index topology is nearly static** (~0.7–3.6% changed). If a delta scheme is pursued, it should key on
  shared index streams, not shared vertex data.
- **The real cliff is total transferred volume per define/open,** not per-value cost — the 54.5 MB
  `openProject` response that trips Studio's 32 MiB cap (#6329) is the same data regardless of text vs.
  packed encoding. The durable fix is architectural (resource-referenced or chunked mesh payloads), with
  encoding as a secondary multiplier.

On the high-fidelity corpus, the former complete projection was 54,564,714 JSON bytes. The checked
resource implementation returns a 24,805-byte complete Studio response plus 34,541,056 packed
bytes, and a steady-state playback response remains 1,213 bytes. One local observation measured
the compact response at 0.207 ms in Node `JSON.parse` and 0.4 ms in Chromium, versus the earlier
2,028 ms-per-pass host-neutral expanded-JSON proxy. See
`evidence/mesh-data-plane.json`; timings are observations rather than thresholds.
