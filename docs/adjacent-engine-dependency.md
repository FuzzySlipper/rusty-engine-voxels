# Adjacent Rusty Engine dependency

Rusty Engine Voxels consumes one local Rusty Engine checkout placed beside this
repository. `Cargo.toml` declares the complete Rust facade at
`../rusty-engine/rust/crates/rusty-engine`. The project compiles against the
checkout's current files exactly as they stand.

There is no Engine pin manifest, revision synchronizer, freshness comparison,
or provider update command in this repository. Project scripts must not fetch,
pull, reset, or otherwise mutate the adjacent checkout. CI creates the required
sibling shape ephemerally before running the Rust gate.

## Studio boundary

Studio is owned, built, hosted, and browser-tested by Rusty Engine. This
repository supplies only:

- `.rusty-studio.json`, which names the project-owned adapter command;
- project data and canonical content; and
- the Rust adapter binary built by `scripts/studio-adapter.sh`.

It does not install or import Engine Studio or renderer TypeScript packages.
Use the persistent Engine Studio service or an Engine-owned development host,
then select `/home/dev/rusty-engine-voxels` and a project-relative file. The
default project URL is:

```text
http://127.0.0.1:4310/?root=%2Fhome%2Fdev%2Frusty-engine-voxels&project=content%2Fprojects%2Fvoxel-lab.project.json
```

The Engine-owned real browser proof runs from `/home/dev/rusty-engine`:

```bash
./scripts/verify-studio-voxel-integration.sh /home/dev/rusty-engine-voxels
```

The downstream `./scripts/verify.sh` remains the host-neutral Rust, adapter,
artifact, and protocol gate. Engine revision identity does not enter project
bytes, adapter responses, or evidence DTOs; checked historical reports may
retain the exact revisions that originally produced them.
