# Handoff: posed flipbook in Studio (2026-08-03)

> Historical session record. Launcher, provider-pin, and cache commands below
> describe the 2026-08-03 environment and are not current operator guidance.
> Use `README.md` and `docs/adjacent-engine-dependency.md` for the current
> adjacent-facade and Engine-owned Studio workflow.

Notes for the next agent. What changed, why, and the environment traps.

## What this session delivered

The knight kit's manually-pivoted poses are now a real, Studio-viewable
flipbook animation — harsh pose-to-pose jumps, no tweening (Engine voxel-object
playback is explicit-time complete-frame switching; there is no interpolation
anywhere in the runtime, so the flipbook feel is free).

New pipeline (see `docs/posed-flipbook.md` for the format details):

- `src/posed.rs` — pose-spec document (schema v1): per-part euler deltas about
  each part's own pivot + single-level chains (child inherits parent's delta
  about the parent's neutral pivot) + per-frame durations. Validation,
  placement math (same rule the pivot experiment verified), assembly via
  `assemble_placed_frame`, `cellDownsample` (1|2|4|8), ASCII review renders.
- `src/flipbook.rs` — `compile_posed_flipbook`: rig-free sibling of
  `compile_flipbook` (the rig was only needed to resolve anchor/collision
  facts; the posed path rejects non-empty fact requests). Frames are
  run-length encoded along +X. Shared object-assembly tail factored into
  `finalize_compiled_flipbook` — the rig-driven path is byte-identical, proven
  by the pinned `flipbook_experiment` hashes.
- `voxel-kit-lab poses --spec ... --out ... --report ...` CLI subcommand.
- `src/pose.rs` — public `euler_degrees_to_quaternion` (X then Y then Z).
- `src/model.rs` — project validation relaxed: `clips: []` is now admitted
  with an empty `defaultClip` (the honest shape for a static source). All
  existing projects are unaffected.

Checked content:

- `content/characters/knight/poses/walk.poses.json` — 4-frame walk cycle
  (walk_a 180ms, pass 140ms, walk_b 180ms, pass 140ms; 6.25 fps), authored
  from the pivot experiment's validated deltas.
- `content/voxel-objects/posed-knight-walk-55a5ddbd…voxel-object.json` —
  compiled canonical object, 15,307,255 bytes, ~46.6–48.1k voxels/frame.
- `content/projects/knight-flipbook.project.json` — Studio project, one
  instance, palette materials `material/knight-1..6` from the kit.
- Evidence: `evidence/kit-poses-knight.json` (renders, counts, caps),
  `evidence/posed-flipbook-studio-smoke.json`,
  `evidence/posed-flipbook-frame{0,2}.png` (browser screenshots).
- `scripts/posed-flipbook-smoke.mjs` — protocol smoke (open project, scrub all
  four frames, expect distinct runtime frames + `setVoxelObjectFrame` ops).
- `tests/posed_flipbook_experiment.rs` — validation errors, pinned content
  hash (debug build reproduces the release CLI's hash), Engine admission,
  immutable publication, runtime project load.

Full suite: 145/145 green.

## Why the downsample

At the kit's native 6.285mm lattice the 4-frame object was ~70MB — over the
Engine's 64MiB artifact cap (`MAX_VOXEL_OBJECT_ARTIFACT_BYTES`), even after
run-length encoding (the codec pretty-prints JSON). `cellDownsample: 2` gives
a ~146-cell character (retro high-fidelity scale) at 15MB. The cap is an
Engine contract, not arbitrary per-project — if bigger flipbooks are wanted,
the options are coarser lattices, fewer frames, or a runtime that meshes
frames at compile time and toggles submeshes (user's idea; would be an Engine
task, not downstream).

## Environment traps (important!)

- The session ran on the user's **desktop**, where the repo lives on an
  **sshfs mount** (`/mnt/den-k8`). The normal dev machine is reached via ssh
  and does not have these problems — run cargo/pnpm there.
- On sshfs: parallel `cargo` corrupts rlibs ("can't find crate", "invalid
  metadata files") and `pnpm install` dies with EPERM. The copied
  `.studio-cache` provider checkout also had dangling workspace symlinks.
- Workarounds used here (all outside version control):
  - `CARGO_TARGET_DIR=/home/patch/.cache/rusty-engine-voxels-target`
  - pnpm 11.7.0 installed at `.studio-cache/pnpm` with a shim at
    `.studio-cache/bin/pnpm`
  - Studio workspace copied to `/home/patch/.cache/rusty-studio-local`,
    `pnpm install --frozen-lockfile` + `pnpm run build` there; host launched
    with `pnpm run host -- --adapter-binary <adapter> --host 127.0.0.1 --port 4310`
  - `.studio-cache/pnpm-store` local pnpm store
- On the current dev machine none of this historical setup is needed: use the
  Engine-owned Studio service, then select this repository and
  `content/projects/knight-flipbook.project.json`.
- `/home/stash/mesh-resources` does not exist on the desktop machine.

## Gotchas discovered

- Studio eagerly registers `animatedMeshResources` into the shared renderer at
  project open and **rejects clips the source GLB doesn't contain**
  (`renderer-host/animated-mesh-host.ts`). A schema-required phantom clip made
  the whole renderer unavailable. Fixed by the `clips: []` relaxation.
- Studio selects the project via the `project=` URL query param; `studio.sh`
  only prints the voxel-lab URL.
- Evidence JSONs record machine timings (`assemblyMilliseconds`,
  `conversionMicroseconds`) — suite runs rewrite them; keep them out of
  content commits unless intentionally regenerated.

## Open threads for the next agent

- Rough seams at rotated joints (M3 fusion is rig-bound; out of scope so far).
- Anchors / hit regions / collision facts on posed flipbooks (needs
  placements-based fact resolution in `compile_posed_flipbook`).
- Pose ergonomics from #6593 are now partially done (pose-spec JSON + CLI);
  incremental re-raster and a render front-end remain.
- The `conversion` block in the knight project is schema furniture; driving a
  mesh conversion from that project is not a supported workflow.
- If flipbook artifacts should get smaller: mesh-at-compile + submesh toggling
  is an Engine-side design (talk to the rusty-engine owner).
