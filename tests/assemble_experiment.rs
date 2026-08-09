use std::path::{Path, PathBuf};

use rusty_engine::voxel_convert;
use rusty_engine_voxels::assemble::{
    assemble_rough_schedule, select_pose_schedule, PoseSelectionSettings, SelectionReason,
};
use rusty_engine_voxels::kit::load_kit;
use rusty_engine_voxels::pose::evaluate_node_poses;
use rusty_engine_voxels::pose::{RasterSettings, RigMap};
use voxel_convert::{import_animated_mesh_source, MeshSourceFormat, MeshSourceImportRequest};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const RIFLEMAN_KIT: &str = "content/characters/rifleman/character.json";
const RIFLEMAN_RIG_MAP: &str = "content/characters/rifleman/rig-map.json";
const RETRO_GLB: &str = "content/sources/kenney-retro-character/character-medium.glb";

fn import_retro() -> voxel_convert::ImportedAnimatedMeshSource {
    let bytes = std::fs::read(root().join(RETRO_GLB)).expect("read retro glb");
    import_animated_mesh_source(&MeshSourceImportRequest {
        source_asset_id: "mesh-animation/retro-character".to_owned(),
        asset_version: 1,
        source_path: RETRO_GLB.to_owned(),
        format: MeshSourceFormat::Glb,
        source_bytes: bytes,
        expected_source_sha256: None,
        mesh_primitive: None,
    })
    .expect("import retro character")
}

fn load_rig_map() -> RigMap {
    let text = std::fs::read_to_string(root().join(RIFLEMAN_RIG_MAP)).expect("read rig map");
    serde_json::from_str(&text).expect("parse rig map")
}

#[test]
fn run_schedule_keeps_mandatory_frames_with_independent_durations() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    let schedule = select_pose_schedule(
        &imported.model,
        run_index,
        &PoseSelectionSettings::default(),
    )
    .expect("select run schedule");

    // Must keep first and last, and produce a bounded stepped schedule.
    assert!(schedule.len() >= 2);
    assert_eq!(schedule[0].reason, SelectionReason::First);
    assert_eq!(schedule[schedule.len() - 1].reason, SelectionReason::Last);
    assert!(schedule.len() <= PoseSelectionSettings::default().max_frames);

    // Independent durations are exact, positive, and cover the clip.
    let clip_duration = imported.model.clips[run_index].duration_microseconds;
    let total: u64 = schedule.iter().map(|p| p.duration_microseconds).sum();
    for pose in &schedule {
        assert!(
            pose.duration_microseconds > 0,
            "each pose holds a positive duration"
        );
    }
    // First pose starts at 0; durations tile to the clip end.
    assert_eq!(schedule[0].time_microseconds, 0);
    assert_eq!(
        schedule[0].time_microseconds + total,
        clip_duration,
        "schedule must tile the full clip duration"
    );

    // Deterministic.
    let again = select_pose_schedule(
        &imported.model,
        run_index,
        &PoseSelectionSettings::default(),
    )
    .expect("reselect");
    assert_eq!(schedule, again);
}

#[test]
fn walk_and_run_schedules_produce_coherent_rough_assemblies() {
    let kit = load_kit(&root(), RIFLEMAN_KIT).expect("kit");
    let imported = import_retro();
    let rig_map = load_rig_map();
    rig_map.validate(&kit, &imported.model).expect("rig map");
    let settings = RasterSettings::default();

    // Use run (index of "run") and idle as the walk-ish clip for schedule variety.
    for clip_name in ["run", "idle"] {
        let clip_index = imported
            .model
            .clips
            .iter()
            .position(|c| c.name == clip_name)
            .expect("clip present");
        let schedule = select_pose_schedule(
            &imported.model,
            clip_index,
            &PoseSelectionSettings::default(),
        )
        .expect("schedule");
        let frames = assemble_rough_schedule(
            &kit,
            &rig_map,
            &imported.model,
            clip_index,
            &schedule,
            &settings,
        )
        .expect("assemble schedule");

        assert_eq!(frames.len(), schedule.len());
        for frame in &frames {
            // Every frame is coherent: non-empty, bounded, mostly canonical.
            assert!(!frame.is_empty(), "{clip_name} frame must be non-empty");
            let (lo, hi) = frame.bounds().expect("bounds");
            assert!(hi[1] > lo[1], "{clip_name} frame has vertical extent");
            // Fusion candidates are a minority of the frame (joints, not everything).
            let fusion = frame.fusion_candidates();
            assert!(
                fusion < frame.len(),
                "{clip_name}: fusion candidates {fusion} should be < frame size {}",
                frame.len()
            );
        }

        // A still region across two poses has zero churn in that region: the
        // head (a single rigid part) should be identical between the first two
        // frames if its bone doesn't move; more robustly, the set of part ids
        // present is stable across all frames.
        let parts_per_frame: Vec<std::collections::BTreeSet<u32>> = frames
            .iter()
            .map(|f| f.voxels.iter().map(|v| v.part_id).collect())
            .collect();
        let first = &parts_per_frame[0];
        for parts in &parts_per_frame {
            assert_eq!(
                parts, first,
                "{clip_name}: part composition must be stable across frames"
            );
        }
    }
}

