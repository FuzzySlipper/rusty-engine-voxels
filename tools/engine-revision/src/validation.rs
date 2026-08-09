use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const ENGINE_REPOSITORY: &str = "https://github.com/FuzzySlipper/rusty-engine";
pub const ENGINE_PACKAGE: &str = "rusty-engine";
pub const ACTIVE_CARRIER_PATHS: [&str; 3] = ["engine-source.json", "Cargo.toml", "Cargo.lock"];
pub const DEVELOPMENT_MANIFEST: &str = "engine-development.json";
pub const DEVELOPMENT_REF: &str = "refs/heads/main";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineSource {
    pub schema_version: u32,
    pub repository: String,
    pub commit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub studio_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPinReadout {
    pub repository: String,
    pub commit: String,
    pub manifest_dependency_count: usize,
    pub locked_provider_package_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineDevelopment {
    pub schema_version: u32,
    pub repository: String,
    pub r#ref: String,
}

/// Decodes the explicit rolling-development intent. It deliberately admits
/// only the public Engine main branch; exact commits remain the certification
/// carrier and are resolved by the consumer command once per sync.
///
/// # Errors
///
/// Returns an error when the manifest is malformed or names anything other
/// than the canonical public Engine main branch.
pub fn parse_engine_development(value: &str) -> Result<EngineDevelopment, String> {
    let source: EngineDevelopment = serde_json::from_str(value)
        .map_err(|error| format!("{DEVELOPMENT_MANIFEST} cannot be decoded: {error}"))?;
    if source.schema_version != 1
        || source.repository != ENGINE_REPOSITORY
        || source.r#ref != DEVELOPMENT_REF
    {
        return Err(format!(
            "{DEVELOPMENT_MANIFEST} must select {ENGINE_REPOSITORY} {DEVELOPMENT_REF}"
        ));
    }
    Ok(source)
}

/// Reads and validates every active Engine carrier below `repo_root`.
///
/// # Errors
///
/// Returns an error when a carrier cannot be read or the carriers are incoherent.
pub fn checked_provider_pin_at(repo_root: &Path) -> Result<ProviderPinReadout, String> {
    let source = read(repo_root, "engine-source.json")?;
    let manifest = read(repo_root, "Cargo.toml")?;
    let lockfile = read(repo_root, "Cargo.lock")?;
    validate_provider_pin(&source, &manifest, &lockfile)
}

/// Decodes the consumer-owned canonical Engine source manifest.
///
/// # Errors
///
/// Returns an error for malformed, extended, non-public, floating, or non-voxel
/// manifests.
pub fn parse_engine_source(value: &str) -> Result<EngineSource, String> {
    let source: EngineSource = serde_json::from_str(value)
        .map_err(|error| format!("engine-source.json cannot be decoded: {error}"))?;
    if source.schema_version != 1
        || source.repository != ENGINE_REPOSITORY
        || !is_commit(&source.commit)
    {
        return Err(
            "engine-source.json does not name one supported exact public provider".to_owned(),
        );
    }
    if source.studio_directory.as_deref() != Some("studio") {
        return Err("engine-source.json must retain the voxel Studio extension".to_owned());
    }
    reject_sibling_topology(value)?;
    Ok(source)
}

/// Validates the canonical source manifest against all Cargo projections.
///
/// # Errors
///
/// Returns an error when any direct dependency or locked Engine package is
/// missing, duplicated, floating, non-canonical, or pinned to another commit.
pub fn validate_provider_pin(
    engine_source_json: &str,
    manifest: &str,
    lockfile: &str,
) -> Result<ProviderPinReadout, String> {
    let source = parse_engine_source(engine_source_json)?;
    reject_sibling_topology(manifest)?;
    let manifest_dependency_count = validate_manifest(manifest, &source)?;
    let locked_provider_package_count = validate_lockfile(lockfile, &source)?;
    Ok(ProviderPinReadout {
        repository: source.repository,
        commit: source.commit,
        manifest_dependency_count,
        locked_provider_package_count,
    })
}

fn validate_manifest(manifest: &str, _source: &EngineSource) -> Result<usize, String> {
    let dependencies = section(manifest, "dependencies")
        .ok_or("Cargo.toml is missing its dependencies section")?;
    let expected =
        format!("{ENGINE_PACKAGE} = {{ git = \"{ENGINE_REPOSITORY}\", branch = \"main\" }}");
    let matches = dependencies
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(&format!("{ENGINE_PACKAGE} =")))
        .collect::<Vec<_>>();
    if matches.len() != 1 || matches[0] != expected {
        return Err(format!(
            "Cargo.toml must declare exactly one canonical rolling {ENGINE_PACKAGE} facade dependency"
        ));
    }

    for line in dependencies.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        let aliases_engine_package = lower.contains("package = \"rusty-engine\"");
        let references_engine_repository = lower.contains("fuzzyslipper/rusty-engine");
        if (aliases_engine_package || references_engine_repository) && line != expected {
            return Err(format!(
                "Cargo.toml contains an unexpected Engine dependency carrier: {line}"
            ));
        }
    }
    for line in manifest.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        let aliases_engine_package = lower.contains("package = \"rusty-engine\"");
        let references_engine_repository = lower.contains("fuzzyslipper/rusty-engine");
        if (aliases_engine_package || references_engine_repository) && line != expected {
            return Err(format!(
                "Cargo.toml contains an Engine carrier outside the closed dependency set: {line}"
            ));
        }
    }
    Ok(1)
}

