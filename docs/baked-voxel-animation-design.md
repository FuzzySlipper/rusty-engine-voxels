# Baked Voxel Character Animation — Project Technical Design

**Status:** Design for implementation
**Supersedes:** `animation-pipeline-idea.md` (rough concept; retained for rationale)
**Authoring target:** 90s CRPG stepped animation, first-person, 80/20 production cost
**Baseline corpus:** retro-character (24×36×24) and retro-character-high-fidelity (96×144×96)

---

## 0. What this document is

`animation-pipeline-idea.md` captured the concept. This document is the implementation-specific
design for *this* repository and its measured corpus. Where the two disagree, this document wins.

The load-bearing decisions, settled here:

1. **Baked, not runtime-evaluated.** The runtime output is an immutable voxel flipbook. Joint
   fusion, hole filling, and per-frame sculpting happen at *authoring time* inside the editor /
   agent cleanup loop, where a human or LLM can see and edit the result. They do not happen at
   runtime.
2. **Deterministic joint fusion is an editor convenience, not the source of truth.** It produces a
   first-pass seam; every frame is then hand/agent edited — filling holes, removing stray voxels,
   and *deleting part voxels where a pose reads better without them*. All edits are diffs against
   the deterministic first pass.
3. **Runtime never meshes during play.** All pose meshes are produced at load; playback toggles
   which pose mesh is visible. This is the same "hide/show submeshes" shape as the runtime-bones
   fallback, but with zero per-frame meshing cost.

---

## 1. Why baked is the correct first play (and what it buys)

The deciding factor is not storage or meshing speed — it is **editability**. The straight
mesh→flipbook pipeline in this repo resamples the continuous skinned surface independently every
pose, so adjacent frames disagree on ~20–70% of occupied cells even where nothing meaningful moved
(see §3). A baked canonical-parts pipeline replaces per-frame re-voxelization with **rigid
transforms of stable voxel parts**, so the only per-frame geometry question left is *how the parts
meet at the joints*.

Because that question is resolved at authoring time, every seam, hole, and stray voxel is a thing a
reviewer (human or LLM) can see, measure, and fix — and the fix is durable. Runtime evaluation
would force that same resolution to happen blind, every frame, with no reviewer in the loop.

If the baked workflow ever failed and the project pivoted to runtime "half-ass bones," the honest
fallback is to **accept gaps and intersection as art style** (robot / magic-critter characters with
deliberately disconnected limbs) rather than attempt runtime hole-filling. Runtime coverup-submesh
tricks are noted as an awkward last resort, not a plan.

---

## 2. The churn problem this solves (measured)

The current straight pipeline's defect is **resampling churn**, not motion. Direct measurement of
the checked high-fidelity object (occupied-cell symmetric difference between consecutive stored
frames, bucketed into 4 equal-height bands; region 0 = feet, region 3 = head):

| Clip | Stored frames | Avg churn / transition | Dominant band | Band distribution (feet→head) |
|---|---|---|---|---|
| `clip/idle` | 7 | 19.5% | region 2 (36%) | 0.11 / 0.27 / 0.36 / 0.26 |
| `clip/jump` | 3 | 7.2% | region 3 (37%) | 0.10 / 0.31 / 0.22 / 0.37 |
| `clip/run` | 4 | **69.0%** | region 0 (32%) | 0.32 / 0.32 / 0.20 / 0.15 |

Evidence: `evidence/churn-study-high-fidelity.json`, produced by `src/churn.rs` (reproducible via
the `churn_study_localizes_flipbook_aliasing_to_limb_regions` test).

Reading:

- The faster the limb motion, the worse the churn (run 69% ≫ idle 19.5%). This is aliasing, not
  signal: the *same* leg re-voxelized from a slightly shifted continuous surface flips cells.
- Churn spreads across **all** bands in the straight pipeline. Even idle — which should be nearly
  still — shows churn in the head band (region 3 at 26%) where almost nothing is moving.
- A canonical-parts pipeline should **confine churn to joint seams** and drive core/head churn
  toward ~0. That is the gate the new pipeline must beat (§6).

---

## 3. Architecture

```text
Canonical exploded voxel kit        (authored once, versioned)
        │  parts + pivots + sockets + palette + invariants
        ▼
Proxy rig pose source               (authored animation / mocap / fitted video)
        │  bone transforms per sampled pose — NO skinning, NO runtime rig
        ▼
Rigid part placement                (per pose: transform each stable part cell set)
        ▼
Deterministic first pass            (socket bridge + overlap resolve + connectivity repair)
        ▼
Per-frame hand / agent edit         (diffs: fill holes, remove strays, delete part voxels)
        ▼
Hard gates + temporal review        (churn localized to seams, identity preserved)
        ▼
Baked immutable flipbook            (canonical voxel-object, same format as today)
        ▼
Runtime: load all pose meshes once, toggle visibility per frame — no per-frame meshing
```