#[test]
fn schedule_error_stays_within_budget() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    let settings = PoseSelectionSettings::default();
    let schedule = select_pose_schedule(&imported.model, run_index, &settings).expect("schedule");

    // The selector must not keep every candidate frame (it reduces), and must
    // keep mandatory event frames in a run cycle (legs cross).
    let candidate_count = 60usize; // rough upper bound of ticks
    assert!(
        schedule.len() < candidate_count.max(4),
        "selector should reduce, not keep everything"
    );
    // In a run cycle there is at least one non-trivial event/error-budget frame
    // beyond first/last.
    let intermediate = schedule
        .iter()
        .filter(|p| {
            matches!(
                p.reason,
                SelectionReason::Event | SelectionReason::ErrorBudget
            )
        })
        .count();
    assert!(
        intermediate >= 1,
        "a run cycle should surface event or error-budget frames"
    );
}

// --- R6336-7 regressions: selector cap, error invariant, mandatory timestamps ---

#[test]
fn max_frames_cap_is_never_exceeded() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // A relaxed budget needs no subdivisions, so even a tight cap fits; the
    // cap bound still holds and events only fill leftover slots.
    let settings = PoseSelectionSettings {
        max_frames: 3,
        error_budget: 1.0e9,
        ..PoseSelectionSettings::default()
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &settings).expect("schedule");
    assert!(
        schedule.len() <= settings.max_frames,
        "cap {} must not be exceeded, got {}",
        settings.max_frames,
        schedule.len()
    );
    // max_frames < 2 is a typed settings error, not a silent overflow.
    let bad = PoseSelectionSettings {
        max_frames: 1,
        ..PoseSelectionSettings::default()
    };
    assert!(select_pose_schedule(&imported.model, run_index, &bad).is_err());
}

#[test]
fn overconstrained_budget_under_tight_cap_is_typed_impossibility() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|clip| clip.name == "run")
        .expect("run clip");
    // R6336-11: max_frames=3 with an error budget whose minimal error-bounded
    // schedule needs far more than 3 frames. The selector must fail with a
    // typed impossibility — not overflow the cap, and not return a partial
    // schedule whose retained intervals silently exceed the budget.
    let settings = PoseSelectionSettings {
        max_frames: 3,
        error_budget: 0.5,
        event_translation_threshold: 0.5,
        event_rotation_threshold: 0.5,
        ..PoseSelectionSettings::default()
    };
    let result = select_pose_schedule(&imported.model, run_index, &settings);
    assert!(
        result.is_err(),
        "an error-bounded schedule that cannot fit the cap must be a typed impossibility, got {result:?}"
    );

    // Adding a mandatory anchor cannot make an infeasible selection feasible.
    let mandatory_settings = PoseSelectionSettings {
        mandatory_timestamps: vec![266_666],
        ..settings
    };
    assert!(
        select_pose_schedule(&imported.model, run_index, &mandatory_settings).is_err(),
        "required anchors plus an over-tight budget must remain a typed impossibility"
    );
}

#[test]
fn mandatory_timestamps_are_always_retained() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    let mandatory_time = 250_000u64;
    let settings = PoseSelectionSettings {
        mandatory_timestamps: vec![mandatory_time],
        ..PoseSelectionSettings::default()
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &settings).expect("schedule");
    assert!(
        schedule.iter().any(|pose| {
            pose.time_microseconds == mandatory_time && pose.reason == SelectionReason::Mandatory
        }),
        "an authored mandatory timestamp must be retained with the Mandatory reason"
    );
}

