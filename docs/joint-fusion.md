# Deterministic joint fusion

Milestone 3 turns an M2 rough assembly into a structurally admitted first-pass
frame. This is an authoring convenience, not the approved animation source of
truth: later bounded edits refine the deterministic base before immutable
flipbook compilation.

## Placement and overlap seam

The proxy rig owns part orientation. The canonical kit owns where rigid parts
meet. `socket_constrained_part_placements` therefore keeps each evaluated bone
rotation but resolves child translation through the already-placed parent and
the kit's mated socket pair. This matters when proxy and kit pivots differ: raw
independent bone translations can otherwise pull a lower arm dozens of cells
away from its elbow. Bind-pose placement is unchanged because those sockets
already coincide.

M2 overlap resolution now keeps a deterministic diagnostic for every discarded
canonical origin. A collision winner is chosen by:

1. protected-region membership;
2. outer-surface membership;
3. parent depth near the attachment hierarchy; and
4. declaration order as the final stable tie-break.

The occupied coordinate still has one owner. The loser record is diagnostic
provenance, not a second geometry authority.

Fusion receives the exact raster settings alongside the rough frame and
authoritatively re-runs M2 overlap resolution before cleanup. The supplied
discard ledger must match that result exactly; forged, altered, duplicated, or
missing records reject with `fusion.overlapLedgerMismatch`. Discard records are
never counted as preserved protected geometry.

## Fusion and cleanup

`fusion::fuse_rough_frame` performs one bounded transaction:

1. Recompute the exact socket-constrained placements for the selected pose.
2. Join each mated pair with a deterministic local capsule-like bridge between
   the nearest canonical cells. Material comes from the nearest canonical
   surface and every generated cell names its `JointBridge` identity.
3. Apply configurable, seam-local cleanup: bridge one-cell gaps, fill enclosed
   one-cell cavities, enforce limb thickness near marked seams, restore bounded
   ground contact, and remove isolated generated cells.
4. Record the ordered pass ledger. Overlap trimming and weapon normalization
   are explicit enforcement passes: overlap losers are retained as diagnostics,
   while a weapon that violates its canonical fixed dimension rejects instead
   of being silently resculpted.
5. Validate the whole candidate before returning it. No partial frame is
   published on failure.

Generated work is capped by `maxGeneratedVoxels`; one socket is capped by
`maxSocketBridgeLength`. Settings outside their bounded domains reject with
machine-readable error codes.

## Hard gates

Admission rejects:

- a missing part or protected-region cell budget;
- more than one face-connected component;
- a missing required socket neighborhood;
- a material outside the kit palette;
- a coordinate outside runtime bounds;
- a frame not resting exactly on `groundY`;
- a volume outside `volumeRange`;
- a limb below `minLimbThickness`; or
- a named part whose canonical fixed dimension changed.

Canonical cells retain `(partId, sourceVoxelIndex)`. Synthetic cells retain the
generating joint or cleanup operation. Whole-frame ground correction adds
`RestoreGroundContact` to each modified cell without replacing canonical
identity.

## Checked evidence

`tests/fusion_experiment.rs` regenerates the selected `run` schedule twice,
requires byte-identical fused JSON, exercises protected-region and generated
quota rejection, and compares the measurement with
`evidence/churn-study-high-fidelity.json`.

The checked result in `evidence/joint-fusion-study.json` reduces average
occupied-coordinate run churn from `0.6897` in the straight pipeline to
`0.270568` in the canonical-parts first pass. All generated cells remain within
four cells of an M2 seam marker.

That comparison is directional rather than a controlled visual-quality score:
the schedules retain different frame counts, and occupied-coordinate churn
includes intentional rigid motion. Generated-operation locality is proved
separately from the spatial churn number.
