//! Explicit sprite-pixel to voxel-frame authoring for the directional lab.
//!
//! This is deliberately a downstream experiment boundary. It consumes an
//! explicit sprite layout, an explicit extrusion depth, and an explicit
//! material policy; it does not add sprite conversion vocabulary to Rusty
//! Engine. Missing views fail closed instead of being mirrored or synthesized.

use std::collections::BTreeSet;
use std::io::Cursor;
use std::path::Path;

use png::{BitDepth, ColorType, Decoder, Transformations};
use rusty_engine::voxel_asset;
use serde::{Deserialize, Serialize};
use serde_json::json;
use voxel_asset::{encode_voxel_object, with_computed_voxel_object_hashes};

use crate::assemble::{AssembledVoxelCell, RoughFrame};
use crate::directional::{DirectionalSpriteLayout, SpriteRect};
use crate::flipbook::{
    compile_posed_flipbook, publish_compiled_flipbook, CompiledFlipbook, FlipbookCompileSettings,
    FlipbookPublication,
};
use crate::kit::load_kit;
use crate::project::{read_bounded, read_bounded_text, safe_join, sha256};

pub const DIRECTIONAL_VOXELIZATION_SCHEMA_VERSION: u32 = 1;
const MAX_SPEC_BYTES: u64 = 256 * 1024;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 4096;
const MAX_DEPTH_CELLS: u32 = 128;
const MIN_VOXELS_PER_FRAME: usize = 10_000;
const MAX_VOXELS_PER_FRAME: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalVoxelizationSpec {
    pub schema_version: u32,
    pub id: String,
    pub kit: String,
    pub layout: String,
    pub source_image: String,
    pub expected_source_sha256: String,
    pub output_asset_id: String,
    pub clip_id: String,
    pub clip_name: String,
    pub cell_size_meters: f64,
    pub depth_cells: u32,
    pub frame_duration_microseconds: u64,
}

#[derive(Debug, Clone)]
pub struct DirectionalVoxelizationRun {
    pub evidence: serde_json::Value,
    pub compiled: CompiledFlipbook,
    pub publication: FlipbookPublication,
}

#[derive(Debug, Clone)]
struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

#[derive(Debug, Clone)]
struct ViewBuild {
    direction_index: usize,
    source_frame: usize,
    rect: SpriteRect,
    anchor: [i32; 2],
}

pub fn load_spec(root: &Path, relative_path: &str) -> Result<DirectionalVoxelizationSpec, String> {
    let path = safe_join(root, relative_path)?;
    let text = read_bounded_text(&path, MAX_SPEC_BYTES, "directional voxelization spec")?;
    serde_json::from_str(&text).map_err(|error| format!("{relative_path}: {error}"))
}

impl DirectionalVoxelizationSpec {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != DIRECTIONAL_VOXELIZATION_SCHEMA_VERSION {
            return Err(format!(
                "directional voxelization schema {} is unsupported; expected {}",
                self.schema_version, DIRECTIONAL_VOXELIZATION_SCHEMA_VERSION
            ));
        }
        for (value, label) in [
            (&self.id, "id"),
            (&self.kit, "kit"),
            (&self.layout, "layout"),
            (&self.source_image, "sourceImage"),
            (&self.expected_source_sha256, "expectedSourceSha256"),
            (&self.output_asset_id, "outputAssetId"),
            (&self.clip_id, "clipId"),
            (&self.clip_name, "clipName"),
        ] {
            if value.trim().is_empty() || value != value.trim() {
                return Err(format!("{label} must be non-empty canonical text"));
            }
        }
        if !self.output_asset_id.starts_with("voxel-object/") {
            return Err("outputAssetId must start with voxel-object/".to_owned());
        }
        if !self.clip_id.starts_with("clip/") {
            return Err("clipId must start with clip/".to_owned());
        }
        if !self.cell_size_meters.is_finite() || self.cell_size_meters <= 0.0 {
            return Err("cellSizeMeters must be finite and positive".to_owned());
        }
        if self.depth_cells == 0 || self.depth_cells > MAX_DEPTH_CELLS {
            return Err(format!("depthCells must be within 1..={MAX_DEPTH_CELLS}"));
        }
        if self.frame_duration_microseconds == 0 {
            return Err("frameDurationMicroseconds must be positive".to_owned());
        }
        Ok(())
    }
}

