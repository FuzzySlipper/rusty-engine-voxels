use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::validation::{
    checked_provider_pin_at, parse_engine_source, EngineSource, ACTIVE_CARRIER_PATHS, ENGINE_CRATES,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReceipt {
    pub previous_commit: String,
    pub commit: String,
    pub dry_run: bool,
    pub diff: String,
}

/// Proves and prepares an exact Engine revision change in a disposable worktree.
///
/// # Errors
///
/// Returns an error when the current pin is incoherent, active carriers are dirty,
/// the target is not publicly fetchable, candidate regeneration or validation
/// fails, or the caller changes while the candidate is being prepared.
pub fn update_engine_revision(
    repo_root: &Path,
    commit: &str,
    dry_run: bool,
) -> Result<UpdateReceipt, String> {
    update_engine_revision_with(
        repo_root,
        commit,
        dry_run,
        prove_public_commit,
        regenerate_lock,
        validate_candidate,
        |_| Ok(()),
    )
}

fn update_engine_revision_with<P, R, V, B>(
    repo_root: &Path,
    commit: &str,
    dry_run: bool,
    prove_public: P,
    regenerate: R,
    validate: V,
    before_apply: B,
) -> Result<UpdateReceipt, String>
where
    P: Fn(&str, &str) -> Result<(), String>,
    R: Fn(&Path, &str, &str) -> Result<(), String>,
    V: Fn(&Path) -> Result<(), String>,
    B: Fn(&Path) -> Result<(), String>,
{
    if !is_commit(commit) {
        return Err("update commit must be one lowercase 40-character SHA".to_owned());
    }
    let before = checked_provider_pin_at(repo_root)?;
    assert_carriers_clean(repo_root)?;
    prove_public(&before.repository, commit)?;

    let head = git(repo_root, &["rev-parse", "HEAD"])?.trim().to_owned();
    let temporary_root = temporary_directory("rusty-engine-voxels-revision")?;
    let candidate = temporary_root.join("candidate");
    let mut worktree_added = false;
    let result = (|| {
        git(
            repo_root,
            &["worktree", "add", "--detach", path_text(&candidate)?, &head],
        )?;
        worktree_added = true;
        rewrite_active_carriers(&candidate, &before.commit, commit)?;
        regenerate(&candidate, &before.commit, commit)?;
        validate(&candidate)?;
        assert_candidate_scope(&candidate)?;
        let diff = scoped_diff(&candidate)?;

        if dry_run {
            return Ok(UpdateReceipt {
                previous_commit: before.commit.clone(),
                commit: commit.to_owned(),
                dry_run: true,
                diff,
            });
        }

        before_apply(repo_root)?;
        let current_head = git(repo_root, &["rev-parse", "HEAD"])?;
        if current_head.trim() != head {
            return Err(format!(
                "caller HEAD changed during update; expected {head}. No update was applied"
            ));
        }
        assert_carriers_clean(repo_root)?;
        if !diff.is_empty() {
            run_with_input(
                "git",
                &["apply", "--whitespace=nowarn", "-"],
                repo_root,
                &diff,
            )?;
        }
        checked_provider_pin_at(repo_root)?;
        Ok(UpdateReceipt {
            previous_commit: before.commit,
            commit: commit.to_owned(),
            dry_run: false,
            diff,
        })
    })();

    if worktree_added {
        let _ = git(
            repo_root,
            &["worktree", "remove", "--force", path_text(&candidate)?],
        );
    }
    let _ = fs::remove_dir_all(&temporary_root);
    let _ = git(repo_root, &["worktree", "prune"]);
    result
}

fn rewrite_active_carriers(
    repo_root: &Path,
    previous_commit: &str,
    commit: &str,
) -> Result<(), String> {
    if !is_commit(previous_commit) || !is_commit(commit) {
        return Err("carrier rewrite requires exact lowercase commits".to_owned());
    }
    let source_path = repo_root.join("engine-source.json");
    let current_source = fs::read_to_string(&source_path)
        .map_err(|error| format!("engine-source.json cannot be read: {error}"))?;
    let source = parse_engine_source(&current_source)?;
    if source.commit != previous_commit {
        return Err("engine-source.json changed before candidate rewrite".to_owned());
    }
    let replacement = EngineSource {
        commit: commit.to_owned(),
        ..source
    };
    let encoded = serde_json::to_string_pretty(&replacement)
        .map_err(|error| format!("engine-source.json cannot be encoded: {error}"))?;
    fs::write(source_path, format!("{encoded}\n"))
        .map_err(|error| format!("engine-source.json cannot be written: {error}"))?;

    let manifest_path = repo_root.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| format!("Cargo.toml cannot be read: {error}"))?;
    let occurrences = manifest.matches(previous_commit).count();
    if occurrences != ENGINE_CRATES.len() {
        return Err(format!(
            "Cargo.toml expected {} active commit carriers; observed {occurrences}",
            ENGINE_CRATES.len()
        ));
    }
    fs::write(manifest_path, manifest.replace(previous_commit, commit))
        .map_err(|error| format!("Cargo.toml cannot be written: {error}"))?;
    Ok(())
}

