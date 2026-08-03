//! Density-experiment integration test: the checked bulky-knight smoke spec
//! bakes deterministically through the static Engine path, and re-running the
//! same spec regenerates byte-identical content identities.

use std::path::{Path, PathBuf};

use rusty_engine_voxels::density::{
    run_density_experiment, write_density_evidence, DensityBakeOutcome, DensityEvidence,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn smoke_evidence() -> DensityEvidence {
    run_density_experiment(&root(), "content/density/bulky-knight-smoke.spec.json")
        .expect("bulky-knight smoke spec runs")
}

fn published_metrics<'a>(
    evidence: &'a DensityEvidence,
    bake_id: &str,
) -> &'a rusty_engine_voxels::density::DensityBakeMetrics {
    let bake = evidence
        .bakes
        .iter()
        .find(|bake| bake.bake_id == bake_id)
        .unwrap_or_else(|| panic!("bake {bake_id} is present"));
    match &bake.outcome {
        DensityBakeOutcome::Published(metrics) => metrics,
        DensityBakeOutcome::Failed { stage, error } => {
            panic!("bake {bake_id} failed at {stage}: {error}")
        }
    }
}

#[test]
fn smoke_bakes_publish_pinned_results() {
    let evidence = smoke_evidence();
    assert_eq!(evidence.bakes.len(), 2);

    let whole = published_metrics(&evidence, "whole-64");
    assert_eq!(whole.source_triangles, 30_095);
    assert_eq!(whole.aggregate_voxels, 2_417);
    assert_eq!(whole.resolved_voxels, 2_417);
    assert_eq!(whole.frame_count, 1);
    assert_eq!(whole.unique_mesh_count, 1);
    assert_eq!(whole.silhouette_jaccard, 0.3757);
    assert_eq!(whole.projection_operation_count, 5);

    let armor = published_metrics(&evidence, "armor-lambert8-48");
    assert_eq!(armor.source_triangles, 13_372);
    assert_eq!(armor.aggregate_voxels, 3_454);
    assert_eq!(armor.silhouette_jaccard, 0.4989);
    assert_eq!(armor.projection_operation_count, 3);
}

#[test]
fn smoke_bakes_regenerate_identically() {
    let first = smoke_evidence();
    let second = smoke_evidence();
    assert_eq!(first.bakes.len(), second.bakes.len());
    for (left, right) in first.bakes.iter().zip(second.bakes.iter()) {
        assert_eq!(left.bake_id, right.bake_id);
        match (&left.outcome, &right.outcome) {
            (
                DensityBakeOutcome::Published(left_metrics),
                DensityBakeOutcome::Published(right_metrics),
            ) => {
                assert_eq!(left_metrics.plan_hash, right_metrics.plan_hash);
                assert_eq!(left_metrics.settings_sha256, right_metrics.settings_sha256);
                assert_eq!(left_metrics.content_hash, right_metrics.content_hash);
                assert_eq!(
                    left_metrics.aggregate_voxels,
                    right_metrics.aggregate_voxels
                );
                assert_eq!(left_metrics.artifact_bytes, right_metrics.artifact_bytes);
                assert_eq!(
                    left_metrics.silhouette_jaccard,
                    right_metrics.silhouette_jaccard
                );
            }
            (left_outcome, right_outcome) => {
                panic!("bake outcomes diverged: {left_outcome:?} vs {right_outcome:?}")
            }
        }
    }
}

#[test]
fn smoke_report_write_round_trips() {
    let evidence = smoke_evidence();
    let report = "evidence/density/bulky-knight-smoke.json";
    write_density_evidence(&root(), report, &evidence).expect("report writes");
    let written = std::fs::read_to_string(root().join(report)).expect("report reads");
    let reparsed: serde_json::Value = serde_json::from_str(&written).expect("report parses");
    let direct = serde_json::to_value(&evidence).expect("evidence serializes to a value");
    assert_eq!(reparsed, direct);
}