/// Build and publish the explicit directional sprite voxel flipbook.
pub fn run_directional_voxelization(
    root: &Path,
    spec_path: &str,
    object_directory: &str,
) -> Result<DirectionalVoxelizationRun, String> {
    let spec = load_spec(root, spec_path)?;
    spec.validate()?;
    let spec_bytes = read_bounded(
        &safe_join(root, spec_path)?,
        MAX_SPEC_BYTES,
        "directional voxelization spec",
    )?;
    let layout = DirectionalSpriteLayout::load(root, &spec.layout)?;
    let layout_bytes = read_bounded(
        &safe_join(root, &spec.layout)?,
        MAX_SPEC_BYTES,
        "directional sprite layout",
    )?;
    if layout.source.path != spec.source_image {
        return Err(format!(
            "layout source {} does not match voxelization source {}",
            layout.source.path, spec.source_image
        ));
    }
    let image_path = safe_join(root, &spec.source_image)?;
    let image_bytes = read_bounded(&image_path, MAX_SOURCE_BYTES, "directional sprite source")?;
    let actual_source_sha256 = sha256(&image_bytes);
    if actual_source_sha256 != spec.expected_source_sha256 {
        return Err(format!(
            "directional sprite source identity drift: expected {}, computed {actual_source_sha256}",
            spec.expected_source_sha256
        ));
    }
    let image = decode_png(&image_bytes, &layout.source.background)?;
    layout.validate(image.width, image.height)?;
    let kit = load_kit(root, &spec.kit).map_err(|error| error.to_string())?;
    let views = explicit_views(&layout)?;
    let mut frames = Vec::with_capacity(views.len());
    let mut frame_evidence = Vec::with_capacity(views.len());
    let mut source_materials = BTreeSet::new();
    for view in views {
        let frame = build_frame(&image, &layout, &spec, &view)?;
        source_materials.extend(frame.voxels.iter().map(|voxel| voxel.material_slot));
        let bounds = frame
            .bounds()
            .ok_or("directional sprite view contains no opaque pixels")?;
        if frame.len() < MIN_VOXELS_PER_FRAME || frame.len() > MAX_VOXELS_PER_FRAME {
            return Err(format!(
                "direction {} frame {} has {} voxels; expected {MIN_VOXELS_PER_FRAME}..={MAX_VOXELS_PER_FRAME}",
                layout.directions[view.direction_index],
                view.source_frame,
                frame.len()
            ));
        }
        if bounds.0[1] != 0 {
            return Err(format!(
                "direction {} frame {} is not grounded at y=0; minimum y is {}",
                layout.directions[view.direction_index], view.source_frame, bounds.0[1]
            ));
        }
        frame_evidence.push(json!({
            "direction": layout.directions[view.direction_index],
            "sourceFrameIndex": view.source_frame,
            "sourceRect": view.rect,
            "anchor": view.anchor,
            "voxels": frame.len(),
            "bounds": { "min": bounds.0, "max": bounds.1 },
            "materialSlots": frame.voxels.iter().map(|voxel| voxel.material_slot).collect::<BTreeSet<_>>(),
        }));
        frames.push(frame);
    }

    let mut source_bytes =
        Vec::with_capacity(spec_bytes.len() + layout_bytes.len() + image_bytes.len() + 1024);
    source_bytes.extend_from_slice(&spec_bytes);
    source_bytes.extend_from_slice(&layout_bytes);
    source_bytes.extend_from_slice(&image_bytes);
    let compile_settings = FlipbookCompileSettings {
        asset_id: spec.output_asset_id.clone(),
        clip_id: spec.clip_id.clone(),
        clip_name: spec.clip_name.clone(),
        source_path: spec_path.to_owned(),
        chunk_size: 16,
        anchors: Vec::new(),
        body_collision: None,
        hit_regions: Vec::new(),
    };
    let mut compiled = compile_posed_flipbook(
        &kit,
        &frames,
        &compile_settings,
        &source_bytes,
        spec.cell_size_meters,
    )?;
    compiled.asset.provenance.converter =
        "rusty-engine-voxels.directional-sprite-pixel-v1".to_owned();
    compiled.asset =
        with_computed_voxel_object_hashes(compiled.asset).map_err(|error| error.to_string())?;
    compiled.canonical_json =
        encode_voxel_object(&compiled.asset).map_err(|error| error.to_string())?;
    for (evidence, animation_frame) in frame_evidence
        .iter_mut()
        .zip(compiled.asset.clips[0].frames.iter())
    {
        evidence["voxelDataHash"] = json!(animation_frame.frame.voxel_data_hash);
    }
    let publication = publish_compiled_flipbook(root, object_directory, &compiled)?;
    let peak_voxels = frames.iter().map(RoughFrame::len).max().unwrap_or(0);
    let total_voxels = frames.iter().map(RoughFrame::len).sum::<usize>();
    let evidence = json!({
        "schemaVersion": 1,
        "kind": "directionalSpriteVoxelization",
        "spec": spec_path,
        "layout": spec.layout,
        "layoutSha256": sha256(&layout_bytes),
        "specSha256": sha256(&spec_bytes),
        "sourceImage": spec.source_image,
        "sourceImageSha256": actual_source_sha256,
        "sourceBundleSha256": sha256(&source_bytes),
        "kit": spec.kit,
        "cellSizeMeters": spec.cell_size_meters,
        "depthCells": spec.depth_cells,
        "directions": layout.directions,
        "frames": frame_evidence,
        "materialSlots": source_materials,
        "peakVoxelsPerFrame": peak_voxels,
        "totalVoxels": total_voxels,
        "publication": {
            "assetId": publication.asset_id,
            "contentHash": publication.content_hash,
            "path": publication.path,
            "byteCount": publication.byte_count,
        },
    });
    Ok(DirectionalVoxelizationRun {
        evidence,
        compiled,
        publication,
    })
}

