# Manual piece pivoting (rig-free posing)

**Question:** can an agent pose a mesh-derived kit by reasoning about part
rotations alone — no rig, no skinning, no engine changes — using the existing
rigid rasterization and assembly tooling? **Answer: yes, with caveats that
shape the next tooling steps.**

## What was tested

`tests/kit_pivot_experiment.rs` loads the checked knight kit (11 parts,
167,962 voxels — see `docs/kit-bake.md`), authors four poses as pivot
rotations (neutral, idle, walk_a, walk_b) with attachment chains (hands
follow arms, sword/pillum follow the arm-hand chain), and assembles rough
frames through the rig-free `assemble_placed_frame`. Evidence:
`evidence/kit-pivot-knight.json`.

## Results

| Pose | Voxels | Fusion candidates | Assembly (debug) |
|---|---|---|---|
| neutral | 167,962 | 39,737 | 8 s |
| idle | 200,117 | 44,953 | 31 s |
| walk_a | 216,363 | 40,354 | 66 s |
| walk_b | 216,629 | 38,928 | 66 s |

Verified: deterministic regeneration (same pose assembled twice is
identical); ground contact holds within a few cells (a real stride drops the
trailing heel below the standing plane — that is correct); per-part volume
stable within seam-overlap noise; torn seams at rotated limbs surface as
fusion candidates for the M3 handoff. The pipeline's headline property holds
at manual scale: **parts that do not move contribute exactly zero churn**
(idle cloth/legs: 0 displaced cells of 19,533 / 22,034).

## Caveats that matter for animation authoring

1. **Pixel density makes small rotations expensive.** At 6.3 mm cells, even
   a 2–5° pivot re-shuffles a third or more of a part's cells (idle torso at
   2° yaw: 33.9% displaced; the long thin pillum at 4°: 95%). This is honest
   rigid motion, not resampling noise — but it means idle/still poses need
   sub-degree deltas or hysteresis to keep still-region churn near zero, and
   pose schedules should only rotate parts at event frames.
2. **Rotation dilates.** Conservative rotation at this scale adds ~20–40%
   cells to swinging parts (legs at ±14–18°: +36–40%). The contract admits
   this; frame authors should know walk-scale motion costs real volume.
3. **Assembly latency.** ~8–66 s per pose in a debug build (rasterization +
   conservative repair + seam marking). Interactive agent-in-the-loop posing
   wants the optimized repair path (union-find + incremental scoring, landed
   here — rotated-part repair went from >120 s to ~2 s per part) and would
   want release builds or incremental re-rasterization for iteration.
4. **Chains are manual.** The attachment chains (hand→arm, weapon→hand) are
   composed per pose in the test. A small "pose spec" document format (per
   part deltas + chains) is the natural next artifact — it is exactly what a
   Studio posing tool or an agent prompt would author.

## What this says about Studio tooling

The agent-driven path (reason about pivots, write rotations, render, revise)
works with the existing Rust tooling — the kit, the neutral transforms, and
the rig-free assembler are sufficient. The gaps are *ergonomic*, not
fundamental:

- A pose-spec document (JSON: part deltas + chains) instead of test-code
  constants, so poses are authored data, not code.
- Faster iteration (release-mode assembly or incremental re-raster) to make
  render→revise loops practical at 100k+ voxels.
- Multiview renders as a first-class CLI (they exist inside the test
  harnesses; a `voxel-kit-lab pose --spec ... --render` front-end would make
  them standalone).

Fusion (M3) still needs a rigged motion source for its context — the rig
remains the plan for real animation (#6592); manual pivoting is for
authoring and validating poses in the meantime.
