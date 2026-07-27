use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::model::VoxelLabProject;

pub const MAX_PROJECT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct LoadedProject {
    pub root: PathBuf,
    pub relative_path: String,
    pub path: PathBuf,
    pub canonical_json: String,
    pub project_hash: String,
    pub project: VoxelLabProject,
}

pub fn load_project(root: &Path, relative_path: &str) -> Result<LoadedProject, String> {
    if !root.is_absolute() {
        return Err("project root must be absolute".to_owned());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", root.display()))?;
    let path = safe_join(&root, relative_path)?;
    reject_symlink(&path)?;
    let canonical_json = read_bounded_text(&path, MAX_PROJECT_BYTES, "project")?;
    let project: VoxelLabProject = serde_json::from_str(&canonical_json)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    project.validate()?;
    Ok(LoadedProject {
        root,
        relative_path: relative_path.to_owned(),
        path,
        project_hash: sha256(canonical_json.as_bytes()),
        canonical_json,
        project,
    })
}

pub fn save_project(
    loaded: &LoadedProject,
    project: &VoxelLabProject,
) -> Result<LoadedProject, String> {
    project.validate()?;
    let canonical_json = format!(
        "{}\n",
        serde_json::to_string_pretty(project).map_err(|error| error.to_string())?
    );
    atomic_write(&loaded.path, canonical_json.as_bytes())?;
    load_project(&loaded.root, &loaded.relative_path)
}

pub fn safe_join(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe project-relative path {relative:?}"));
    }
    Ok(root.join(path))
}

pub fn read_bounded(path: &Path, max_bytes: u64, kind: &str) -> Result<Vec<u8>, String> {
    reject_symlink(path)?;
    let metadata = fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular {kind} file", path.display()));
    }
    if metadata.len() > max_bytes {
        return Err(format!(
            "{} exceeds the {max_bytes}-byte {kind} limit",
            path.display()
        ));
    }
    fs::read(path).map_err(|error| format!("{}: {error}", path.display()))
}

pub fn read_bounded_text(path: &Path, max_bytes: u64, kind: &str) -> Result<String, String> {
    String::from_utf8(read_bounded(path, max_bytes, kind)?)
        .map_err(|_| format!("{} is not UTF-8 {kind} content", path.display()))
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    if path.exists() {
        reject_symlink(path)?;
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", path.display()))?;
    let pending = parent.join(format!(".{file_name}.pending"));
    if pending.exists() {
        return Err(format!(
            "stale pending artifact blocks publication: {}",
            pending.display()
        ));
    }
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&pending)
            .map_err(|error| format!("{}: {error}", pending.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("{}: {error}", pending.display()))?;
        fs::rename(&pending, path).map_err(|error| format!("{}: {error}", path.display()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("{}: {error}", parent.display()))
    })();
    if result.is_err() && pending.exists() {
        let _ = fs::remove_file(&pending);
    }
    result
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn reject_symlink(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        Err(format!(
            "symbolic links are not accepted: {}",
            path.display()
        ))
    } else {
        Ok(())
    }
}