#[test]
fn error_budget_intervals_stay_within_budget() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // A budget below the clip's minimum inter-tick error is unsatisfiable and
    // must be a typed impossibility error, not a silent over-budget schedule.
    let unsatisfiable = PoseSelectionSettings {
        error_budget: 0.05,
        event_translation_threshold: 1.0e9,
        event_rotation_threshold: 1.0e9,
        ..PoseSelectionSettings::default()
    };
    let result = select_pose_schedule(&imported.model, run_index, &unsatisfiable);
    assert!(
        result.is_err(),
        "an unsatisfiable error budget must be rejected with a typed error"
    );

    // A satisfiable budget keeps every consecutive kept interval within budget.
    let settings = PoseSelectionSettings {
        error_budget: 1.0,
        event_translation_threshold: 1.0e9,
        event_rotation_threshold: 1.0e9,
        ..PoseSelectionSettings::default()
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &settings).expect("schedule");
    let times: Vec<u64> = schedule.iter().map(|p| p.time_microseconds).collect();
    let poses: Vec<_> = times
        .iter()
        .map(|&t| evaluate_node_poses(&imported.model, run_index, t).expect("pose"))
        .collect();
    let budget = settings.error_budget;
    for pair in poses.windows(2) {
        let error = rusty_engine_voxels::assemble::pose_error(&pair[0], &pair[1]);
        assert!(
            error <= budget,
            "consecutive poses exceed the budget: {error} > {budget}"
        );
    }
}

// --- R6336-9 regressions: mandatory capacity, true tail, per-step feasibility ---

#[test]
fn mandatory_capacity_overflow_is_typed_impossibility_not_silent_drop() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // max_frames=3 + three distinct mandatory timestamps (none equal to first or
    // last) needs 5 slots; the selector must reject with a typed impossibility
    // error rather than silently drop a mandatory anchor.
    let settings = PoseSelectionSettings {
        max_frames: 3,
        mandatory_timestamps: vec![133_333, 266_666, 400_000],
        ..PoseSelectionSettings::default()
    };
    let result = select_pose_schedule(&imported.model, run_index, &settings);
    assert!(
        result.is_err(),
        "mandatory anchors that cannot fit under the cap must be a typed error, not a silent drop"
    );

    // The same anchors DO fit when the cap allows the staged error-bounded
    // schedule (first + last + 3 mandatory + required subdivisions), and all
    // are retained.
    let fitting = PoseSelectionSettings {
        max_frames: 16,
        mandatory_timestamps: vec![133_333, 266_666, 400_000],
        ..PoseSelectionSettings::default()
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &fitting).expect("schedule");
    for time in [133_333u64, 266_666, 400_000] {
        assert!(
            schedule
                .iter()
                .any(|p| p.time_microseconds == time && p.reason == SelectionReason::Mandatory),
            "mandatory timestamp {time} must be retained when the cap allows it"
        );
    }
}

#[test]
fn tight_cap_keeps_true_tail_not_truncated_candidate() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // A very tight cap must still label the actual final tick (the true tail of
    // the native timeline) as Last, not an early truncated candidate. (The
    // relaxed budget keeps the two-frame schedule feasible under the hard
    // cap/budget contract.)
    let settings = PoseSelectionSettings {
        max_frames: 2,
        error_budget: 1.0e9,
        ..PoseSelectionSettings::default()
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &settings).expect("schedule");
    let last = schedule.last().expect("non-empty schedule");
    assert_eq!(last.reason, SelectionReason::Last);
    // The true tail is the largest candidate tick below the clip duration.
    let clip = &imported.model.clips[run_index];
    let duration = clip.duration_microseconds;
    let tick = 16_667u64.max(duration / 256);
    let mut expected_tail = 0u64;
    let mut t = 0u64;
    while t < duration {
        expected_tail = t;
        t += tick;
    }
    assert_eq!(
        last.time_microseconds, expected_tail,
        "the Last pose must be the true tail of the native timeline, not a truncated candidate"
    );
}

