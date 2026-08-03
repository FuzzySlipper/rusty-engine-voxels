use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use rusty_engine_voxels::density::{run_density_experiment, write_density_evidence};

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
    let command = arguments.next().unwrap_or_else(|| "run".to_owned());
    let mut root = env::current_dir().map_err(|error| error.to_string())?;
    let mut spec = None;
    let mut report = None;
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--root" => root = PathBuf::from(value),
            "--spec" => spec = Some(value),
            "--report" => report = Some(value),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    match command.as_str() {
        "run" => {
            let spec = spec.ok_or("run requires --spec PATH")?;
            let evidence = run_density_experiment(&root, &spec)?;
            if let Some(report) = report {
                write_density_evidence(&root, &report, &evidence)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&evidence).map_err(|error| error.to_string())?
            );
        }
        _ => {
            return Err(
                "usage: voxel-density-lab run --spec PATH [--report PATH] [--root PATH]".to_owned(),
            )
        }
    }
    Ok(())
}
