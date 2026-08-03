use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use rusty_engine_voxels::kit_bake::{run_kit_bake, write_kit_bake_output};

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
    let command = arguments.next().unwrap_or_else(|| "bake".to_owned());
    let mut root = env::current_dir().map_err(|error| error.to_string())?;
    let mut spec = None;
    let mut out = None;
    let mut report = None;
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--root" => root = PathBuf::from(value),
            "--spec" => spec = Some(value),
            "--out" => out = Some(value),
            "--report" => report = Some(value),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    match command.as_str() {
        "bake" => {
            let spec = spec.ok_or("bake requires --spec PATH")?;
            let output = run_kit_bake(&root, &spec)?;
            if let (Some(out), Some(report)) = (out, report) {
                write_kit_bake_output(&root, &out, &report, &output)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&output.evidence).map_err(|error| error.to_string())?
            );
        }
        _ => {
            return Err(
                "usage: voxel-kit-lab bake --spec PATH [--out PATH --report PATH] [--root PATH]"
                    .to_owned(),
            )
        }
    }
    Ok(())
}
