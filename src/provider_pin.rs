use serde::{Deserialize, Serialize};

use crate::conversion::ENGINE_REVISION;

const ENGINE_REPOSITORY: &str = "https://github.com/FuzzySlipper/rusty-engine";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPinReadout {
    pub repository: String,
    pub commit: String,
    pub manifest_dependency_count: usize,
    pub locked_provider_package_count: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EngineSource {
    schema_version: u32,
    public_repository: String,
    commit: String,
    studio_directory: String,
}

pub fn checked_provider_pin() -> Result<ProviderPinReadout, String> {
    validate_provider_pin(
        include_str!("../engine-source.json"),
        include_str!("../Cargo.toml"),
        include_str!("../Cargo.lock"),
    )
}

pub fn validate_provider_pin(
    engine_source_json: &str,
    manifest: &str,
    lockfile: &str,
) -> Result<ProviderPinReadout, String> {
    let source: EngineSource =
        serde_json::from_str(engine_source_json).map_err(|error| error.to_string())?;
    if source.schema_version != 1
        || source.public_repository != ENGINE_REPOSITORY
        || source.studio_directory != "studio"
        || !is_commit(&source.commit)
    {
        return Err(
            "engine-source.json does not name one supported exact public provider".to_owned(),
        );
    }
    if source.commit != ENGINE_REVISION {
        return Err(format!(
            "engine source {} does not match compiled provider revision {ENGINE_REVISION}",
            source.commit
        ));
    }
    reject_sibling_topology(engine_source_json)?;
    reject_sibling_topology(manifest)?;
    let dependency_lines = manifest
        .lines()
        .filter(|line| line.contains(ENGINE_REPOSITORY))
        .collect::<Vec<_>>();
    if dependency_lines.is_empty()
        || dependency_lines
            .iter()
            .any(|line| !line.contains(&format!("rev = \"{}\"", source.commit)))
    {
        return Err(
            "Cargo.toml provider dependencies do not share the exact Engine pin".to_owned(),
        );
    }
    let locked_sources = lockfile
        .lines()
        .filter(|line| line.contains("git+https://github.com/FuzzySlipper/rusty-engine?rev="))
        .collect::<Vec<_>>();
    if locked_sources.is_empty()
        || locked_sources
            .iter()
            .any(|line| !line.contains(&format!("?rev={}#{}", source.commit, source.commit)))
    {
        return Err("Cargo.lock contains an unpinned or mismatched Engine package".to_owned());
    }
    Ok(ProviderPinReadout {
        repository: source.public_repository,
        commit: source.commit,
        manifest_dependency_count: dependency_lines.len(),
        locked_provider_package_count: locked_sources.len(),
    })
}

fn reject_sibling_topology(value: &str) -> Result<(), String> {
    if value.contains("../rusty-engine") || value.contains("/home/dev/rusty-engine") {
        Err("provider configuration contains a sibling checkout path".to_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_pin_is_closed_and_exact() {
        let readout = checked_provider_pin().expect("checked provider pin should be coherent");
        assert_eq!(readout.commit, ENGINE_REVISION);
        assert_eq!(readout.manifest_dependency_count, 6);
        assert!(readout.locked_provider_package_count >= 6);
    }

    #[test]
    fn drift_and_sibling_inputs_are_rejected() {
        let source = include_str!("../engine-source.json");
        let manifest = include_str!("../Cargo.toml");
        let lock = include_str!("../Cargo.lock");
        assert!(validate_provider_pin(
            &source.replace(ENGINE_REVISION, "0000000000000000000000000000000000000000"),
            manifest,
            lock,
        )
        .is_err());
        assert!(
            validate_provider_pin(source, &format!("{manifest}\n../rusty-engine"), lock).is_err()
        );
        assert!(validate_provider_pin(
            source,
            &manifest.replace(ENGINE_REVISION, "0000000000000000000000000000000000000000"),
            lock,
        )
        .is_err());
    }
}