fn explicit_views(layout: &DirectionalSpriteLayout) -> Result<Vec<ViewBuild>, String> {
    let mut views = Vec::new();
    for direction_index in 0..layout.directions.len() {
        let direction = &layout.directions[direction_index];
        for action in &layout.actions {
            for (source_frame, frame) in action.frames.iter().enumerate() {
                let view = frame
                    .views
                    .iter()
                    .find(|view| view.direction == *direction)
                    .ok_or_else(|| {
                        format!(
                            "{}/{}/{} is missing an explicit direction entry",
                            action.id, frame.id, direction
                        )
                    })?;
                let rect = view.rect.clone().ok_or_else(|| {
                    format!(
                        "{}/{}/{} is explicitly missing and cannot be voxelized",
                        action.id, frame.id, direction
                    )
                })?;
                let anchor = view.anchor.as_ref().ok_or_else(|| {
                    format!(
                        "{}/{}/{} requires an explicit grounding anchor",
                        action.id, frame.id, direction
                    )
                })?;
                views.push(ViewBuild {
                    direction_index,
                    source_frame,
                    rect,
                    anchor: [anchor.x, anchor.y],
                });
            }
        }
    }
    Ok(views)
}

fn build_frame(
    image: &RgbaImage,
    layout: &DirectionalSpriteLayout,
    spec: &DirectionalVoxelizationSpec,
    view: &ViewBuild,
) -> Result<RoughFrame, String> {
    let direction = layout.directions[view.direction_index].as_str();
    let mut voxels = Vec::new();
    let half_depth = i64::from(spec.depth_cells.saturating_sub(1)) / 2;
    for local_y in 0..view.rect.height {
        for local_x in 0..view.rect.width {
            let source_x = view.rect.x + local_x;
            let source_y = view.rect.y + local_y;
            let pixel = image.pixel(source_x, source_y)?;
            if pixel[3] == 0 {
                continue;
            }
            let lateral = i64::from(local_x) - i64::from(view.anchor[0]);
            let y = i64::from(view.anchor[1]) - i64::from(local_y);
            let material_slot = material_slot(pixel, local_y, view.rect.height);
            for depth in 0..spec.depth_cells {
                let normal = i64::from(depth) - half_depth;
                let coordinate = match direction {
                    "front" => [lateral, y, normal],
                    "right" => [normal, y, -lateral],
                    "back" => [-lateral, y, -normal],
                    "left" => [-normal, y, lateral],
                    other => {
                        return Err(format!(
                            "unsupported directional voxel orientation {other}; add an explicit mapping"
                        ))
                    }
                };
                voxels.push(AssembledVoxelCell {
                    coordinate,
                    material_slot,
                    part_id: 0,
                    source_voxel_index: u32::try_from(voxels.len())
                        .map_err(|_| "directional voxel index overflow")?,
                    needs_fusion: false,
                });
            }
        }
    }
    if voxels.is_empty() {
        return Err(format!(
            "direction {direction} frame has no opaque sprite pixels"
        ));
    }
    Ok(RoughFrame {
        time_microseconds: 0,
        duration_microseconds: spec.frame_duration_microseconds,
        voxels,
        discarded_overlaps: Vec::new(),
    })
}

