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
cadence, and visual presentation. The project adapter implements Studio protocol 12 only as an
explicit host boundary; protocol 12 is neither the voxel project schema nor an industry voxel
standard. The retained single-placement operation remains an explicit one-instance upsert.
Protocol 12 additionally admits 1–32 ordered placements as one create-only transaction: it rejects
duplicate or existing identities, stale project hashes, invalid later entries, exhausted JSON-safe
owner IDs, oversized project/readout encodings, and unsupported material overrides before replacing
the project document once. Owner IDs and the receipt preserve request order even though canonical
instances are sorted by authored identity. Complete runtime admission and renderer projection are
staged before publication, and a fresh adapter process reconstructs the same accepted owners.

The adapter owns one transient `VoxelObjectPlayer` session, clears it on open/reread/mutation/close,
and never serializes its posture into this project's durable instance frame.
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

The baked-character experiment currently owns its downstream-first canonical
kit, pose selection, and deterministic joint-fusion proof. The fusion owner is
documented in [`joint-fusion.md`](joint-fusion.md). It remains authoring-side
evidence; no runtime rig, scheduler, or second Engine voxel authority is
introduced.

The finished fused schedule crosses into the existing canonical Engine
voxel-object/runtime/projection path through the bounded compiler documented in
[`baked-flipbook-runtime.md`](baked-flipbook-runtime.md). Named anchors and
coarse collision remain immutable per-frame facts; games own their meaning and
whether to apply collision. The experiment does not retain the authoring rig at
runtime.

Art cleanup remains downstream authoring intent. The closed operations and
base-hash-bound diff replay in [`cleanup-loop.md`](cleanup-loop.md) may change
only declared regions and must pass the kit-derived protection, palette,
anchor, quota, dimension, and selected connectivity gates. Agent views and
metrics are observations; only an explicitly accepted ordered diff changes the
frame later compiled into Engine bytes.

Finished-frame temporal admission remains downstream as well. The metrics,
identity inventory, anchor trajectories, typed drift warnings, and deterministic
flicker-review projections in
[`temporal-consistency.md`](temporal-consistency.md) observe the fused schedule
without introducing runtime animation authority. Occupied-coordinate churn
uses the same headline measure as `src/churn.rs`; canonical source identities
and M3's validated discarded-overlap ledger distinguish visibility and rigid
motion from identity instability.

## Voxel mesh data plane

The `format-study` harness (`src/format_study.rs`, `voxel-lab format-study`) measures the checked
corpus against candidate mesh-payload encodings so the upstream voxel data-plane decision
(rusty-engine #6331) starts from corpus evidence rather than preference. Those measurements selected
Engine's `packedStreamsLeV1` presentation resources. Canonical schema-1 voxel-object JSON remains
unchanged; the resource cache is disposable and regenerates from the admitted object. Checked
`evidence/format-study-*.json` files retain their original Engine revision as historical decision
evidence. The live gate recomputes the study against the current provider pin instead of treating
those older measurements as current certification.

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
  plus base64's 4/3 overhead. With the current greedy mesher, baseline packed base64 is a wash (1.03×
  expanded JSON) while high fidelity is ~0.85×. Full binary remains about 0.64–0.77×. The leverage of
  packing is still primarily parse cost and payload structure, with a useful high-fidelity byte win.
- **Frame-to-frame vertex data barely overlaps.** Across the 14 unique high-fidelity meshes, about 74%
  of vertex attribute values change between poses and no mesh is fully shared with the base. Whole-mesh
  delta encoding remains net-negative under text accounting and saves about 34% under binary
  accounting. Vertex deduplication across frames remains a poor fit for this corpus.
- **Greedy meshing makes topology pose-dependent.** About 24% of high-fidelity index values now change
  between poses because independently merged rectangles differ with each silhouette. The earlier
  pre-greedy finding that index topology was nearly static remains useful provenance, but it is not a
  current encoding assumption.
- **The real cliff is total transferred volume per define/open,** not per-value cost — the 54.5 MB
  `openProject` response that trips Studio's 32 MiB cap (#6329) is the same data regardless of text vs.
  packed encoding. The durable fix is architectural (resource-referenced or chunked mesh payloads), with
  encoding as a secondary multiplier.

On the high-fidelity corpus, the former complete projection was 54,564,714 JSON bytes. At the
current provider pin, greedy meshing plus resource publication returns a roughly 24.7 KiB complete
Studio response and 11,712,856 raw resource bytes; steady-state playback remains about 1.2 KiB. The
older exact response, resource, Node, and Chromium measurements remain in
`evidence/mesh-data-plane.json` under their recorded Engine revision; timings are observations rather
than thresholds.
