use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use render_model::{PackedMeshResource, MAX_MESH_RESOURCE_BYTES};
use rusty_engine::render_model;
use serde::Serialize;

use crate::project::{read_bounded, safe_join};

const CACHE_DIRECTORY: &str = ".studio-cache/render-resources";
const MAX_RESOURCE_COUNT: usize = 1_024;
const MAX_AGGREGATE_BYTES: u64 = 256 * 1024 * 1024;
static PENDING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MeshResourceReadout {
    pub resource: String,
    pub content_hash: String,
    pub byte_length: u32,
    pub source_path: String,
}

pub(crate) fn publish_mesh_resources(
    project_root: &Path,
    resources: Vec<PackedMeshResource>,
) -> Result<Vec<MeshResourceReadout>, String> {
    if resources.len() > MAX_RESOURCE_COUNT {
        return Err(format!(
            "mesh resource set has {} entries; maximum is {MAX_RESOURCE_COUNT}",
            resources.len()
        ));
    }
    if resources.is_empty() {
        return Ok(Vec::new());
    }
    let cache = safe_join(project_root, CACHE_DIRECTORY)?;
    fs::create_dir_all(&cache).map_err(|error| format!("{}: {error}", cache.display()))?;
    let canonical_root = project_root
        .canonicalize()
        .map_err(|error| format!("{}: {error}", project_root.display()))?;
    let canonical_cache = cache
        .canonicalize()
        .map_err(|error| format!("{}: {error}", cache.display()))?;
    if !canonical_cache.starts_with(&canonical_root) {
        return Err(format!(
            "mesh resource cache escapes project root: {}",
            cache.display()
        ));
    }

    let mut aggregate_bytes = 0_u64;
    let mut unique = BTreeMap::<String, PackedMeshResource>::new();
    for resource in resources {
        resource
            .validate()
            .map_err(|error| format!("mesh resource rejected: {error:?}"))?;
        aggregate_bytes = aggregate_bytes.saturating_add(u64::from(resource.byte_length()));
        if aggregate_bytes > MAX_AGGREGATE_BYTES {
            return Err(format!(
                "mesh resource set exceeds the {MAX_AGGREGATE_BYTES}-byte Studio bound"
            ));
        }
        if let Some(existing) = unique.insert(resource.resource.clone(), resource.clone()) {
            if existing != resource {
                return Err(format!(
                    "mesh resource identity {} has conflicting bytes",
                    resource.resource
                ));
            }
        }
    }

    unique
        .into_values()
        .map(|resource| {
            let digest = resource
                .resource
                .strip_prefix("mesh-resource/")
                .ok_or_else(|| "mesh resource identity lost its prefix".to_owned())?;
            let source_path = format!("{CACHE_DIRECTORY}/{digest}.rmesh");
            let path = safe_join(project_root, &source_path)?;
            let existing = fs::symlink_metadata(&path)
                .ok()
                .map(|_| read_bounded(&path, u64::from(MAX_MESH_RESOURCE_BYTES), "mesh resource"))
                .transpose()?;
            if existing.as_deref() != Some(resource.bytes.as_slice()) {
                atomic_cache_write(&path, &resource.bytes)?;
            }
            let byte_length = resource.byte_length();
            Ok(MeshResourceReadout {
                resource: resource.resource,
                content_hash: resource.content_hash,
                byte_length,
                source_path,
            })
        })
        .collect()
}

fn atomic_cache_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| format!("{} has no UTF-8 file name", path.display()))?;
    let sequence = PENDING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let pending = parent.join(format!(
        ".{file_name}.{}.{}.pending",
        std::process::id(),
        sequence
    ));
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
    if result.is_err() {
        let _ = fs::remove_file(&pending);
    }
    result
}

pub(crate) fn merge_mesh_resource_readouts(
    current: &mut Vec<MeshResourceReadout>,
    additions: Vec<MeshResourceReadout>,
) -> Result<(), String> {
    let mut merged = current
        .iter()
        .cloned()
        .map(|readout| (readout.resource.clone(), readout))
        .collect::<BTreeMap<_, _>>();
    for addition in additions {
        if let Some(existing) = merged.get(&addition.resource) {
            if existing != &addition {
                return Err(format!(
                    "mesh resource identity {} has conflicting publication readouts",
                    addition.resource
                ));
            }
        } else {
            merged.insert(addition.resource.clone(), addition);
        }
    }
    *current = merged.into_values().collect();
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::time::{SystemTime, UNIX_EPOCH};

    use render_model::{mesh_resource_content_hash, PackedMeshResource, MESH_RESOURCE_MAGIC};

    use super::*;

    #[test]
    fn publication_is_content_addressed_idempotent_and_repairs_corrupt_cache_bytes() {
        let root = temporary_root("repair");
        fs::create_dir_all(&root).expect("create test project root");
        let resource = fixture_resource();

        let first =
            publish_mesh_resources(&root, vec![resource.clone()]).expect("publish resource");
        let path = safe_join(&root, &first[0].source_path).expect("safe cache path");
        assert_eq!(fs::read(&path).expect("read cache"), resource.bytes);
        fs::write(&path, b"corrupt").expect("corrupt cache");

        let second =
            publish_mesh_resources(&root, vec![resource.clone()]).expect("repair resource");

        assert_eq!(first, second);
        assert_eq!(fs::read(path).expect("read repaired cache"), resource.bytes);
        fs::remove_dir_all(root).expect("remove test project root");
    }

    #[test]
    fn concurrent_publication_uses_independent_pending_files() {
        const WRITERS: usize = 8;
        let root = Arc::new(temporary_root("concurrent"));
        fs::create_dir_all(root.as_ref()).expect("create test project root");
        let resource = fixture_resource();
        let barrier = Arc::new(Barrier::new(WRITERS));
        let writers = (0..WRITERS)
            .map(|_| {
                let root = Arc::clone(&root);
                let barrier = Arc::clone(&barrier);
                let resource = resource.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    publish_mesh_resources(root.as_ref(), vec![resource])
                })
            })
            .collect::<Vec<_>>();

        let manifests = writers
            .into_iter()
            .map(|writer| writer.join().expect("publisher thread should finish"))
            .collect::<Result<Vec<_>, _>>()
            .expect("concurrent publishers should all succeed");

        assert!(manifests.windows(2).all(|pair| pair[0] == pair[1]));
        fs::remove_dir_all(root.as_ref()).expect("remove test project root");
    }

    fn temporary_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "rusty-engine-voxels-mesh-resource-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn fixture_resource() -> PackedMeshResource {
        let mut bytes = vec![0_u8; 16];
        bytes[..8].copy_from_slice(&MESH_RESOURCE_MAGIC);
        bytes[8..12].copy_from_slice(&16_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&1_u32.to_le_bytes());
        let content_hash = mesh_resource_content_hash(&bytes);
        PackedMeshResource {
            resource: format!("mesh-resource/{}", &content_hash["sha256:".len()..]),
            content_hash,
            bytes,
        }
    }
}
