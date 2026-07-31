# Video-fitted proxy motion

M7 adds one optional input stage to the exploded-kit pipeline. It does not
derive character geometry from video. A synchronized four-view clip supplies
motion evidence; Rust admits and fits that evidence into the same rigid proxy
parts already consumed by M2 through M6.

## Checked source

The checked 16-frame, 24 fps run is rendered from Kenney's CC0
`character-medium.glb` with stable orthographic front, side, back, and
three-quarter cameras. The lossless, bit-exact FFV1 NUT video is
`content/sources/kenney-retro-character/run-multiview.nut`. The original model
and its license remain beside it.

`scripts/regenerate-video-motion-evidence.sh` owns complete source
regeneration. It renders every view with Blender, encodes the panels losslessly,
decodes that video through the pinned MediaPipe Pose Landmarker, invokes the
Rust fitter and retargeter, and serializes the resulting Rust-owned transforms
as `motion.glb`.

The MediaPipe package is pinned to 0.10.35. Its external full float16 model is
pinned by SHA-256
`5134a3aad27a58b93da0088d431f366da362b44e3ccfbe3462b3827a839011b1`;
it is downloaded into the ignored regeneration cache and is not redistributed.

## Authority and fitting

`src/video_motion.rs` is the fit owner. It strictly admits bounded cameras,
frames, observations, confidence values, and timing. For every semantic joint
it solves weighted orthographic camera equations, applies a three-sample
temporal filter, computes one median length for each of 16 body bones and the
weapon span, and projects every frame onto those exact fixed lengths.

Contact correction detects low, slow feet. A contact run keeps its fitted foot
fixed and translates the complete proxy, preserving every bone length. The
checked run has one three-frame right-foot contact. Two isolated MediaPipe
detection gaps are explicitly marked and interpolated at half observation
weight; endpoint and adjacent gaps reject during evidence generation.

The checked character is unarmed. Its per-view grip and muzzle landmarks are
therefore explicitly identified as `inferredFromRightHandAxis`, rather than
misrepresented as image detections. They form a fixed-length virtual weapon
proxy for the rifle part.

The retargeter is also Rust-owned. The authored proxy's already-admissible
`run@0us` pose supplies calibration only; it prevents a neutral exploded kit
from reintroducing protected-part overlaps. Later motion comes from fitted
landmark deltas. The checked policy converts source meters to 0.04-meter voxel
cells, applies 0.25 translation and rotation gains, and records those values in
`evidence/video-motion/proxy-motion.json`. Python only packs those complete
transforms into GLB buffers.

The source video and estimator output are evidence, not geometry authority.
The canonical kit remains unchanged, all fitted parts are rigid, and
`fitted_motion_rig_map` derives an ordinary M2 rig map from named proxy nodes.

## Proof and limits

`tests/video_motion_experiment.rs` proves:

- all 17 admitted body/weapon lengths remain exact after smoothing and contact
  correction;
- contact frames have no fitted right-foot slide;
- the checked fit is reproducible from the admitted landmarks;
- the generated `motion.glb` imports through the exact Engine pin; and
- its 16-frame `fitted-run` traverses M2 assembly, M3 fusion, M4 canonical
  compilation/runtime admission, and M6 temporal analysis.

The fitted clip has zero canonical-identity churn and 258,729 millionths
average spatial churn. The authored M1-M6 run is the comparison reference at
270,567 millionths. These structural metrics establish comparable pipeline
fitness, not perceptual equivalence or general human-video capture quality.
The focused test also constructs the deterministic M6 GIF review.

`scripts/check-video-motion-evidence.sh` regenerates the fitted JSON,
Rust-retargeted proxy JSON, and GLB from checked landmarks and requires
byte-identical outputs. The ordinary repository gate runs this check without
Blender, MediaPipe, or network access.

Current limits are intentional:

- the checked source is generated, single-subject, calibrated, and
  unoccluded;
- weapon endpoints are a declared virtual proxy because the source is unarmed;
- camera synchronization and association are fixed by the panel contract; and
- this is an offline authoring stage, not a capture runtime or general pose
  estimation framework.