fn prove_public_commit(repository: &str, commit: &str) -> Result<(), String> {
    let temporary_root = temporary_directory("rusty-engine-public-commit")?;
    let result = (|| {
        run("git", &["init", "--bare", "--quiet", "."], &temporary_root)?;
        run(
            "git",
            &[
                "-c",
                "protocol.version=2",
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=1",
                repository,
                commit,
            ],
            &temporary_root,
        )?;
        let fetched = git(&temporary_root, &["rev-parse", "FETCH_HEAD"])?;
        if fetched.trim() != commit {
            return Err(format!(
                "public fetch resolved {}; expected exact commit {commit}",
                fetched.trim()
            ));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&temporary_root);
    result.map_err(|error| {
        format!("Engine commit {commit} is not publicly fetchable from {repository}: {error}")
    })
}

fn regenerate_lock(candidate: &Path, _previous: &str, _commit: &str) -> Result<(), String> {
    let manifest_path = candidate.join("Cargo.toml");
    let manifest = path_text(&manifest_path)?;
    run(
        "cargo",
        &[
            "metadata",
            "--format-version",
            "1",
            "--manifest-path",
            manifest,
        ],
        candidate,
    )?;
    Ok(())
}

fn validate_candidate(candidate: &Path) -> Result<(), String> {
    checked_provider_pin_at(candidate)?;
    let manifest_path = candidate.join("Cargo.toml");
    let manifest = path_text(&manifest_path)?;
    run(
        "cargo",
        &[
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--no-deps",
            "--manifest-path",
            manifest,
        ],
        candidate,
    )?;
    Ok(())
}

fn assert_carriers_clean(repo_root: &Path) -> Result<(), String> {
    let mut args = vec!["status", "--porcelain=v1", "--"];
    args.extend(ACTIVE_CARRIER_PATHS);
    let status = git(repo_root, &args)?;
    if status.trim().is_empty() {
        Ok(())
    } else {
        Err(format!(
            "active Engine carrier or lock files are dirty; preserve or commit them before update:\n{}",
            status.trim()
        ))
    }
}

fn assert_candidate_scope(candidate: &Path) -> Result<(), String> {
    let status = git(candidate, &["status", "--porcelain=v1"])?;
    for entry in status.lines() {
        let path = entry.get(3..).unwrap_or(entry).trim();
        if !ACTIVE_CARRIER_PATHS.contains(&path) {
            return Err(format!(
                "candidate update changed non-carrier path {path}; no update was applied"
            ));
        }
    }
    Ok(())
}

fn scoped_diff(repo_root: &Path) -> Result<String, String> {
    let mut args = vec!["diff", "--binary", "--"];
    args.extend(ACTIVE_CARRIER_PATHS);
    git(repo_root, &args)
}

fn temporary_directory(prefix: &str) -> Result<PathBuf, String> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock cannot create temporary path: {error}"))?
        .as_nanos();
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id()));
    fs::create_dir(&path).map_err(|error| {
        format!(
            "cannot create temporary directory {}: {error}",
            path.display()
        )
    })?;
    Ok(path)
}

