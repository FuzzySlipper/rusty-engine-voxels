# Bounded frame cleanup loop

Milestone M5 turns the deterministic fused frame into a safe authoring surface.
`src/cleanup.rs` owns a closed edit language, validation, deterministic replay,
agent-facing evidence, and metric-based accept/revise evaluation. It does not
call a model, mutate source files, retain callbacks, or introduce runtime
behavior.

## Edit contract

Every operation computes an inclusive affected bounding box before doing work.
The closed schema contains:

- `add_voxel`, `remove_voxel`, `move_voxel`, `fill_box`, and `clear_box`;
- `replace_material`, `bridge_regions`, `thicken_region`, and `thin_region`;
- `copy_canonical_region`, `restore_from_previous_frame`, and
  `restore_from_next_frame`;
- `smooth_local_surface`, `carve_local_surface`, and `enforce_connectivity`;
- `shift_component`; and
- `set_anchor`.

The policy declares editable regions, palette slots, operation/region/voxel
quotas, required anchors, immutable canonical origins, protected parts and
dimension tolerances, and any part components that must remain connected.
Validation stages all edits in a private coordinate map and returns a typed
`EditError`; the base, previous frame, next frame, and accepted diff list remain
unchanged after every rejection.

One diff targets the exact SHA-256 of a deterministic fused base and identifies
its pass. The bounded automatic schedule admits at most three uniquely numbered
agent geometry passes and one temporal pass. Human edits remain explicit
recorded diffs. Reopening regenerates the result from the base and complete
ordered diff sequence rather than trusting an overwritten frame file.

## Agent bundle and decision loop

`build_agent_input_bundle` produces the current complete frame and hash,
canonical-part summaries, occupancy/component metrics, structural warnings,
style rules, front/side/top projections, corresponding part-ID passes,
previous/next difference overlays, named anchors, and the three-frame temporal
window. These projections are deterministic geometric observations for an
agent, not final art renders.

`evaluate_cleanup_diff` performs the bounded loop:

1. replay accepted diffs;
2. stage and structurally validate the proposed diff;
3. rebuild the complete diagnostic bundle;
4. compare occupied-volume, component, and warning metrics; and
5. return `Accept` or `Revise`.

Hard safety failures return typed rejection before a candidate exists. A
`Revise` candidate is observational and is not appended to the caller's
accepted diff list.

## Checked character proof

`tests/cleanup_experiment.rs` removes one real canonical voxel from the checked
rifleman's left lower arm, supplies the defective frame with its previous and
next neighbors, and restores the defect solely through
`restore_from_previous_frame`. Replaying the recorded diff yields the identical
1,391-voxel result. A separate `remove_voxel` hand edit removes one unprotected
backpack voxel, proving that cleanup is not add-only.

The same suite executes and round-trips all 17 operation shapes and verifies
typed failures for undeclared regions, invalid palette slots, voxel quota,
protected-origin changes, missing required anchors, and disconnected required
components. Exact checked facts are in `evidence/cleanup-loop.json`.
