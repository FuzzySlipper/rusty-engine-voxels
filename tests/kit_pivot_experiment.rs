//! Manual piece-pivoting experiment: can an agent pose a mesh-derived kit by
//! reasoning about part rotations alone — no rig, no skinning, no engine
//! changes — using the existing rigid rasterization and assembly tooling?
//!
//! Loads the checked knight kit (168k voxels, 11 parts, authored by
//! `voxel-kit-lab`), applies hand-authored pivot rotations for an idle and two
//! walk poses, assembles rough frames through the rig-free
//! `assemble_placed_frame`, and measures per-part churn, volume stability,
//! ground contact, and determinism. Renders are recorded in
//! `evidence/kit-pivot-knight.json` for review.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusty_engine_voxels::assemble::{assemble_placed_frame, RoughFrame};
use rusty_engine_voxels::kit::{load_kit, neutral_part_transforms, VoxelKit};
use rusty_engine_voxels::pose::{RasterSettings, RigidTransform};
use rusty_engine_voxels::project::atomic_write;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const KNIGHT_KIT: &str = "content/characters/knight/character.json";

/// A hand-authored pose: per-part euler deltas (degrees) about each part's
/// own pivot, plus chains whose child part inherits a parent's rotation about
/// the parent's pivot (hand follows arm, weapon follows hand-arm chain).
#[derive(Clone, Copy)]
struct Delta {
    x_deg: f64,
    y_deg: f64,
    z_deg: f64,
}

impl Delta {
    const ZERO: Delta = Delta {
        x_deg: 0.0,
        y_deg: 0.0,
        z_deg: 0.0,
    };

    fn quaternion(self) -> [f64; 4] {
        // Compose Rz * Ry * Rx (apply X first, then Y, then Z).
        let qx = axis_angle([1.0, 0.0, 0.0], self.x_deg);
        let qy = axis_angle([0.0, 1.0, 0.0], self.y_deg);
        let qz = axis_angle([0.0, 0.0, 1.0], self.z_deg);
        quat_mul(qz, quat_mul(qy, qx))
    }
}

fn axis_angle(axis: [f64; 3], degrees: f64) -> [f64; 4] {
    let half = degrees.to_radians() * 0.5;
    let s = half.sin();
    [axis[0] * s, axis[1] * s, axis[2] * s, half.cos()]
}

fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// Rotate `point` by `rotation` (unit quaternion, no translation).
fn quat_rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let [x, y, z, w] = q;
    let [vx, vy, vz] = v;
    let tx = 2.0 * (y * vz - z * vy);
    let ty = 2.0 * (z * vx - x * vz);
    let tz = 2.0 * (x * vy - y * vx);
    [
        vx + w * tx + y * tz - z * ty,
        vy + w * ty + z * tx - x * tz,
        vz + w * tz + x * ty - y * tx,
    ]
}

struct PoseSpec {
    name: &'static str,
    deltas: &'static [(&'static str, Delta)],
    /// child part → parent part whose delta the child inherits about the
    /// parent's pivot.
    chains: &'static [(&'static str, &'static str)],
}

