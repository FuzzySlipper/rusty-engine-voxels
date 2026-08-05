# Reference-media voxel review

The review harness is an authoring aid for people and agents. It does not
convert reference media into voxels and it does not publish candidate edits.
It opens a canonical downstream project through the real Studio adapter,
scrubs explicitly selected candidate frames, applies authored camera moves,
and captures the shared renderer beside each reference image.

## Reference manifest

`content/reviews/directional-sentinel-reference-review.json` is the checked
directional-sprite example. Its important separation is:

- `reference.entries[]` identifies the target media and its source frame or
  direction;
- `candidateFrameIndex` identifies the voxel frame to inspect;
- `camera` selects an authored sequence of disposable viewport moves;
- `interpretation` records the human judgment and limitations without making
  it part of Engine or canonical voxel state.

The reference kind is intentionally media-oriented rather than conversion-
oriented. It accepts `directional-sprite`, `image-sequence`, and
`video-frame-sequence`. All entries currently point to PNGs so the comparison
sheet is portable; a video workflow can extract review frames first and list
their timestamps in `timeMicroseconds`. The video itself remains source
media, not an Engine asset.

Example entry:

```json
{
  "id": "turntable-012",
  "label": "Turntable at 1.2 seconds",
  "path": "local/reference/turntable/frame-012.png",
  "timeMicroseconds": 1200000,
  "candidateFrameIndex": 12,
  "camera": "yaw-90"
}
```

The manifest is deliberately explicit. It does not infer camera calibration,
direction correspondence, depth, palette, or frame matching. Those are the
things a human or an agent should be able to revise while inspecting the
result.

## Run a review pass

From the repository root:

```bash
./scripts/reference-media-review.sh \
  content/reviews/directional-sentinel-reference-review.json
```

The script resolves the exact provider from `engine-source.json`, builds the
downstream adapter if needed, opens the candidate project through Studio, and
writes disposable output under `local/reference-media-review/` by default:

- `reference-media-review.json` records source paths and hashes, candidate
  object/frame identity, camera identity, renderer frame hashes, screenshot
  hashes, and provider/Engine revisions;
- `reference-comparison.svg` places every target PNG beside its candidate
  renderer capture for fast visual scanning;
- one PNG per reference entry preserves the actual shared-renderer view.

The output is a review pack, not canonical project state. Edit the voxel model
through the normal Studio/adapter workflow, rerun the command, and compare the
new pack against the previous one. Agents can use the JSON manifest to select
the next frame/view and the SVG/PNGs to decide what to change. Humans can use
the same files as a compact side-by-side art review.

## What this makes possible

This supports an iterative loop without pretending that hidden depth is
deterministically recoverable:

1. establish or edit a candidate voxel volume in Studio;
2. capture the candidate from the angles that matter;
3. compare each capture with the corresponding sprite, still, or extracted
   video frame;
4. make a bounded edit to the candidate;
5. rerun the review pack and inspect the change.

The first directional example uses the existing uncertain local sprite sheet
and the existing eight-frame candidate. It is intentionally an inspection
fixture, not a production reconstruction claim. A later editor can add
voxel-level or region-level annotations, but those edits should remain
downstream authoring data until the accepted candidate is explicitly applied
and read back through Studio.
