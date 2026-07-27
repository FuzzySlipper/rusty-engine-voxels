use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rusty_engine_voxels::conversion::{convert_project, report_path, ConversionEvidence};
use rusty_engine_voxels::project::atomic_write;
use rusty_engine_voxels::runtime::{verify_runtime_project, RuntimeEvidence};
use rusty_engine_voxels::DEFAULT_PROJECT_FILE;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExperimentEvidence {
    conversion: ConversionEvidence,
    runtime: RuntimeEvidence,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| "verify".to_owned());
    let mut root = env::current_dir().map_err(|error| error.to_string())?;
    let mut project = DEFAULT_PROJECT_FILE.to_owned();
    let mut report = None;
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--root" => root = PathBuf::from(value),
            "--project" => project = value,
            "--report" => report = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    if !root.is_absolute() {
        root = root.canonicalize().map_err(|error| error.to_string())?;
    }
    match command.as_str() {
        "convert" => {
            let conversion = convert_project(&root, &project)?;
            print_json(&conversion)?;
        }
        "load" => {
            let runtime = verify_runtime_project(&root, &project)?;
            print_json(&runtime)?;
        }
        "verify" => {
            let conversion = convert_project(&root, &project)?;
            let runtime = verify_runtime_project(&root, &project)?;
            let evidence = ExperimentEvidence {
                conversion,
                runtime,
            };
            let path = report.unwrap_or_else(|| report_path(&root));
            write_json(&path, &evidence)?;
            print_json(&evidence)?;
        }
        _ => return Err(
            "usage: voxel-lab [convert|load|verify] [--root PATH] [--project PATH] [--report PATH]"
                .to_owned(),
        ),
    }
    Ok(())
}

fn print_json(value: &impl Serialize) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    atomic_write(path, bytes.as_bytes())
}
