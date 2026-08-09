pub use rusty_engine_voxels_revision::{validate_provider_pin, ProviderPinReadout};

pub fn checked_provider_pin() -> Result<ProviderPinReadout, String> {
    validate_provider_pin(
        include_str!("../engine-source.json"),
        include_str!("../Cargo.toml"),
        include_str!("../Cargo.lock"),
    )
}

pub fn engine_revision() -> Result<String, String> {
    Ok(checked_provider_pin()?.commit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_resolution_uses_one_complete_facade_and_exact_lock() {
        let readout = checked_provider_pin().expect("checked provider pin should be coherent");
        assert_eq!(engine_revision().expect("runtime revision"), readout.commit);
        assert_eq!(readout.manifest_dependency_count, 1);
        assert!(readout.locked_provider_package_count >= 1);
        for source in [
            include_str!("conversion.rs"),
            include_str!("churn.rs"),
            include_str!("format_study.rs"),
            include_str!("runtime.rs"),
        ] {
            assert!(
                !source.contains(&readout.commit),
                "runtime/evidence source must not duplicate the authored Engine commit"
            );
        }
    }

    #[test]
    fn drift_and_sibling_inputs_are_rejected() {
        let source = include_str!("../engine-source.json");
        let manifest = include_str!("../Cargo.toml");
        let lock = include_str!("../Cargo.lock");
        let revision = engine_revision().expect("runtime revision");
        assert!(validate_provider_pin(
            &source.replace(&revision, "0000000000000000000000000000000000000000"),
            manifest,
            lock,
        )
        .is_err());
        assert!(
            validate_provider_pin(source, &format!("{manifest}\n../rusty-engine"), lock).is_err()
        );
        assert!(validate_provider_pin(
            source,
            &manifest.replace("branch = \"main\"", "branch = \"release\""),
            lock,
        )
        .is_err());
    }
}