#[test]
fn below_max_step_budget_is_typed_impossibility() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // A budget between the clip's min and max adjacent-step errors is still
    // unsatisfiable somewhere (some adjacent step exceeds it), so it must be a
    // typed impossibility error. The reviewer's 0.05 is below the max step.
    let settings = PoseSelectionSettings {
        error_budget: 0.05,
        event_translation_threshold: 1.0e9,
        event_rotation_threshold: 1.0e9,
        ..PoseSelectionSettings::default()
    };
    let result = select_pose_schedule(&imported.model, run_index, &settings);
    assert!(
        result.is_err(),
        "a budget below the maximum indivisible-step error must be a typed impossibility error"
    );
}
#[test]
fn subdivision_beyond_cap_is_typed_impossibility_not_overflow() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // max_frames=3 with one mandatory anchor and a tight error budget requires
    // subdivision on both sides of the mandatory frame, which would need 5
    // frames. The selector must return a typed impossibility, never overflow.
    let settings = PoseSelectionSettings {
        max_frames: 3,
        mandatory_timestamps: vec![266_666],
        error_budget: 1.0,
        ..PoseSelectionSettings::default()
    };
    let result = select_pose_schedule(&imported.model, run_index, &settings);
    assert!(
        result.is_err(),
        "an error-bounded schedule that needs more frames than the cap must be a typed impossibility, got {result:?}"
    );

    // With room to subdivide, the same anchors fit within the cap and stay
    // within the error budget.
    let fitting = PoseSelectionSettings {
        max_frames: 16,
        mandatory_timestamps: vec![266_666],
        error_budget: 1.0,
        ..PoseSelectionSettings::default()
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &fitting).expect("schedule");
    assert!(schedule.len() <= fitting.max_frames);
    assert!(schedule
        .iter()
        .any(|p| p.time_microseconds == 266_666 && p.reason == SelectionReason::Mandatory));
}

// --- R6336-11: the exact one-mandatory cap-pressure regression ---

#[test]
fn one_mandatory_cap_pressure_is_typed_impossibility_with_measured_intervals() {
    let imported = import_retro();
    let run_index = imported
        .model
        .clips
        .iter()
        .position(|c| c.name == "run")
        .expect("run clip");
    // The exact R6336-11 reproduction: max_frames=3, one mandatory anchor,
    // event thresholds disabled, admitted error_budget=0.5. The complete
    // error-bounded schedule needs 24 frames (first + last + 1 mandatory + 21
    // required error subdivisions), so selection must fail with a typed
    // impossibility naming the requirement — never five frames under a
    // three-frame cap, and never a partial schedule whose retained intervals
    // silently exceed the budget.
    let settings = PoseSelectionSettings {
        max_frames: 3,
        mandatory_timestamps: vec![250_000],
        event_translation_threshold: 1.0e9,
        event_rotation_threshold: 1.0e9,
        error_budget: 0.5,
    };
    let result = select_pose_schedule(&imported.model, run_index, &settings);
    let Err(error) = &result else {
        panic!("a 24-frame error-bounded schedule under a 3-frame cap must be a typed impossibility, got {result:?}");
    };
    let message = error.to_string();
    assert!(
        message.contains("cannot hold the error-bounded schedule"),
        "the impossibility must be the staged-schedule cap error: {message}"
    );
    assert!(
        message.contains("24 frames"),
        "the impossibility must name the required frame count: {message}"
    );

    // With a cap that exactly fits the staged schedule, selection succeeds and
    // BOTH the frame count and every interval's measured error honor the
    // contract.
    let fitting = PoseSelectionSettings {
        max_frames: 24,
        ..settings
    };
    let schedule = select_pose_schedule(&imported.model, run_index, &fitting).expect("schedule");
    assert_eq!(
        schedule.len(),
        24,
        "the staged error-bounded schedule is exact: first + last + 1 mandatory + 21 subdivisions"
    );
    assert_eq!(
        schedule.first().map(|p| p.reason),
        Some(SelectionReason::First)
    );
    assert_eq!(
        schedule.last().map(|p| p.reason),
        Some(SelectionReason::Last)
    );
    assert!(schedule
        .iter()
        .any(|p| p.time_microseconds == 250_000 && p.reason == SelectionReason::Mandatory));
    let poses: Vec<_> = schedule
        .iter()
        .map(|p| {
            evaluate_node_poses(&imported.model, run_index, p.time_microseconds).expect("pose")
        })
        .collect();
    for pair in poses.windows(2) {
        let error = rusty_engine_voxels::assemble::pose_error(&pair[0], &pair[1]);
        assert!(
            error <= fitting.error_budget,
            "every retained interval must measure within budget: {error} > {}",
            fitting.error_budget
        );
    }
}