fn validate_lockfile(lockfile: &str, source: &EngineSource) -> Result<usize, String> {
    let expected_source = format!("git+{ENGINE_REPOSITORY}?branch=main#{}", source.commit);
    let blocks = lockfile.split("[[package]]").skip(1).collect::<Vec<_>>();
    let name_line = format!("name = \"{ENGINE_PACKAGE}\"");
    let matches = blocks
        .iter()
        .filter(|block| block.lines().any(|line| line.trim() == name_line))
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "Cargo.lock expected exactly one package block for {ENGINE_PACKAGE}; observed {}",
            matches.len()
        ));
    }
    let observed = matches[0].lines().map(str::trim).find_map(|line| {
        line.strip_prefix("source = \"")
            .and_then(|v| v.strip_suffix('"'))
    });
    if observed != Some(expected_source.as_str()) {
        return Err(format!(
            "Cargo.lock {ENGINE_PACKAGE} does not resolve the canonical rolling Engine source at the selected exact commit"
        ));
    }

    let provider_sources = lockfile
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            line.strip_prefix("source = \"")
                .and_then(|v| v.strip_suffix('"'))
        })
        .filter(|source_line| source_line.to_ascii_lowercase().contains("rusty-engine"))
        .collect::<Vec<_>>();
    if provider_sources.is_empty() {
        return Err("Cargo.lock is missing locked Engine sources".to_owned());
    }
    if provider_sources
        .iter()
        .any(|observed| *observed != expected_source)
    {
        return Err("Cargo.lock contains a non-canonical or mixed Engine source".to_owned());
    }
    Ok(provider_sources.len())
}

fn section<'a>(manifest: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("[{name}]");
    let start = manifest.find(&marker)? + marker.len();
    let remainder = &manifest[start..];
    let end = remainder.find("\n[").unwrap_or(remainder.len());
    Some(&remainder[..end])
}

fn reject_sibling_topology(value: &str) -> Result<(), String> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("../rusty-engine")
        || lower.contains("/home/dev/rusty-engine")
        || (lower.contains("file://") && lower.contains("rusty-engine"))
    {
        Err("provider configuration contains a sibling or filesystem checkout path".to_owned())
    } else {
        Ok(())
    }
}

