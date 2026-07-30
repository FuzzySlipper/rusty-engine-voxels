# Engine revision updates

The voxel lab has one consumer-owned Engine identity: [`../engine-source.json`](../engine-source.json).
Its closed schema names the canonical public repository, one lowercase 40-character commit, and
the voxel lab's `studio` provider directory. The commit is projected into exactly six direct Rust
dependencies and their Cargo lock entries. Runtime and experiment evidence read the embedded
manifest through the same validator; there is no second authored revision constant.

## Commands

Run these commands from the repository root:

```bash
./scripts/engine-revision check
./scripts/engine-revision update <40-character-public-sha> --dry-run
./scripts/engine-revision update <40-character-public-sha>
```

`check` is deterministic, read-only, and does not intentionally use the network. It rejects
malformed or extended source manifests; sibling, path, branch, tag, floating, aliased, mixed, or
non-canonical Cargo sources; missing or duplicate package blocks; and any disagreement between the
manifest, six direct dependencies, and every locked Engine package.

`update` first requires the current pin to pass `check` and only refuses dirty active carriers. It
therefore preserves unrelated work while preventing an update from overwriting edits to:

- `engine-source.json`;
- `Cargo.toml`; or
- `Cargo.lock`.

The command proves the requested commit with an exact fetch from the canonical public repository,
creates a detached disposable worktree at the caller's current HEAD, rewrites only those three
carriers, regenerates the Cargo lock, and validates the complete candidate. Before applying the
candidate patch it rechecks both caller HEAD and carrier cleanliness. Candidate failure, a caller
race, or an invalid target leaves the caller untouched and removes the disposable worktree.

`--dry-run` performs the same public fetch, regeneration, and candidate validation, prints the
prospective carrier diff, and does not mutate the caller. A normal update leaves the three carrier
changes uncommitted for review.

## Intentional compatibility work

The command does not mechanically rewrite Studio protocol declarations, TypeScript or renderer
package identities, checked evidence, reports, documentation, or any Engine-owned reverse-consumer
pin. After a provider update, adapt compatibility deliberately and run:

```bash
./scripts/verify.sh
./scripts/verify-studio.sh
```

Historical evidence remains attributable to the Engine revision recorded in that evidence. New
evidence derives its revision from the current embedded source manifest.

## Rollback

Rollback uses the same path rather than hand-editing carriers:

```bash
./scripts/engine-revision update <previous-40-character-public-sha>
```

Commit or otherwise preserve the current carrier change before initiating another update, then run
both verification gates at the restored revision.
