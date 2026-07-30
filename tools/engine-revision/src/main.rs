use std::path::Path;

use rusty_engine_voxels_revision::{checked_provider_pin_at, update_engine_revision};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let repo_root = std::env::current_dir().map_err(|error| error.to_string())?;
    match arguments.as_slice() {
        [command] if command == "check" => {
            let readout = checked_provider_pin_at(&repo_root)?;
            println!(
                "Engine revision {} is coherent across {} manifest dependencies and {} locked provider packages.",
                readout.commit,
                readout.manifest_dependency_count,
                readout.locked_provider_package_count
            );
            Ok(())
        }
        [command, commit] if command == "update" => update(&repo_root, commit, false),
        [command, commit, flag] if command == "update" && flag == "--dry-run" => {
            update(&repo_root, commit, true)
        }
        [command, flag, commit] if command == "update" && flag == "--dry-run" => {
            update(&repo_root, commit, true)
        }
        _ => Err(
            "usage: ./scripts/engine-revision check | update <40-character-sha> [--dry-run]"
                .to_owned(),
        ),
    }
}

fn update(repo_root: &Path, commit: &str, dry_run: bool) -> Result<(), String> {
    let receipt = update_engine_revision(repo_root, commit, dry_run)?;
    if receipt.dry_run {
        println!(
            "Dry run: Engine {} -> {} would apply:\n{}",
            receipt.previous_commit, receipt.commit, receipt.diff
        );
    } else {
        println!(
            "Updated Engine {} -> {}. Review and commit the three active carrier changes.",
            receipt.previous_commit, receipt.commit
        );
    }
    Ok(())
}
