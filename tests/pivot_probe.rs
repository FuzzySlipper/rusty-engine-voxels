//! Timing probe for the manual-pivot experiment: which phase is slow at
//! 168k voxels in a debug build?

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use rusty_engine_voxels::assemble::assemble_placed_frame;
use rusty_engine_voxels::kit::{load_kit, neutral_part_transforms};
use rusty_engine_voxels::pose::{rasterize_part, RasterSettings, RigidTransform};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const KNIGHT_KIT: &str = "content/characters/knight/character.json";

#[test]
fn probe_neutral_assembly_timing() {
    let kit = load_kit(&root(), KNIGHT_KIT).expect("kit");
    let neutral = neutral_part_transforms(&kit).expect("neutral transforms");
    let settings = RasterSettings::default();

    let started = Instant::now();
    let mut placements = BTreeMap::new();
    for part in &kit.parts {
        let (_, translation) = neutral.get(&part.id).expect("neutral");
        placements.insert(
            part.id.clone(),
            RigidTransform {
                rotation: [0.0, 0.0, 0.0, 1.0],
                translation: [
                    translation[0] as f64,
                    translation[1] as f64,
                    translation[2] as f64,
                ],
            },
        );
    }
    eprintln!("placements: {:?}", started.elapsed());

    let started = Instant::now();
    let mut part_cells = 0usize;
    for part in &kit.parts {
        let placement = placements[&part.id];
        let per_part = Instant::now();
        let cells = rasterize_part(part, placement, &settings).expect("rasterize");
        part_cells += cells.len();
        eprintln!(
            "  rasterize {}: {:?} ({} cells)",
            part.id,
            per_part.elapsed(),
            cells.len()
        );
    }
    eprintln!(
        "rasterize all: {:?} ({part_cells} cells)",
        started.elapsed()
    );

    let started = Instant::now();
    let frame = assemble_placed_frame(&kit, &placements, 0, 1, &settings).expect("frame");
    eprintln!(
        "assemble_placed_frame: {:?} ({} voxels, {} fusion candidates)",
        started.elapsed(),
        frame.len(),
        frame.fusion_candidates()
    );
}

fn axis_angle_x(degrees: f64) -> [f64; 4] {
    let half = degrees.to_radians() * 0.5;
    [half.sin(), 0.0, 0.0, half.cos()]
}

#[test]
fn probe_walk_a_assembly_timing() {
    let kit = load_kit(&root(), KNIGHT_KIT).expect("kit");
    let neutral = neutral_part_transforms(&kit).expect("neutral transforms");
    let settings = RasterSettings::default();

    let deltas: BTreeMap<&str, f64> = [
        ("torso", 0.0),
        ("helmet", -3.0),
        ("left_leg", 18.0),
        ("right_leg", -14.0),
        ("left_arm", -10.0),
        ("right_arm", 12.0),
    ]
    .into_iter()
    .collect();
    let chains: BTreeMap<&str, &str> = [
        ("left_hand", "left_arm"),
        ("right_hand", "right_arm"),
        ("sword", "left_arm"),
        ("pillum", "right_arm"),
    ]
    .into_iter()
    .collect();

    let mut placements = BTreeMap::new();
    for part in &kit.parts {
        let (_, translation) = neutral.get(&part.id).expect("neutral");
        let base = [
            translation[0] as f64,
            translation[1] as f64,
            translation[2] as f64,
        ];
        let degrees = deltas.get(part.id.as_str()).copied().unwrap_or(0.0);
        let rotation = axis_angle_x(degrees);
        let mut placement = RigidTransform {
            rotation,
            translation: base,
        };
        if let Some(parent_id) = chains.get(part.id.as_str()) {
            let parent_degrees = deltas.get(parent_id).copied().unwrap_or(0.0);
            let parent_rotation = axis_angle_x(parent_degrees);
            let (_, parent_translation) = neutral.get(*parent_id).expect("parent");
            let pivot = [
                parent_translation[0] as f64,
                parent_translation[1] as f64,
                parent_translation[2] as f64,
            ];
            let rotated_pivot = rusty_engine_voxels::pose::RigidTransform {
                rotation: parent_rotation,
                translation: [0.0, 0.0, 0.0],
            }
            .apply(pivot);
            let about_parent = RigidTransform {
                rotation: parent_rotation,
                translation: [
                    pivot[0] - rotated_pivot[0],
                    pivot[1] - rotated_pivot[1],
                    pivot[2] - rotated_pivot[2],
                ],
            };
            placement = about_parent.then(placement);
        }
        placements.insert(part.id.clone(), placement);
    }

    let started = Instant::now();
    for part in &kit.parts {
        let placement = placements[&part.id];
        let per_part = Instant::now();
        let cells = rasterize_part(part, placement, &settings).expect("rasterize");
        eprintln!(
            "  walk_a rasterize {}: {:?} ({} cells)",
            part.id,
            per_part.elapsed(),
            cells.len()
        );
    }
    eprintln!("walk_a rasterize all: {:?}", started.elapsed());

    let started = Instant::now();
    let frame = assemble_placed_frame(&kit, &placements, 0, 1, &settings).expect("frame");
    eprintln!(
        "walk_a assemble_placed_frame: {:?} ({} voxels, {} fusion candidates)",
        started.elapsed(),
        frame.len(),
        frame.fusion_candidates()
    );
}