Two properties are doing the real work:

- **Parts are voxelized once.** A `left_lower_arm` is a fixed integer cell set with a stable pivot.
  A pose is a rigid transform of that set. There is no continuous surface left to alias.
- **Edits are diffs against a deterministic base.** The deterministic first pass is reproducible;
  every hand/agent change is a bounded, reviewable diff on top. Regenerating a frame re-applies the
  same base plus the same diffs.

---

## 4. Runtime meshing strategy (decided)

All pose meshes are produced at load; playback toggles visibility.

```text
load:  admit voxel-object -> resolve frames -> mesh every unique frame once -> retain all meshes
play:  for each animation sample, enable the current pose mesh, disable the previous one
```

This matches the existing engine runtime shape: `voxel-object-runtime` already admits the object
and meshes every unique frame up front, and `render-projection` already plays back by swapping
which frame is referenced (`setVoxelObjectFrame` carries a single small op per sample — measured in
`evidence/high-fidelity-animated-voxel-report.json`, steady-state samples are one op, not a
re-mesh). So the "mesh all poses at load, toggle during play" model is **already how the engine
works**; the baked pipeline just feeds it cleaner frames.

The cost to confirm at target density is load-time meshing memory (all unique pose meshes resident).
That is a measured quantity, not a guess — `runtime.uniqueMeshPayloadBytes` in the HF report is
34.5 MB for 14 meshes at 96×144×96. If a future character pushes this too high, the lever is fewer
stored poses or a coarser grid, not per-frame meshing.

---

## 5. What changes vs the current straight pipeline

| Concern | Straight mesh→flipbook (current) | Baked canonical-parts (this design) |
|---|---|---|
| Per-frame geometry | re-voxelize whole skinned surface | rigid-transform stable parts |
| Frame-to-frame churn | 20–70% of cells, all bands | joint-seam-localized only |
| Vertex correspondence across frames | none (~65% mesh churn) | parts stable; only seams differ |
| Runtime rig / skinning | none (flipbook) | none (flipbook) |
| Runtime meshing during play | none (meshes prebuilt) | none (meshes prebuilt) |
| Joint handling | implicit in resampling noise | explicit, editor-fused, hand-finished |
| Editability of a frame | opaque re-voxelization | deterministic base + reviewable diffs |
| Reuse across animations | re-convert per animation | one kit, many pose sources |

---

## 6. Success gate for the vertical slice

The first baked character must beat this repo's straight-pipeline baseline on the churn metric:

1. **Core/head churn ≈ 0.** In clips where a region is not intentionally moving (idle head, planted
   feet), frame-to-frame churn in that band approaches zero. Baseline to beat: idle head band
   (region 3) currently carries 26% of churn.
2. **Churn is joint-localized.** In a run clip, churn concentrates at joint seams rather than
   spreading across whole limbs. Baseline to beat: run churn is currently spread 32/32/20/15 across
   all four bands.
3. **Identity preserved.** The character reads as the same character from arbitrary gameplay
   angles in every frame (hard gates: connectivity, ground contact, protected dimensions, palette
   stability).
4. **Deterministic regeneration.** Rebuilding a frame from kit + pose + recorded diffs reproduces
   the approved frame exactly.

The churn measurement is already implemented (`src/churn.rs`), so criteria 1–2 are runnable, not
subjective.

---

## 7. What this design deliberately does NOT solve (yet)

- **Runtime part-transform ("half-ass bones").** A separate, later feature for procedural motion /
  IK. If pursued, prefer accepting gaps/intersection as art style over runtime hole-filling.
- **Video→proxy-motion fitting.** A tailwind from recent AI pose-estimation progress, but a
  separate input stage; the pipeline validates first with ordinary rigged animation.
- **Temporal occupancy filtering of the straight pipeline.** Still a valid, smaller improvement to
  the *current* conversion path (upstream), independent of this redesign.

---

## 8. Relationship to existing work

- Reuses this repo's corpus as the churn baseline and its `churn.rs`/`format_study.rs` harnesses as
  the measurement gate.
- Runtime output stays the **same canonical voxel-object format and flipbook player** the engine
  already owns — no new runtime format, no rig at runtime.
- Joint fusion, the exploded kit, and the edit DSL are new authoring-side components; they are
  upstream `voxel-convert`-class mechanisms, not downstream copies, per this repo's AGENTS.md
  ownership boundary.
