# Rusty Engine Voxels agent guidance

This repository is a standalone downstream Studio project and voxel experimentation space. It owns
its project schema, source corpus, experiment settings, instance/playback choices, evidence, and
Studio adapter. Rusty Engine is an exact public provider dependency; do not add sibling-checkout
dependencies on `rusty-engine` or `rusty-engine-demo`.

- Use the Engine owners for mesh import, animation sampling, voxel conversion, canonical assets,
  runtime admission, playback, and renderer-neutral projection. Do not reproduce those semantics.
- Keep canonical voxel objects distinct from the downstream project document and from transient
  Studio candidates.
- Keep animation playback caller-driven with explicit time. Do not add component callbacks or a
  universal scheduler.
- Checked source assets must retain their adjacent license and provenance.
- Experiments should be reproducible from the checked source and exact provider revision. Record
  measured results without turning machine-specific timings into CI thresholds.
- The Studio adapter may reject unrelated protocol operations, but responses for supported
  operations must remain closed, bounded, and attributable to a named owner.
- Keep generated caches and provider checkouts outside version control.

## Den Guidance Bootstrap

- Project ID: `rusty-engine-voxels`
- Resolve live guidance with the Den MCP `get_agent_guidance` tool before
  substantial work.
- Treat the resolved Den guidance packet and its referenced Den documents as
  the source of truth.
- If Den is unreachable, stop and tell the user which Den tool or command
  failed and what you were about to do. Do not reconstruct Den state from local files.