# Temporal consistency and flicker review

Milestone 6 validates a finished fused schedule before it is compiled into the
immutable Engine voxel-object. The validator is downstream authoring evidence:
it observes M3 provenance and M4 frame facts, but does not become a runtime
scheduler, renderer authority, or animation format.

## What is measured

For every canonical part in every frame, `src/temporal.rs` records:

- voxel count, occupied bounds and dimensions;
- centroid and variance-ranked principal axes;
- palette-slot histogram and exposed surface area;
- face-connected component count and front silhouette area; and
- all pairwise distances between the frame's named anchors.

Neighbor comparisons produce typed soft warnings with an exact frame, region,
and review view. The current warning families cover volume and dimension drift,
component changes, large generated seams, canonical material changes, and
anchor-to-proxy drift. The caller owns tolerances because they are art-direction
policy, not reusable Engine semantics.

Hard failures are reserved for facts that make comparison untrustworthy:
duplicate occupied coordinates, a changed explicitly protected source-identity
inventory, a missing/blinking required anchor, or a missing proxy anchor. M3's
authoritatively validated discarded-overlap ledger remains part of the
canonical identity inventory; occlusion is therefore not misreported as an
identity disappearing.

## Checked result

`tests/temporal_experiment.rs` rebuilds the rifleman `run` clip from the kit and
source GLB, then compares the finished 20-frame schedule with the straight
mesh-to-flipbook baseline:

| Measure | Straight pipeline | Canonical-parts schedule |
|---|---:|---:|
| average occupied-coordinate churn | 689,700 millionths | 270,567 millionths |
| improvement | — | 607,704 millionths |
| canonical source-identity churn | not available | 0 millionths |

Every one of the thirteen parts, including head, torso, and rifle, retains a
zero canonical-identity churn result. Spatial churn remains nonzero because
rigid parts intentionally move through world cells; its four height-band counts
are `3761 / 1624 / 1737 / 1083`. M3 separately proves that generated changes
remain local to marked seams. The combination avoids presenting ordinary
locomotion as boiling while still exposing where the visible silhouette
changes.

This proof caught and corrected a real M2 defect: coverage cells could occupy a
missing source voxel's preferred destination and cause its fallback placement
to be skipped. The raster owner now retains every canonical source identity
before M6 evidence is accepted.

## Human review artifacts

The same deterministic run generates:

- `evidence/temporal-review/alternating.gif`;
- a three-frame onion-skin SVG;
- a temporal difference heat-map SVG;
- a silhouette-edge-motion SVG; and
- a palette-flicker SVG.

Hashes and byte counts live in `evidence/temporal-consistency.json`. These are
diagnostic projections for human review. They do not mutate a frame, certify
subjective art quality, or replace runtime/browser proof.

Regenerate the checked files with:

```bash
RUSTY_UPDATE_TEMPORAL_EVIDENCE=1 \
  cargo test --locked --test temporal_experiment \
  finished_run_passes_identity_churn_and_generates_flicker_review
```

Then run `./scripts/verify.sh`; an ordinary test run compares regenerated
values and bytes with the checked evidence.
