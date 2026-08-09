# Engine revision updates

The voxel lab has one ordinary Rust dependency: the complete `rusty-engine` facade from the
canonical public `main` branch. Its libraries remain available through preserved namespaces such
as `rusty_engine::voxel_convert`; downstream does not select individual crates. `Cargo.lock` records
one exact Engine resolution, and [`../engine-source.json`](../engine-source.json) records that same
lowercase 40-character commit for runtime evidence and the Engine-owned `studio` checkout. The
common validator rejects drift between those surfaces.

## Commands

Run these commands from the repository root:

```bash
./scripts/engine-revision check
./scripts/engine-revision dev check
./scripts/engine-revision dev sync --report-only --json
./scripts/engine-revision update <40-character-public-sha> --dry-run
./scripts/engine-revision update <40-character-public-sha>
```

`check` is deterministic, read-only, and does not intentionally use the network. It rejects
malformed or extended source manifests; sibling, path, tag, revision-pinned, aliased, mixed, or
non-canonical Cargo sources; anything other than one direct rolling facade dependency; missing or
duplicate facade lock blocks; and any disagreement between the source manifest and every locked
Engine package.

`dev sync` resolves the committed `engine-development.json` intent (`refs/heads/main`) to one
public or explicitly supplied local Engine SHA and writes the ignored
`.engine-development/resolution.json` report. `--report-only` inspects a moving source without
changing the exact resolution. `dev check` strictly decodes that report and verifies that the
active source/lock projection uses the same SHA; it rejects stale or tampered reports. The ordinary
verification gate runs public report-only resolution followed by this check, so a newly advanced
Engine `main` breaks loudly instead of leaving the consumer silently stale. Development mode is
operational compatibility work, not certification evidence.

`update` first requires the current pin to pass `check` and only refuses dirty active carriers. It
therefore preserves unrelated work while preventing an update from overwriting edits to:

- `engine-source.json`;
- `Cargo.toml`; or
- `Cargo.lock`.

The command proves the requested commit with an exact fetch from the canonical public repository,
creates a detached disposable worktree at the caller's current HEAD, updates the exact source
manifest, asks Cargo to resolve the rolling facade precisely to the requested commit, and validates
the complete candidate. `Cargo.toml` remains the rolling `main` declaration. Before applying the
candidate patch it rechecks both caller HEAD and carrier cleanliness. Candidate failure, a caller
race, or an invalid target leaves the caller untouched and removes the disposable worktree.

`--dry-run` performs the same public fetch, regeneration, and candidate validation, prints the
prospective diff, and does not mutate the caller. A normal update leaves the synchronized
`engine-source.json` and `Cargo.lock` changes uncommitted for review.

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
