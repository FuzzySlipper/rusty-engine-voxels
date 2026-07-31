use std::path::PathBuf;

use rusty_engine_voxels::video_motion::fit_multiview_landmarks_json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let source = PathBuf::from(arguments.next().ok_or("missing landmark source path")?);
    let output = PathBuf::from(arguments.next().ok_or("missing fitted output path")?);
    if arguments.next().is_some() {
        return Err("expected exactly <landmarks.json> <fitted-motion.json>".into());
    }
    let source_json = std::fs::read_to_string(&source)?;
    let fitted = fit_multiview_landmarks_json(&source_json)?;
    let encoded = serde_json::to_string_pretty(&fitted)? + "\n";
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, encoded)?;
    Ok(())
}
