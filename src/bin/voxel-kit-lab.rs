use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use rusty_engine_voxels::directional::inspect_layout;
use rusty_engine_voxels::kit_bake::{run_kit_bake, write_kit_bake_output};
use rusty_engine_voxels::posed::{run_posed_flipbook, write_posed_flipbook_report};

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
    let mut action = None;
    let mut frame = None;
    let mut comparison = None;
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--root" => root = PathBuf::from(value),
            "--spec" => spec = Some(value),
            "--out" => out = Some(value),
            "--report" => report = Some(value),
            "--action" => action = Some(value),
            "--frame" => frame = Some(value),
            "--comparison" => comparison = Some(value),
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
        "poses" => {
            let spec = spec.ok_or("poses requires --spec PATH")?;
            let out = out.unwrap_or_else(|| "content/voxel-objects".to_owned());
            let output = run_posed_flipbook(&root, &spec, &out)?;
            if let Some(report) = report {
                write_posed_flipbook_report(&root, &report, &output)?;
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&output.evidence).map_err(|error| error.to_string())?
            );
        }
        "sprite-inspect" => {
            let spec = spec.ok_or("sprite-inspect requires --spec PATH")?;
            let out = out.unwrap_or_else(|| "local/directional-sprite-inspection".to_owned());
            let result = inspect_layout(
                &root,
                &spec,
                &out,
                action.as_deref(),
                frame.as_deref(),
                comparison.as_deref(),
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&result.normalized).map_err(|error| error.to_string())?
            );
        }
        _ => {
            return Err(
                "usage: voxel-kit-lab <bake|poses|sprite-inspect> --spec PATH [--out PATH --report PATH --action ID --frame ID --comparison PATH] [--root PATH]"
                    .to_owned(),
            )
        }
    }
    Ok(())
}
