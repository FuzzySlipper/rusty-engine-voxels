use std::path::Path;

use rusty_engine_voxels_revision::{
    check_development_resolution, checked_provider_pin_at, sync_development_revision,
    update_engine_revision,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let repo_root = std::env::current_dir().map_err(|error| error.to_string())?;
    if arguments.len() >= 2 && arguments[0] == "dev" && arguments[1] == "sync" {
        return development_sync_arguments(&repo_root, &arguments[2..]);
    }
    match arguments.as_slice() {
        [command] if command == "check" || command == "certify" => {
            if command == "certify" {
                return Err(certification_usage());
            }
            let readout = checked_provider_pin_at(&repo_root)?;
            println!(
                "Engine revision {} is coherent across {} manifest dependencies and {} locked provider packages.",
                readout.commit,
                readout.manifest_dependency_count,
                readout.locked_provider_package_count
            );
            Ok(())
        }
        [command, subcommand] if command == "certify" && subcommand == "check" => {
            let readout = checked_provider_pin_at(&repo_root)?;
            println!(
                "Engine revision {} is coherent across {} manifest dependencies and {} locked provider packages.",
                readout.commit,
                readout.manifest_dependency_count,
                readout.locked_provider_package_count
            );
            Ok(())
        }
        [command, subcommand] if command == "dev" && subcommand == "check" => {
            let report = check_development_resolution(&repo_root)?;
            println!(
                "Development resolution check passed: {} -> {} ({}{}).",
                report.requested_ref,
                report.resolved_commit,
                report.source,
                if report.dirty { ", dirty" } else { "" }
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
        [command, subcommand, commit] if command == "certify" && subcommand == "update" => {
            update(&repo_root, commit, false)
        }
        [command, subcommand, commit, flag]
            if command == "certify" && subcommand == "update" && flag == "--dry-run" =>
        {
            update(&repo_root, commit, true)
        }
        _ => Err(format!(
            "usage: ./scripts/engine-revision check | certify check | update <40-character-sha> [--dry-run] | certify update <40-character-sha> [--dry-run] | {}",
            development_usage()
        )),
    }
}

fn development_sync_arguments(repo_root: &Path, arguments: &[String]) -> Result<(), String> {
    let mut worktree = None;
    let mut report_only = false;
    let mut json = false;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--report-only" => report_only = true,
            "--json" => json = true,
            "--worktree" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(development_usage)?;
                worktree = Some(Path::new(value));
            }
            _ => return Err(development_usage()),
        }
        index += 1;
    }
    development_sync(repo_root, worktree, report_only, json)
}

fn development_sync(
    repo_root: &Path,
    worktree: Option<&Path>,
    report_only: bool,
    json: bool,
) -> Result<(), String> {
    let receipt = sync_development_revision(repo_root, worktree, report_only)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&receipt)
                .map_err(|error| format!("development receipt cannot be encoded: {error}"))?
        );
    } else {
        println!(
            "Development Engine {} resolved to {} ({}{}).",
            receipt.requested_ref,
            receipt.resolved_commit,
            receipt.source,
            if receipt.dirty { ", dirty" } else { "" }
        );
        if report_only {
            println!("Report only: active certification carriers were not changed.");
        } else {
            println!(
                "Active carriers were refreshed as one coherent development resolution; this is not certification evidence."
            );
        }
    }
    Ok(())
}

fn development_usage() -> String {
    "dev check | dev sync [--worktree /absolute/engine-root] [--report-only] [--json]".to_owned()
}

fn certification_usage() -> String {
    "usage: ./scripts/engine-revision certify check | certify update <40-character-sha> [--dry-run]"
        .to_owned()
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
