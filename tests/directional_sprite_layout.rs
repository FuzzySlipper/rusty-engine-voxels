use std::fs;
use std::path::{Path, PathBuf};

use png::{BitDepth, ColorType};
use rusty_engine_voxels::directional::{
    inspect_layout, DirectionalSpriteAction, DirectionalSpriteFrame, DirectionalSpriteLayout,
    DirectionalSpriteSource, DirectionalSpriteView, SpriteBackground, SpriteRect,
};

fn root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/tmp/directional-layout-test")
        .join(std::process::id().to_string());
    fs::create_dir_all(root.join("local")).expect("test root");
    root
}

fn write_png(path: &Path) {
    let mut bytes = Vec::with_capacity(8 * 8 * 4);
    for _ in 0..(8 * 8) {
        bytes.extend_from_slice(&[0_u8, 255, 255, 255]);
    }
    let file = fs::File::create(path).expect("PNG file");
    let mut encoder = png::Encoder::new(file, 8, 8);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer.write_image_data(&bytes).expect("PNG pixels");
}

fn checked_layout() -> DirectionalSpriteLayout {
    DirectionalSpriteLayout {
        schema_version: 1,
        id: "fixture".to_owned(),
        source: DirectionalSpriteSource {
            path: "local/source.png".to_owned(),
            background: SpriteBackground {
                color_key: Some([0, 255, 255, 255]),
                color_keys: Vec::new(),
            },
        },
        directions: vec![
            "front".to_owned(),
            "right".to_owned(),
            "back".to_owned(),
            "left".to_owned(),
        ],
        actions: vec![DirectionalSpriteAction {
            id: "idle".to_owned(),
            name: "Idle".to_owned(),
            frames: vec![DirectionalSpriteFrame {
                id: "idle-0".to_owned(),
                name: "Idle 0".to_owned(),
                views: vec![
                    DirectionalSpriteView {
                        direction: "front".to_owned(),
                        rect: Some(SpriteRect {
                            x: 0,
                            y: 0,
                            width: 2,
                            height: 2,
                        }),
                        anchor: None,
                    },
                    DirectionalSpriteView {
                        direction: "right".to_owned(),
                        rect: Some(SpriteRect {
                            x: 2,
                            y: 0,
                            width: 2,
                            height: 2,
                        }),
                        anchor: None,
                    },
                    DirectionalSpriteView {
                        direction: "back".to_owned(),
                        rect: None,
                        anchor: None,
                    },
                    DirectionalSpriteView {
                        direction: "left".to_owned(),
                        rect: None,
                        anchor: None,
                    },
                ],
            }],
        }],
    }
}

#[test]
fn inspect_publishes_deterministic_crops_contact_sheet_and_missing_diagnostics() {
    let root = root();
    write_png(&root.join("local/source.png"));
    fs::write(
        root.join("layout.json"),
        serde_json::to_vec_pretty(&checked_layout()).expect("layout JSON"),
    )
    .expect("layout file");

    let inspection =
        inspect_layout(&root, "layout.json", "local/out", None, None, None).expect("inspection");
    assert_eq!(inspection.normalized.missing_views.len(), 2);
    assert_eq!(inspection.crops.len(), 2);
    assert!(inspection
        .output_dir
        .join("layout.normalized.json")
        .is_file());
    assert!(inspection.output_dir.join("contact-sheet.svg").is_file());
    assert!(inspection.contact_sheet_svg.contains("MISSING (explicit)"));

    let malformed = DirectionalSpriteLayout {
        actions: vec![DirectionalSpriteAction {
            id: "bad".to_owned(),
            name: "Bad".to_owned(),
            frames: vec![DirectionalSpriteFrame {
                id: "bad-0".to_owned(),
                name: "Bad 0".to_owned(),
                views: checked_layout().actions[0].frames[0]
                    .views
                    .iter()
                    .cloned()
                    .map(|mut view| {
                        if view.direction == "right" {
                            view.rect = Some(SpriteRect {
                                x: 1,
                                y: 0,
                                width: 2,
                                height: 2,
                            });
                        }
                        view
                    })
                    .collect(),
            }],
        }],
        ..checked_layout()
    };
    fs::write(
        root.join("malformed.json"),
        serde_json::to_vec(&malformed).expect("JSON"),
    )
    .expect("malformed file");
    let error = inspect_layout(
        &root,
        "malformed.json",
        "local/not-published",
        None,
        None,
        None,
    )
    .expect_err("overlap must reject before output");
    assert!(error.contains("overlap"), "{error}");
    assert!(!root.join("local/not-published").exists());

    fs::remove_dir_all(root).expect("bounded test cleanup");
}