const POSES: &[PoseSpec] = &[
    PoseSpec {
        name: "neutral",
        deltas: &[],
        chains: &[],
    },
    PoseSpec {
        name: "idle",
        deltas: &[
            (
                "helmet",
                Delta {
                    x_deg: 5.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "torso",
                Delta {
                    x_deg: 0.0,
                    y_deg: 2.0,
                    z_deg: 0.0,
                },
            ),
            (
                "left_arm",
                Delta {
                    x_deg: 4.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "right_arm",
                Delta {
                    x_deg: 4.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
        ],
        chains: &[
            ("left_hand", "left_arm"),
            ("right_hand", "right_arm"),
            ("sword", "left_arm"),
            ("pillum", "right_arm"),
        ],
    },
    PoseSpec {
        name: "walk_a",
        deltas: &[
            (
                "torso",
                Delta {
                    x_deg: 0.0,
                    y_deg: 4.0,
                    z_deg: 0.0,
                },
            ),
            (
                "helmet",
                Delta {
                    x_deg: -3.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "left_leg",
                Delta {
                    x_deg: 18.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "right_leg",
                Delta {
                    x_deg: -14.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "left_arm",
                Delta {
                    x_deg: -10.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "right_arm",
                Delta {
                    x_deg: 12.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
        ],
        chains: &[
            ("left_hand", "left_arm"),
            ("right_hand", "right_arm"),
            ("sword", "left_arm"),
            ("pillum", "right_arm"),
        ],
    },
    PoseSpec {
        name: "walk_b",
        deltas: &[
            (
                "torso",
                Delta {
                    x_deg: 0.0,
                    y_deg: -4.0,
                    z_deg: 0.0,
                },
            ),
            (
                "helmet",
                Delta {
                    x_deg: -3.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "left_leg",
                Delta {
                    x_deg: -14.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "right_leg",
                Delta {
                    x_deg: 18.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "left_arm",
                Delta {
                    x_deg: 12.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
            (
                "right_arm",
                Delta {
                    x_deg: -10.0,
                    y_deg: 0.0,
                    z_deg: 0.0,
                },
            ),
        ],
        chains: &[
            ("left_hand", "left_arm"),
            ("right_hand", "right_arm"),
            ("sword", "left_arm"),
            ("pillum", "right_arm"),
        ],
    },
];

/// Compute one rigid placement per part for a pose, from the kit's neutral
/// transforms. Parts rotate about their own pivot (the part-local origin);
/// chained children additionally inherit their parent's rotation about the
/// parent's pivot point, keeping the chain roughly attached.
fn pose_placements(
    kit: &VoxelKit,
    spec: &PoseSpec,
) -> Result<BTreeMap<String, RigidTransform>, String> {
    let neutral = neutral_part_transforms(kit).map_err(|error| error.to_string())?;
    let deltas: BTreeMap<&str, Delta> = spec.deltas.iter().copied().collect();
    let chains: BTreeMap<&str, &str> = spec.chains.iter().copied().collect();
    let mut placements = BTreeMap::new();
    for part in &kit.parts {
        let (base_rotation, base_translation) = neutral
            .get(&part.id)
            .ok_or_else(|| format!("part {} has no neutral transform", part.id))?;
        let _ = base_rotation;
        let own = deltas.get(part.id.as_str()).copied().unwrap_or(Delta::ZERO);
        let own_rotation = own.quaternion();
        // Rotate cells (pivot-local) by the own delta; the pivot (local
        // origin) stays at the neutral translation.
        let mut placement = RigidTransform {
            rotation: own_rotation,
            translation: [
                base_translation[0] as f64,
                base_translation[1] as f64,
                base_translation[2] as f64,
            ],
        };
        if let Some(parent_id) = chains.get(part.id.as_str()) {
            let parent_delta = deltas.get(parent_id).copied().unwrap_or(Delta::ZERO);
            let parent_rotation = parent_delta.quaternion();
            let (_, parent_translation) = neutral
                .get(*parent_id)
                .ok_or_else(|| format!("chain parent {parent_id} has no neutral transform"))?;
            let pivot = [
                parent_translation[0] as f64,
                parent_translation[1] as f64,
                parent_translation[2] as f64,
            ];
            let rotated_pivot = quat_rotate(parent_rotation, pivot);
            // about_parent = { rotation: parent_q, translation: pivot - parent_q*pivot }
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
    Ok(placements)
}

fn part_churn(reference: &RoughFrame, candidate: &RoughFrame) -> BTreeMap<String, (usize, usize)> {
    // Per part: (displaced cells, total cells) comparing reference → candidate.
    let mut reference_cells: BTreeMap<u32, std::collections::BTreeSet<[i64; 3]>> = BTreeMap::new();
    for voxel in &reference.voxels {
        reference_cells
            .entry(voxel.part_id)
            .or_default()
            .insert(voxel.coordinate);
    }
    let mut churn: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut candidate_cells: BTreeMap<u32, std::collections::BTreeSet<[i64; 3]>> = BTreeMap::new();
    for voxel in &candidate.voxels {
        candidate_cells
            .entry(voxel.part_id)
            .or_default()
            .insert(voxel.coordinate);
    }
    for (part_index, cells) in &candidate_cells {
        let displaced = cells
            .iter()
            .filter(|cell| {
                !reference_cells
                    .get(part_index)
                    .is_some_and(|reference| reference.contains(*cell))
            })
            .count();
        churn.insert(format!("part/{part_index}"), (displaced, cells.len()));
    }
    churn
}

fn render(frame: &RoughFrame, outer: usize, width: usize, height: usize) -> Vec<String> {
    let Some((min, max)) = frame.bounds() else {
        return Vec::new();
    };
    let span_w = (max[outer] - min[outer] + 1) as f64;
    let span_h = (max[1] - min[1] + 1) as f64;
    let scale = (width as f64 / span_w).min(height as f64 / span_h);
    let out_w = (span_w * scale).ceil() as usize;
    let out_h = (span_h * scale).ceil() as usize;
    let mut grid = vec![vec![' '; out_w]; out_h];
    for voxel in &frame.voxels {
        let gx = ((voxel.coordinate[outer] - min[outer]) as f64 * scale) as usize;
        let gy = ((max[1] - voxel.coordinate[1]) as f64 * scale) as usize;
        if gx < out_w && gy < out_h {
            grid[gy][gx] = match voxel.material_slot {
                1 => '#',
                2 => 'H',
                3 => 'S',
                4 => 'h',
                5 => 'W',
                6 => 'C',
                _ => '+',
            };
        }
    }
    grid.into_iter()
        .map(|row| row.into_iter().collect())
        .collect()
}

#[test]
fn manual_pivoting_poses_a_mesh_derived_kit() {
    let kit = load_kit(&root(), KNIGHT_KIT).expect("checked knight kit loads");
    assert_eq!(kit.parts.len(), 11);
    let settings = RasterSettings::default();

    let mut frames: BTreeMap<&str, RoughFrame> = BTreeMap::new();
    let mut timings: BTreeMap<&str, u128> = BTreeMap::new();
    for spec in POSES {
        let placements = pose_placements(&kit, spec).expect("pose placements");
        let started = std::time::Instant::now();
        let frame = assemble_placed_frame(&kit, &placements, 0, 1, &settings)
            .unwrap_or_else(|error| panic!("pose {} assembles: {error}", spec.name));
        timings.insert(spec.name, started.elapsed().as_millis());
        frames.insert(spec.name, frame);
    }

    // Determinism: the same pose assembled twice is identical.
    let placements = pose_placements(&kit, &POSES[2]).expect("placements");
    let again = assemble_placed_frame(&kit, &placements, 0, 1, &settings).expect("walk_a again");
    assert_eq!(frames["walk_a"].voxels, again.voxels);

    // Ground contact: no pose sinks meaningfully below the ground plane. A
    // real stride drops the trailing heel a few cells below the standing
    // plane; deeper sinks would mean a pivot is wrong, not a pose is deep.
    for (name, frame) in &frames {
        let (min, _) = frame.bounds().expect("frame bounds");
        assert!(
            min[1] >= -8,
            "pose {name} sinks below ground: {:?}",
            frame.bounds()
        );
    }

    // Volume stability: rigid parts hold their volume within seam-overlap
    // noise (legs sweeping the cloth skirt, the sword tip at the boots):
    // per-part within ±15% low / +55% high — enough to catch a wrong pivot
    // (a part flying off or shredding) without policing legitimate overlap
    // resolution. The conservative contract only ever dilates upward; losses
    // come from earlier parts winning contested seam cells.
    let neutral = &frames["neutral"];
    for name in ["idle", "walk_a", "walk_b"] {
        let frame = &frames[name];
        let mut neutral_counts: BTreeMap<u32, usize> = BTreeMap::new();
        for voxel in &neutral.voxels {
            *neutral_counts.entry(voxel.part_id).or_default() += 1;
        }
        let mut pose_counts: BTreeMap<u32, usize> = BTreeMap::new();
        for voxel in &frame.voxels {
            *pose_counts.entry(voxel.part_id).or_default() += 1;
        }
        for (part_index, neutral_count) in &neutral_counts {
            let count = pose_counts.get(part_index).copied().unwrap_or(0);
            assert!(
                count * 20 >= neutral_count * 17 && count <= neutral_count * 3 / 2 + 8,
                "pose {name} part {part_index} volume {count} vs neutral {neutral_count}"
            );
        }
    }

    // The pipeline's core claim, at manual scale: parts that do not move
    // contribute exactly zero churn between poses.
    let idle_churn = part_churn(neutral, &frames["idle"]);
    let cloth_index = kit
        .parts
        .iter()
        .position(|part| part.id == "cloth")
        .expect("cloth part") as u32;
    let leg_left_index = kit
        .parts
        .iter()
        .position(|part| part.id == "left_leg")
        .expect("left leg") as u32;
    assert_eq!(
        idle_churn.get(&format!("part/{cloth_index}")),
        Some(&(0, idle_churn[&format!("part/{cloth_index}")].1)),
        "a still part contributes zero churn in idle"
    );
    assert_eq!(
        idle_churn.get(&format!("part/{leg_left_index}")),
        Some(&(0, idle_churn[&format!("part/{leg_left_index}")].1)),
        "still legs contribute zero churn in idle"
    );

    // Walk poses move the legs hard while the head/torso churn stays small.
    let walk_churn = part_churn(neutral, &frames["walk_a"]);
    let (leg_displaced, leg_total) = walk_churn[&format!("part/{leg_left_index}")];
    assert!(
        leg_displaced * 5 > leg_total,
        "walk_a should displace at least a fifth of the swinging leg, got {leg_displaced}/{leg_total}"
    );

    // The walk cycle's two contact poses actually differ.
    assert_ne!(frames["walk_a"].voxels, frames["walk_b"].voxels);

    // Seam regions are flagged for fusion (the M3 handoff): rotating limbs
    // opens gaps and overlaps at sockets, and the assembly must name them.
    for name in ["walk_a", "walk_b"] {
        assert!(
            frames[name].fusion_candidates() > 0,
            "pose {name} should flag fusion candidates at torn seams"
        );
    }

    // Evidence: counts, churn, and review renders per pose.
    let mut evidence = String::from("{\n  \"poses\": [\n");
    for (index, spec) in POSES.iter().enumerate() {
        let frame = &frames[spec.name];
        let churn = part_churn(neutral, frame);
        let churn_json = churn
            .iter()
            .map(|(part, (displaced, total))| {
                format!("      {{ \"part\": \"{part}\", \"displaced\": {displaced}, \"total\": {total} }}")
            })
            .collect::<Vec<_>>()
            .join(",\n");
        let front = render(frame, 0, 72, 34)
            .iter()
            .map(|row| {
                format!(
                    "        \"{}\"",
                    row.replace('\\', "\\\\").replace('"', "\\\"")
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        let side = render(frame, 2, 40, 34)
            .iter()
            .map(|row| {
                format!(
                    "        \"{}\"",
                    row.replace('\\', "\\\\").replace('"', "\\\"")
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        evidence.push_str(&format!(
            "    {{\n      \"name\": \"{}\",\n      \"voxels\": {},\n      \"fusionCandidates\": {},\n      \"assemblyMilliseconds\": {},\n      \"churnVsNeutral\": [\n{}\n      ],\n      \"front\": [\n{}\n      ],\n      \"side\": [\n{}\n      ]\n    }}{}\n",
            spec.name,
            frame.len(),
            frame.fusion_candidates(),
            timings[spec.name],
            churn_json,
            front,
            side,
            if index + 1 == POSES.len() { "" } else { "," }
        ));
    }
    evidence.push_str("  ]\n}\n");
    let report = root().join(format!(
        "target/test-evidence/kit-pivot-knight-{}.json",
        std::process::id()
    ));
    atomic_write(&report, evidence.as_bytes()).expect("report writes");
}
