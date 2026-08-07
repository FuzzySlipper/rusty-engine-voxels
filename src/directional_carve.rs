//! True 3D visual-hull carving from four directional sprite views.
//!
//! Unlike `directional_voxel.rs` which extrudes each view into an independent
//! 2.5D slab per clip frame, this module intersects silhouettes from all
//! supplied views for *one* pose (idle-0 first frame) to produce a single
//! volumetric frame. Depth is inferred from agreement, not authored as a
//! fixed column. It is deliberately downstream and non-deterministic.

use std::io::Cursor;
use std::path::Path;

use png::{BitDepth, ColorType, Decoder, Transformations};
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

pub const DIRECTIONAL_CARVE_SCHEMA_VERSION: u32 = 1;
const MAX_SPEC_BYTES: u64 = 256 * 1024;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 4096;
const MIN_VOXELS_PER_FRAME: usize = 3_000;
const MAX_VOXELS_PER_FRAME: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalCarveSpec {
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
    pub frame_id: String,
    pub frame_duration_microseconds: u64,
    pub carve_threshold: usize,
}

#[derive(Debug, Clone)]
pub struct DirectionalCarveRun {
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
struct ViewRgba {
    direction: String,
    rect: SpriteRect,
    anchor: [i32; 2],
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

pub fn load_spec(root: &Path, relative_path: &str) -> Result<DirectionalCarveSpec, String> {
    let path = safe_join(root, relative_path)?;
    let text = read_bounded_text(&path, MAX_SPEC_BYTES, "directional carve spec")?;
    serde_json::from_str(&text).map_err(|error| format!("{relative_path}: {error}"))
}

impl DirectionalCarveSpec {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != DIRECTIONAL_CARVE_SCHEMA_VERSION {
            return Err(format!(
                "directional carve schema {} is unsupported; expected {}",
                self.schema_version, DIRECTIONAL_CARVE_SCHEMA_VERSION
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
            (&self.frame_id, "frameId"),
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
        if self.frame_duration_microseconds == 0 {
            return Err("frameDurationMicroseconds must be positive".to_owned());
        }
        if self.carve_threshold == 0 || self.carve_threshold > 4 {
            return Err("carveThreshold must be 1..=4".to_owned());
        }
        Ok(())
    }
}

pub fn run_directional_carve(
    root: &Path,
    spec_path: &str,
    object_directory: &str,
) -> Result<DirectionalCarveRun, String> {
    let spec = load_spec(root, spec_path)?;
    spec.validate()?;
    let spec_bytes = read_bounded(
        &safe_join(root, spec_path)?,
        MAX_SPEC_BYTES,
        "directional carve spec",
    )?;
    let layout = DirectionalSpriteLayout::load(root, &spec.layout)?;
    let layout_bytes = read_bounded(
        &safe_join(root, &spec.layout)?,
        MAX_SPEC_BYTES,
        "directional carve layout",
    )?;
    if layout.source.path != spec.source_image {
        return Err(format!(
            "layout source {} does not match carve source {}",
            layout.source.path, spec.source_image
        ));
    }
    let image_path = safe_join(root, &spec.source_image)?;
    let image_bytes = read_bounded(&image_path, MAX_SOURCE_BYTES, "directional carve source")?;
    let actual_source_sha256 = sha256(&image_bytes);
    if actual_source_sha256 != spec.expected_source_sha256 {
        return Err(format!(
            "directional carve source identity drift: expected {}, computed {actual_source_sha256}",
            spec.expected_source_sha256
        ));
    }
    let image = decode_png(&image_bytes, &layout.source.background)?;
    layout.validate(image.width, image.height)?;

    let mut target_views: Vec<ViewRgba> = Vec::new();
    let mut found_frame = false;
    for action in &layout.actions {
        for frame in &action.frames {
            if frame.id == spec.frame_id {
                found_frame = true;
                for direction in &layout.directions {
                    let view = frame
                        .views
                        .iter()
                        .find(|v| &v.direction == direction)
                        .ok_or_else(|| format!("frame {} missing direction {direction}", spec.frame_id))?;
                    let rect = view.rect.clone().ok_or_else(|| {
                        format!("frame {} direction {direction} is explicitly missing", spec.frame_id)
                    })?;
                    let anchor = view.anchor.clone().ok_or_else(|| {
                        format!("frame {} direction {direction} missing anchor", spec.frame_id)
                    })?;
                    let cropped = crop_rect(&image, &rect)?;
                    target_views.push(ViewRgba {
                        direction: direction.clone(),
                        rect,
                        anchor: [anchor.x, anchor.y],
                        width: cropped.0,
                        height: cropped.1,
                        pixels: cropped.2,
                    });
                }
                break;
            }
        }
        if found_frame {
            break;
        }
    }
    if !found_frame {
        return Err(format!("frameId {} not found in layout", spec.frame_id));
    }
    if target_views.len() != 4 {
        return Err(format!(
            "expected 4 directions for carve, got {}",
            target_views.len()
        ));
    }

    let kit = load_kit(root, &spec.kit).map_err(|error| error.to_string())?;
    let frame = build_carved_frame(&target_views, &spec)?;
    let carved_voxel_count = frame.len();

    if frame.len() < MIN_VOXELS_PER_FRAME || frame.len() > MAX_VOXELS_PER_FRAME {
        return Err(format!(
            "carved frame has {} voxels; expected {MIN_VOXELS_PER_FRAME}..={MAX_VOXELS_PER_FRAME}",
            frame.len()
        ));
    }
    let bounds = frame
        .bounds()
        .ok_or("carved frame has no voxels")?;
    if bounds.0[1] != 0 {
        return Err(format!(
            "carved frame not grounded at y=0; min y is {}",
            bounds.0[1]
        ));
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
    let frames = vec![frame];
    let mut compiled = compile_posed_flipbook(
        &kit,
        &frames,
        &compile_settings,
        &source_bytes,
        spec.cell_size_meters,
    )?;
    compiled.asset.provenance.converter = "rusty-engine-voxels.directional-carve-v1".to_owned();
    compiled.asset =
        with_computed_voxel_object_hashes(compiled.asset).map_err(|error| error.to_string())?;
    compiled.canonical_json =
        encode_voxel_object(&compiled.asset).map_err(|error| error.to_string())?;
    let publication = publish_compiled_flipbook(root, object_directory, &compiled)?;

    let evidence = json!({
        "schemaVersion": 1,
        "kind": "directionalCarve",
        "spec": spec_path,
        "layout": spec.layout,
        "layoutSha256": sha256(&layout_bytes),
        "specSha256": sha256(&spec_bytes),
        "sourceImage": spec.source_image,
        "sourceImageSha256": actual_source_sha256,
        "sourceBundleSha256": sha256(&source_bytes),
        "kit": spec.kit,
        "cellSizeMeters": spec.cell_size_meters,
        "frameId": spec.frame_id,
        "carveThreshold": spec.carve_threshold,
        "directions": layout.directions,
        "views": target_views.iter().map(|v| json!({
            "direction": v.direction,
            "rect": v.rect,
            "anchor": v.anchor,
            "width": v.width,
            "height": v.height,
        })).collect::<Vec<_>>(),
        "carvedFrame": {
            "voxels": carved_voxel_count,
            "bounds": { "min": bounds.0, "max": bounds.1 },
            "voxelDataHash": compiled.asset.clips[0].frames[0].frame.voxel_data_hash.clone(),
        },
        "publication": {
            "assetId": publication.asset_id,
            "contentHash": publication.content_hash,
            "path": publication.path,
            "byteCount": publication.byte_count,
        },
    });
    Ok(DirectionalCarveRun {
        evidence,
        compiled,
        publication,
    })
}

fn build_carved_frame(
    views: &[ViewRgba],
    spec: &DirectionalCarveSpec,
) -> Result<RoughFrame, String> {
    let front = views.iter().find(|v| v.direction == "front").ok_or("missing front")?;
    let right = views.iter().find(|v| v.direction == "right").ok_or("missing right")?;
    let back = views.iter().find(|v| v.direction == "back").ok_or("missing back")?;
    let left = views.iter().find(|v| v.direction == "left").ok_or("missing left")?;

    let max_width = views.iter().map(|v| v.width as i32).max().unwrap_or(40);
    let max_height = views.iter().map(|v| v.height as i32).max().unwrap_or(57);
    let half_w = (max_width as i64) / 2 + 2;
    let half_d = half_w;

    let mut voxels: Vec<AssembledVoxelCell> = Vec::new();

    for y in 0..max_height {
        for x in -half_w..=half_w {
            for z in -half_d..=half_d {
                let mut hits = 0;
                let mut sample_pixels: Vec<[u8; 4]> = Vec::new();
                if let Some(px) = sample_view(front, x as i64, y as i64) {
                    if px[3] != 0 {
                        hits += 1;
                        sample_pixels.push(px);
                    }
                }
                if let Some(px) = sample_view(right, -z as i64, y as i64) {
                    if px[3] != 0 {
                        hits += 1;
                        sample_pixels.push(px);
                    }
                }
                if let Some(px) = sample_view(back, -x as i64, y as i64) {
                    if px[3] != 0 {
                        hits += 1;
                        sample_pixels.push(px);
                    }
                }
                if let Some(px) = sample_view(left, z as i64, y as i64) {
                    if px[3] != 0 {
                        hits += 1;
                        sample_pixels.push(px);
                    }
                }
                if hits >= spec.carve_threshold as i32 {
                    let material_slot = material_slot_from_samples(&sample_pixels, y as u32, max_height as u32);
                    voxels.push(AssembledVoxelCell {
                        coordinate: [x as i64, y as i64, z as i64],
                        material_slot,
                        part_id: 0,
                        source_voxel_index: voxels.len() as u32,
                        needs_fusion: false,
                    });
                }
            }
        }
    }
    if voxels.is_empty() {
        return Err("carved hull is empty — threshold too high or views misaligned".to_owned());
    }
    Ok(RoughFrame {
        time_microseconds: 0,
        duration_microseconds: spec.frame_duration_microseconds,
        voxels,
        discarded_overlaps: Vec::new(),
    })
}

fn sample_view(view: &ViewRgba, lateral: i64, y: i64) -> Option<[u8; 4]> {
    let u = lateral + view.anchor[0] as i64;
    let v = view.anchor[1] as i64 - y;
    if u < 0 || v < 0 || u >= view.width as i64 || v >= view.height as i64 {
        return None;
    }
    let offset = ((v as u32 * view.width + u as u32) * 4) as usize;
    if offset + 4 > view.pixels.len() {
        return None;
    }
    Some([
        view.pixels[offset],
        view.pixels[offset + 1],
        view.pixels[offset + 2],
        view.pixels[offset + 3],
    ])
}

fn material_slot_from_samples(pixels: &[[u8; 4]], local_y: u32, height: u32) -> u16 {
    if pixels.is_empty() {
        return 1;
    }
    let mut red_count = 0;
    for px in pixels {
        if u16::from(px[0]) > u16::from(px[1]) + 32 && u16::from(px[0]) > u16::from(px[2]) + 32 {
            red_count += 1;
        }
    }
    if red_count * 2 >= pixels.len() {
        return 3;
    }
    if local_y.saturating_mul(4) < height {
        2
    } else {
        1
    }
}

fn crop_rect(image: &RgbaImage, rect: &SpriteRect) -> Result<(u32, u32, Vec<u8>), String> {
    if rect.x + rect.width > image.width || rect.y + rect.height > image.height {
        return Err(format!(
            "rect {:?} outside {}x{}",
            rect, image.width, image.height
        ));
    }
    let mut pixels = Vec::with_capacity((rect.width * rect.height * 4) as usize);
    for row in 0..rect.height {
        let src_y = rect.y + row;
        let src_x = rect.x;
        let offset = ((src_y * image.width + src_x) * 4) as usize;
        let row_bytes = (rect.width * 4) as usize;
        pixels.extend_from_slice(&image.pixels[offset..offset + row_bytes]);
    }
    Ok((rect.width, rect.height, pixels))
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