/// The experiment intentionally keeps a small, inspectable palette policy:
/// red-dominant sprite pixels are the weapon/accent slot, the top quarter is
/// the head slot, and the remainder is the body slot. It is not a production
/// segmentation algorithm; the source and policy remain explicitly local.
fn material_slot(pixel: [u8; 4], local_y: u32, height: u32) -> u16 {
    let red_dominant = u16::from(pixel[0]) > u16::from(pixel[1]) + 32
        && u16::from(pixel[0]) > u16::from(pixel[2]) + 32;
    if red_dominant {
        3
    } else if local_y.saturating_mul(4) < height {
        2
    } else {
        1
    }
}

fn decode_png(
    bytes: &[u8],
    background: &crate::directional::SpriteBackground,
) -> Result<RgbaImage, String> {
    let mut decoder = Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::EXPAND | Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("PNG header decode failed: {error}"))?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || info.width > MAX_SOURCE_DIMENSION
        || info.height > MAX_SOURCE_DIMENSION
    {
        return Err(format!(
            "PNG dimensions {}x{} exceed {MAX_SOURCE_DIMENSION} bound",
            info.width, info.height
        ));
    }
    let mut buffer = vec![0; reader.output_buffer_size()];
    let output = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("PNG frame decode failed: {error}"))?;
    if output.bit_depth != BitDepth::Eight {
        return Err("PNG must decode to 8-bit channels".to_owned());
    }
    let raw = &buffer[..output.buffer_size()];
    let mut pixels = Vec::with_capacity(
        usize::try_from(u64::from(output.width) * u64::from(output.height) * 4)
            .map_err(|_| "PNG size overflow")?,
    );
    match output.color_type {
        ColorType::Rgb => {
            for chunk in raw.chunks_exact(3) {
                pixels.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        ColorType::Rgba => pixels.extend_from_slice(raw),
        ColorType::Grayscale => {
            for value in raw {
                pixels.extend_from_slice(&[*value, *value, *value, 255]);
            }
        }
        ColorType::GrayscaleAlpha => {
            for chunk in raw.chunks_exact(2) {
                pixels.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
        }
        ColorType::Indexed => return Err("indexed PNG did not expand to RGB/RGBA".to_owned()),
    }
    for pixel in pixels.chunks_exact_mut(4) {
        if background_key(background, pixel) {
            pixel[3] = 0;
        }
    }
    Ok(RgbaImage {
        width: output.width,
        height: output.height,
        pixels,
    })
}

fn background_key(background: &crate::directional::SpriteBackground, pixel: &[u8]) -> bool {
    background
        .color_key
        .as_ref()
        .is_some_and(|key| pixel == key)
        || background.color_keys.iter().any(|key| pixel == key)
}

impl RgbaImage {
    fn pixel(&self, x: u32, y: u32) -> Result<[u8; 4], String> {
        if x >= self.width || y >= self.height {
            return Err(format!(
                "sprite pixel {x},{y} is outside {}x{}",
                self.width, self.height
            ));
        }
        let offset = usize::try_from((y * self.width + x) * 4)
            .map_err(|_| "sprite pixel offset overflow")?;
        self.pixels
            .get(offset..offset + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| "sprite pixel buffer is truncated".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directional::{DirectionalSpriteLayout, DirectionalSpriteSource};

    fn spec(depth_cells: u32) -> DirectionalVoxelizationSpec {
        DirectionalVoxelizationSpec {
            schema_version: DIRECTIONAL_VOXELIZATION_SCHEMA_VERSION,
            id: "test-directional-voxelization".to_owned(),
            kit: "content/kit.json".to_owned(),
            layout: "content/layout.json".to_owned(),
            source_image: "local/source.png".to_owned(),
            expected_source_sha256: "sha256:test".to_owned(),
            output_asset_id: "voxel-object/test".to_owned(),
            clip_id: "clip/idle".to_owned(),
            clip_name: "Idle".to_owned(),
            cell_size_meters: 0.01,
            depth_cells,
            frame_duration_microseconds: 120_000,
        }
    }

    fn image() -> RgbaImage {
        RgbaImage {
            width: 2,
            height: 2,
            pixels: vec![
                255, 0, 0, 255, // opaque accent pixel
                0, 255, 255, 0, // transparent keyed pixel
                0, 0, 0, 0, 0, 0, 0, 0,
            ],
        }
    }

    fn layout(direction: &str) -> DirectionalSpriteLayout {
        DirectionalSpriteLayout {
            schema_version: 1,
            id: "test-layout".to_owned(),
            source: DirectionalSpriteSource {
                path: "local/source.png".to_owned(),
                background: Default::default(),
            },
            directions: vec![direction.to_owned()],
            actions: Vec::new(),
        }
    }

    fn view() -> ViewBuild {
        ViewBuild {
            direction_index: 0,
            source_frame: 0,
            rect: SpriteRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            },
            anchor: [1, 1],
        }
    }

    #[test]
    fn every_opaque_pixel_gets_the_authored_depth_column() {
        let frame = build_frame(&image(), &layout("front"), &spec(4), &view()).expect("frame");
        assert_eq!(frame.len(), 4);
        assert_eq!(frame.voxels[0].coordinate, [-1, 1, -1]);
        assert_eq!(frame.voxels[3].coordinate, [-1, 1, 2]);
        assert!(frame.voxels.iter().all(|voxel| voxel.material_slot == 3));
    }

    #[test]
    fn cardinal_orientation_mapping_is_explicit() {
        let expected = [
            ("front", [-1, 1, 0]),
            ("right", [0, 1, 1]),
            ("back", [1, 1, 0]),
            ("left", [0, 1, -1]),
        ];
        for (direction, coordinate) in expected {
            let frame =
                build_frame(&image(), &layout(direction), &spec(2), &view()).expect("frame");
            assert_eq!(frame.voxels[0].coordinate, coordinate, "{direction}");
        }
    }

    #[test]
    fn material_policy_has_stable_head_and_body_fallbacks() {
        assert_eq!(material_slot([20, 10, 10, 255], 0, 8), 2);
        assert_eq!(material_slot([20, 10, 10, 255], 3, 8), 1);
        assert_eq!(material_slot([255, 20, 20, 255], 7, 8), 3);
    }
}
