use std::fs;
use std::path::PathBuf;

use rusty_engine_voxels::project::atomic_write;
use rusty_engine_voxels::surface_evidence::build_textured_voxel_report;

fn main() -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture_path = root.join("content/textures/directional-atlas.png");
    let output_path = root.join("evidence/textured-voxel-surfaces.json");
    let report = build_textured_voxel_report(
        &fs::read(&fixture_path).map_err(|error| format!("{}: {error}", fixture_path.display()))?,
    )?;
    let bytes = format!(
        "{}\n",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    )
    .into_bytes();
    if std::env::args().any(|argument| argument == "--check") {
        let existing = fs::read(&output_path)
            .map_err(|error| format!("{}: {error}", output_path.display()))?;
        if existing != bytes {
            return Err("textured voxel evidence is stale; regenerate it".to_owned());
        }
    } else {
        atomic_write(&output_path, &bytes)?;
    }
    Ok(())
}