fn is_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("path is not valid UTF-8: {}", path.display()))
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    run("git", args, cwd)
}

fn run(program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("{program} emitted non-UTF-8 output: {error}"))
    } else {
        Err(format!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn run_with_input(program: &str, args: &[&str], cwd: &Path, input: &str) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {program}: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| format!("{program} stdin is unavailable"))?
        .write_all(input.as_bytes())
        .map_err(|error| format!("cannot write {program} input: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("cannot wait for {program}: {error}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("{program} emitted non-UTF-8 output: {error}"))
    } else {
        Err(format!(
            "{} {} failed: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::ENGINE_REPOSITORY;

    const OLD: &str = "1111111111111111111111111111111111111111";
    const NEW: &str = "2222222222222222222222222222222222222222";

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = temporary_directory("revision-test").expect("temporary fixture");
            fs::write(
                root.join("engine-source.json"),
                format!(
                    "{{\n  \"schemaVersion\": 1,\n  \"repository\": \"{ENGINE_REPOSITORY}\",\n  \"commit\": \"{OLD}\",\n  \"studioDirectory\": \"studio\"\n}}\n"
                ),
            )
            .expect("source");
            let dependencies = ENGINE_CRATES
                .iter()
                .map(|name| {
                    format!("{name} = {{ git = \"{ENGINE_REPOSITORY}\", rev = \"{OLD}\" }}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[dependencies]\n{dependencies}\n"),
            )
            .expect("manifest");
            fs::write(root.join("Cargo.lock"), lockfile(OLD)).expect("lock");
            fs::write(root.join("history.md"), format!("historical pin {OLD}\n")).expect("history");
            run("git", &["init", "--quiet"], &root).expect("git init");
            run("git", &["add", "."], &root).expect("git add");
            run(
                "git",
                &[
                    "-c",
                    "user.name=Revision Test",
                    "-c",
                    "user.email=revision@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
                &root,
            )
            .expect("git commit");
            Self { root }
        }

        fn update(&self, target: &str, dry_run: bool) -> Result<UpdateReceipt, String> {
            update_engine_revision_with(
                &self.root,
                target,
                dry_run,
                |_, _| Ok(()),
                |candidate, previous, commit| {
                    let lock = fs::read_to_string(candidate.join("Cargo.lock"))
                        .map_err(|error| error.to_string())?;
                    fs::write(candidate.join("Cargo.lock"), lock.replace(previous, commit))
                        .map_err(|error| error.to_string())
                },
                |candidate| checked_provider_pin_at(candidate).map(|_| ()),
                |_| Ok(()),
            )
        }

        fn commit(&self, message: &str) {
            run(
                "git",
                &["add", "engine-source.json", "Cargo.toml", "Cargo.lock"],
                &self.root,
            )
            .expect("git add update");
            run(
                "git",
                &[
                    "-c",
                    "user.name=Revision Test",
                    "-c",
                    "user.email=revision@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    message,
                ],
                &self.root,
            )
            .expect("git commit update");
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn lockfile(commit: &str) -> String {
        ENGINE_CRATES
            .iter()
            .map(|name| {
                format!(
                    "[[package]]\nname = \"{name}\"\nsource = \"git+{ENGINE_REPOSITORY}?rev={commit}#{commit}\"\n"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dry_run_is_non_mutating_and_cleans_its_worktree() {
        let fixture = Fixture::new();
        let before = fs::read_to_string(fixture.root.join("engine-source.json")).expect("before");
        let receipt = fixture.update(NEW, true).expect("dry run");
        assert!(receipt.dry_run);
        assert!(receipt.diff.contains(NEW));
        assert_eq!(
            fs::read_to_string(fixture.root.join("engine-source.json")).expect("after"),
            before
        );
        assert_eq!(
            git(&fixture.root, &["worktree", "list", "--porcelain"])
                .expect("worktrees")
                .matches("worktree ")
                .count(),
            1
        );
    }

    #[test]
    fn update_and_rollback_touch_only_active_carriers() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("notes.txt"), "unrelated dirty work\n").expect("notes");
        fixture.update(NEW, false).expect("forward update");
        assert!(fs::read_to_string(fixture.root.join("engine-source.json"))
            .expect("source")
            .contains(NEW));
        assert_eq!(
            fs::read_to_string(fixture.root.join("history.md")).expect("history"),
            format!("historical pin {OLD}\n")
        );
        assert_eq!(
            fs::read_to_string(fixture.root.join("notes.txt")).expect("notes"),
            "unrelated dirty work\n"
        );
        fixture.commit("forward");
        fixture.update(OLD, false).expect("rollback");
        assert!(fs::read_to_string(fixture.root.join("engine-source.json"))
            .expect("source")
            .contains(OLD));
    }

    #[test]
    fn dirty_carrier_is_refused_without_disturbing_unrelated_work() {
        let fixture = Fixture::new();
        let manifest = fs::read_to_string(fixture.root.join("Cargo.toml")).expect("manifest");
        fs::write(
            fixture.root.join("Cargo.toml"),
            format!("{manifest}\n# dirty carrier\n"),
        )
        .expect("dirty carrier");
        fs::write(fixture.root.join("notes.txt"), "keep me\n").expect("notes");
        let error = fixture
            .update(NEW, false)
            .expect_err("dirty carrier should fail");
        assert!(error.contains("carrier or lock files are dirty"));
        assert_eq!(
            fs::read_to_string(fixture.root.join("notes.txt")).expect("notes"),
            "keep me\n"
        );
    }

    #[test]
    fn candidate_failure_is_cleaned_without_applying_changes() {
        let fixture = Fixture::new();
        let result = update_engine_revision_with(
            &fixture.root,
            NEW,
            false,
            |_, _| Ok(()),
            |_, _, _| Err("injected regeneration failure".to_owned()),
            |_| Ok(()),
            |_| Ok(()),
        );
        assert!(result
            .expect_err("failure should propagate")
            .contains("injected"));
        assert!(fs::read_to_string(fixture.root.join("engine-source.json"))
            .expect("source")
            .contains(OLD));
        assert_eq!(
            git(&fixture.root, &["worktree", "list", "--porcelain"])
                .expect("worktrees")
                .matches("worktree ")
                .count(),
            1
        );
    }

    #[test]
    fn caller_head_and_carrier_races_are_refused() {
        let fixture = Fixture::new();
        let head_race = update_engine_revision_with(
            &fixture.root,
            NEW,
            false,
            |_, _| Ok(()),
            |candidate, previous, commit| {
                let lock = fs::read_to_string(candidate.join("Cargo.lock"))
                    .map_err(|error| error.to_string())?;
                fs::write(candidate.join("Cargo.lock"), lock.replace(previous, commit))
                    .map_err(|error| error.to_string())
            },
            |candidate| checked_provider_pin_at(candidate).map(|_| ()),
            |root| {
                fs::write(root.join("race.txt"), "race\n").map_err(|error| error.to_string())?;
                run("git", &["add", "race.txt"], root)?;
                run(
                    "git",
                    &[
                        "-c",
                        "user.name=Revision Test",
                        "-c",
                        "user.email=revision@example.invalid",
                        "commit",
                        "--quiet",
                        "-m",
                        "race",
                    ],
                    root,
                )?;
                Ok(())
            },
        );
        assert!(head_race.expect_err("HEAD race").contains("HEAD changed"));

        let carrier_race = update_engine_revision_with(
            &fixture.root,
            NEW,
            false,
            |_, _| Ok(()),
            |candidate, previous, commit| {
                let lock = fs::read_to_string(candidate.join("Cargo.lock"))
                    .map_err(|error| error.to_string())?;
                fs::write(candidate.join("Cargo.lock"), lock.replace(previous, commit))
                    .map_err(|error| error.to_string())
            },
            |candidate| checked_provider_pin_at(candidate).map(|_| ()),
            |root| fs::write(root.join("engine-source.json"), "race\n").map_err(|e| e.to_string()),
        );
        assert!(carrier_race
            .expect_err("carrier race")
            .contains("carrier or lock files are dirty"));
    }
}