fn is_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read(repo_root: &Path, relative: &str) -> Result<String, String> {
    fs::read_to_string(repo_root.join(relative))
        .map_err(|error| format!("{relative} cannot be read: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD: &str = "1111111111111111111111111111111111111111";

    fn source(commit: &str) -> String {
        format!(
            "{{\"schemaVersion\":1,\"repository\":\"{ENGINE_REPOSITORY}\",\"commit\":\"{commit}\",\"studioDirectory\":\"studio\"}}"
        )
    }

    fn manifest() -> String {
        format!(
            "[package]\nname = \"consumer\"\n\n[dependencies]\n{ENGINE_PACKAGE} = {{ git = \"{ENGINE_REPOSITORY}\", branch = \"main\" }}\n"
        )
    }

    fn lockfile(commit: &str) -> String {
        format!(
            "[[package]]\nname = \"{ENGINE_PACKAGE}\"\nsource = \"git+{ENGINE_REPOSITORY}?branch=main#{commit}\"\n\n[[package]]\nname = \"render-model\"\nsource = \"git+{ENGINE_REPOSITORY}?branch=main#{commit}\"\n"
        )
    }

    #[test]
    fn accepts_one_complete_rolling_facade_at_one_exact_resolution() {
        let readout = validate_provider_pin(&source(OLD), &manifest(), &lockfile(OLD))
            .expect("coherent pin should pass");
        assert_eq!(readout.commit, OLD);
        assert_eq!(readout.manifest_dependency_count, 1);
        assert_eq!(readout.locked_provider_package_count, 2);
    }

    #[test]
    fn accepts_only_the_public_main_development_intent() {
        let value = format!(
            "{{\"schemaVersion\":1,\"repository\":\"{ENGINE_REPOSITORY}\",\"ref\":\"{DEVELOPMENT_REF}\"}}"
        );
        let source = parse_engine_development(&value).expect("development intent should pass");
        assert_eq!(source.r#ref, DEVELOPMENT_REF);
        assert!(parse_engine_development(&value.replace(DEVELOPMENT_REF, "main")).is_err());
        assert!(parse_engine_development(
            &value.replace(ENGINE_REPOSITORY, "https://example.invalid/rusty-engine")
        )
        .is_err());
    }

    #[test]
    fn rejects_schema_source_and_commit_drift() {
        let mixed_case = "abcdefabcdefabcdefabcdefabcdefabcdefabcd".to_ascii_uppercase();
        assert!(parse_engine_source(&source(&mixed_case)).is_err());
        assert!(parse_engine_source(&source(OLD).replace("\"studio\"", "\"web\"")).is_err());
        assert!(parse_engine_source(
            &source(OLD).replace(ENGINE_REPOSITORY, "https://github.com/other/rusty-engine")
        )
        .is_err());
        assert!(parse_engine_source(&format!(
            "{{\"schemaVersion\":1,\"repository\":\"{ENGINE_REPOSITORY}\",\"commit\":\"{OLD}\",\"studioDirectory\":\"studio\",\"branch\":\"main\"}}"
        ))
        .is_err());
    }

    #[test]
    fn rejects_missing_floating_aliased_and_sibling_manifest_carriers() {
        let base = manifest();
        assert!(validate_provider_pin(
            &source(OLD),
            &base.replace("rusty-engine =", "missing-rusty-engine ="),
            &lockfile(OLD)
        )
        .is_err());
        assert!(validate_provider_pin(
            &source(OLD),
            &base.replace("branch = \"main\"", &format!("rev = \"{OLD}\"")),
            &lockfile(OLD)
        )
        .is_err());
        assert!(validate_provider_pin(
            &source(OLD),
            &base.replace(
                "rusty-engine =",
                "engine = { package = \"rusty-engine\", path = \"../rusty-engine/rust/crates/rusty-engine\" } #"
            ),
            &lockfile(OLD)
        )
        .is_err());
        assert!(validate_provider_pin(
            &source(OLD),
            &format!(
                "{base}\n[dev-dependencies]\nextra = {{ git = \"{ENGINE_REPOSITORY}\", branch = \"main\" }}\n"
            ),
            &lockfile(OLD)
        )
        .is_err());
    }

    #[test]
    fn rejects_missing_duplicate_stale_and_noncanonical_lock_blocks() {
        let lock = lockfile(OLD);
        let first = format!(
            "[[package]]\nname = \"{ENGINE_PACKAGE}\"\nsource = \"git+{ENGINE_REPOSITORY}?branch=main#{OLD}\"\n"
        );
        assert!(
            validate_provider_pin(&source(OLD), &manifest(), &lock.replace(&first, "")).is_err()
        );
        assert!(
            validate_provider_pin(&source(OLD), &manifest(), &format!("{lock}\n{first}")).is_err()
        );
        assert!(validate_provider_pin(
            &source(OLD),
            &manifest(),
            &lock.replace(OLD, "2222222222222222222222222222222222222222")
        )
        .is_err());
        assert!(validate_provider_pin(
            &source(OLD),
            &manifest(),
            &lock.replace("FuzzySlipper", "fuzzyslipper")
        )
        .is_err());
    }
}
