//! Bounded, explicit inspection of directional sprite sheets.
//!
//! This module deliberately stops at authored layout and review artifacts. It
//! never infers depth, mirrors a view, or creates voxel content. A layout names
//! the rectangles an author selected and may use `null` for an explicitly
//! missing direction.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use png::{BitDepth, ColorType, Decoder, Transformations};
use serde::{Deserialize, Serialize};

use crate::base64::encode as base64_encode;
use crate::project::{atomic_write, read_bounded, safe_join, sha256};

pub const DIRECTIONAL_LAYOUT_SCHEMA_VERSION: u32 = 1;
const MAX_LAYOUT_BYTES: u64 = 256 * 1024;
const MAX_SOURCE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SOURCE_DIMENSION: u32 = 4096;
const MAX_CELLS: usize = 256;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_CONTACT_DIMENSION: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalSpriteLayout {
    pub schema_version: u32,
    pub id: String,
    pub source: DirectionalSpriteSource,
    pub directions: Vec<String>,
    pub actions: Vec<DirectionalSpriteAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalSpriteSource {
    pub path: String,
    #[serde(default)]
    pub background: SpriteBackground,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpriteBackground {
    #[serde(default)]
    pub color_key: Option<[u8; 4]>,
    /// Additional exact RGBA keys, useful for sheets with a page background
    /// around per-cell transparency colors. Keys are explicit; no tolerance
    /// or palette inference is performed.
    #[serde(default)]
    pub color_keys: Vec<[u8; 4]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalSpriteAction {
    pub id: String,
    pub name: String,
    pub frames: Vec<DirectionalSpriteFrame>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalSpriteFrame {
    pub id: String,
    pub name: String,
    pub views: Vec<DirectionalSpriteView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DirectionalSpriteView {
    pub direction: String,
    pub rect: Option<SpriteRect>,
    #[serde(default)]
    pub anchor: Option<SpriteAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpriteRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpriteAnchor {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NormalizedDirectionalLayout {
    pub schema_version: u32,
    pub id: String,
    pub source: NormalizedSpriteSource,
    pub directions: Vec<String>,
    pub actions: Vec<DirectionalSpriteAction>,
    pub source_area: u64,
    pub covered_area: u64,
    pub unused_area: u64,
    pub missing_views: Vec<MissingView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalizedSpriteSource {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub byte_count: usize,
    pub sha256: String,
    pub background: SpriteBackground,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissingView {
    pub action: String,
    pub frame: String,
    pub direction: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectionalInspection {
    pub normalized: NormalizedDirectionalLayout,
    pub output_dir: PathBuf,
    pub crops: BTreeMap<String, Vec<u8>>,
    pub contact_sheet_svg: String,
}

#[derive(Debug, Clone)]
struct RgbaImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl DirectionalSpriteLayout {
    pub fn load(root: &Path, relative_path: &str) -> Result<Self, String> {
        let path = safe_join(root, relative_path)?;
        let bytes = read_bounded(&path, MAX_LAYOUT_BYTES, "directional sprite layout")?;
        serde_json::from_slice(&bytes).map_err(|error| format!("{relative_path}: {error}"))
    }

    pub fn validate(&self, image_width: u32, image_height: u32) -> Result<(), String> {
        if self.schema_version != DIRECTIONAL_LAYOUT_SCHEMA_VERSION {
            return Err(format!(
                "layout schema {} is unsupported; expected {}",
                self.schema_version, DIRECTIONAL_LAYOUT_SCHEMA_VERSION
            ));
        }
        require_identity(&self.id, "id")?;
        if self.directions.len() < 2 || self.directions.len() > 8 {
            return Err("directions must contain 2..=8 labels".to_owned());
        }
        let mut directions = BTreeSet::new();
        for direction in &self.directions {
            require_identity(direction, "directions[]")?;
            if !directions.insert(direction.as_str()) {
                return Err(format!("direction {direction} is repeated"));
            }
        }
        if self.actions.is_empty() || self.actions.len() > 32 {
            return Err("actions must contain 1..=32 entries".to_owned());
        }
        let mut action_ids = BTreeSet::new();
        let mut frame_ids = BTreeSet::new();
        let mut rectangles = Vec::new();
        let mut cell_count = 0_usize;
        for action in &self.actions {
            require_identity(&action.id, "actions.id")?;
            require_identity(&action.name, "actions.name")?;
            if !action_ids.insert(action.id.as_str()) {
                return Err(format!("action {} is repeated", action.id));
            }
            if action.frames.is_empty() || action.frames.len() > 64 {
                return Err(format!(
                    "action {} frames must contain 1..=64 entries",
                    action.id
                ));
            }
            for frame in &action.frames {
                require_identity(&frame.id, "frames.id")?;
                require_identity(&frame.name, "frames.name")?;
                if !frame_ids.insert((action.id.as_str(), frame.id.as_str())) {
                    return Err(format!(
                        "frame {} is repeated in action {}",
                        frame.id, action.id
                    ));
                }
                if frame.views.len() != self.directions.len() {
                    return Err(format!(
                        "frame {} in action {} must contain exactly one view per direction",
                        frame.id, action.id
                    ));
                }
                let mut frame_directions = BTreeSet::new();
                let mut present = 0_usize;
                for view in &frame.views {
                    if !directions.contains(view.direction.as_str()) {
                        return Err(format!(
                            "frame {} in action {} names unknown direction {}",
                            frame.id, action.id, view.direction
                        ));
                    }
                    if !frame_directions.insert(view.direction.as_str()) {
                        return Err(format!(
                            "frame {} in action {} repeats direction {}",
                            frame.id, action.id, view.direction
                        ));
                    }
                    if let Some(rect) = &view.rect {
                        present += 1;
                        cell_count += 1;
                        validate_rect(
                            rect,
                            image_width,
                            image_height,
                            &action.id,
                            &frame.id,
                            &view.direction,
                        )?;
                        rectangles.push((
                            action.id.as_str(),
                            frame.id.as_str(),
                            view.direction.as_str(),
                            rect,
                        ));
                    }
                }
                if present == 0 {
                    return Err(format!(
                        "frame {} in action {} must have at least one present view",
                        frame.id, action.id
                    ));
                }
            }
        }
        if cell_count > MAX_CELLS {
            return Err(format!(
                "layout contains {cell_count} cells; maximum is {MAX_CELLS}"
            ));
        }
        for (index, (_, _, _, left)) in rectangles.iter().enumerate() {
            for (_, _, _, right) in rectangles.iter().skip(index + 1) {
                if rectangles_overlap(left, right) {
                    return Err(format!(
                        "sprite rectangles overlap: ({},{},{},{}) and ({},{},{},{})",
                        left.x,
                        left.y,
                        left.width,
                        left.height,
                        right.x,
                        right.y,
                        right.width,
                        right.height
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn inspect_layout(
    root: &Path,
    layout_path: &str,
    output_path: &str,
    action_filter: Option<&str>,
    frame_filter: Option<&str>,
    comparison_path: Option<&str>,
) -> Result<DirectionalInspection, String> {
    let layout = DirectionalSpriteLayout::load(root, layout_path)?;
    let source_path = safe_join(root, &layout.source.path)?;
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "directional sprite source")?;
    let image = decode_png(&source_bytes, &layout.source.background)?;
    layout.validate(image.width, image.height)?;
    let normalized = normalize_layout(&layout, &source_bytes, image.width, image.height)?;
    let output_dir = bounded_local_path(root, output_path)?;
    if output_dir.exists() {
        return Err(format!(
            "output directory already exists: {}",
            output_dir.display()
        ));
    }
    fs::create_dir_all(output_dir.join("crops")).map_err(|error| error.to_string())?;
    let mut crops = BTreeMap::new();
    let mut output_bytes = 0_usize;
    for action in &normalized.actions {
        if action_filter.is_some_and(|value| value != action.id) {
            continue;
        }
        for frame in &action.frames {
            if frame_filter.is_some_and(|value| value != frame.id) {
                continue;
            }
            for view in &frame.views {
                let Some(rect) = &view.rect else { continue };
                let crop = image.crop(rect, &layout.source.background);
                output_bytes = output_bytes
                    .checked_add(crop.len())
                    .ok_or("directional output size overflow")?;
                if output_bytes > MAX_OUTPUT_BYTES {
                    return Err(format!(
                        "generated crops exceed {MAX_OUTPUT_BYTES}-byte bound"
                    ));
                }
                let key = format!("{}--{}--{}", action.id, frame.id, view.direction);
                let path = output_dir.join("crops").join(format!("{key}.png"));
                atomic_write(&path, &crop)?;
                crops.insert(key, crop);
            }
        }
    }
    let normalized_json = format!(
        "{}\n",
        serde_json::to_string_pretty(&normalized).map_err(|error| error.to_string())?
    );
    atomic_write(
        &output_dir.join("layout.normalized.json"),
        normalized_json.as_bytes(),
    )?;
    let contact_sheet_svg = contact_sheet_svg(
        &normalized,
        &crops,
        action_filter,
        frame_filter,
        comparison_path.map(|path| {
            let source = safe_join(root, path)
                .and_then(|path| read_bounded(&path, MAX_SOURCE_BYTES, "comparison render"));
            source.ok().map(|bytes| (path.to_owned(), bytes))
        }),
    )?;
    if contact_sheet_svg.len() > MAX_CONTACT_DIMENSION * MAX_CONTACT_DIMENSION {
        return Err("contact sheet exceeds bounded output size".to_owned());
    }
    atomic_write(
        &output_dir.join("contact-sheet.svg"),
        contact_sheet_svg.as_bytes(),
    )?;
    Ok(DirectionalInspection {
        normalized,
        output_dir,
        crops,
        contact_sheet_svg,
    })
}

fn normalize_layout(
    layout: &DirectionalSpriteLayout,
    source_bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<NormalizedDirectionalLayout, String> {
    let mut actions = layout.actions.clone();
    for action in &mut actions {
        for frame in &mut action.frames {
            frame.views.sort_by_key(|view| {
                layout
                    .directions
                    .iter()
                    .position(|direction| direction == &view.direction)
                    .unwrap_or(usize::MAX)
            });
        }
    }
    let mut missing_views = Vec::new();
    for action in &actions {
        for frame in &action.frames {
            for view in &frame.views {
                if view.rect.is_none() {
                    missing_views.push(MissingView {
                        action: action.id.clone(),
                        frame: frame.id.clone(),
                        direction: view.direction.clone(),
                    });
                }
            }
        }
    }
    let mut covered_area = 0_u64;
    for action in &actions {
        for frame in &action.frames {
            for view in &frame.views {
                if let Some(rect) = &view.rect {
                    covered_area += u64::from(rect.width) * u64::from(rect.height);
                }
            }
        }
    }
    // Validation rejects overlaps, so the sum is the union area.
    Ok(NormalizedDirectionalLayout {
        schema_version: layout.schema_version,
        id: layout.id.clone(),
        source: NormalizedSpriteSource {
            path: layout.source.path.clone(),
            width,
            height,
            byte_count: source_bytes.len(),
            sha256: sha256(source_bytes),
            background: layout.source.background.clone(),
        },
        directions: layout.directions.clone(),
        actions,
        source_area: u64::from(width) * u64::from(height),
        covered_area,
        unused_area: u64::from(width) * u64::from(height) - covered_area,
        missing_views,
    })
}

fn contact_sheet_svg(
    normalized: &NormalizedDirectionalLayout,
    crops: &BTreeMap<String, Vec<u8>>,
    action_filter: Option<&str>,
    frame_filter: Option<&str>,
    comparison: Option<Option<(String, Vec<u8>)>>,
) -> Result<String, String> {
    let mut rows = Vec::new();
    for action in &normalized.actions {
        if action_filter.is_some_and(|value| value != action.id) {
            continue;
        }
        for frame in &action.frames {
            if frame_filter.is_some_and(|value| value != frame.id) {
                continue;
            }
            rows.push((action, frame));
        }
    }
    if rows.is_empty() {
        return Err("the selected action/frame has no matching views".to_owned());
    }
    let card_width = 220_u32;
    let card_height = 190_u32;
    let width = card_width
        .checked_mul(u32::try_from(normalized.directions.len()).map_err(|_| "direction count")?)
        .ok_or("contact sheet width overflow")?;
    let height = card_height
        .checked_mul(u32::try_from(rows.len()).map_err(|_| "row count")?)
        .and_then(|value| value.checked_add(48))
        .ok_or("contact sheet height overflow")?;
    if usize::try_from(width).unwrap_or(usize::MAX) > MAX_CONTACT_DIMENSION
        || usize::try_from(height).unwrap_or(usize::MAX) > MAX_CONTACT_DIMENSION
    {
        return Err("contact sheet dimensions exceed bound".to_owned());
    }
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><rect width=\"100%\" height=\"100%\" fill=\"#16181d\"/><text x=\"12\" y=\"22\" fill=\"#f5f7fa\" font-family=\"monospace\" font-size=\"14\">{}</text>",
        escape_xml(&format!("{} — explicit sprite layout; missing views are not synthesized", normalized.id))
    );
    let render_uri =
        comparison.and_then(|value| value.map(|(path, bytes)| data_uri(&path, &bytes)));
    for (row_index, (action, frame)) in rows.iter().enumerate() {
        let y = 48 + u32::try_from(row_index).unwrap_or(0) * card_height;
        for (column, direction) in normalized.directions.iter().enumerate() {
            let x = u32::try_from(column).unwrap_or(0) * card_width;
            svg.push_str(&format!(
                "<g transform=\"translate({x} {y})\"><rect x=\"4\" y=\"4\" width=\"212\" height=\"182\" rx=\"5\" fill=\"#242831\" stroke=\"#4a5260\"/><text x=\"10\" y=\"20\" fill=\"#ffffff\" font-family=\"monospace\" font-size=\"12\">{}</text>",
                escape_xml(&format!("{} / {} / {}", action.name, frame.name, direction))
            ));
            let view = frame.views.iter().find(|view| view.direction == *direction);
            if let Some(Some(rect)) = view.map(|view| view.rect.as_ref()) {
                let key = format!("{}--{}--{}", action.id, frame.id, direction);
                if let Some(bytes) = crops.get(&key) {
                    let uri = data_uri("crop.png", bytes);
                    let image_height = rect.height.min(128);
                    let image_width = rect.width.min(128);
                    let image_x = 18 + (128 - image_width) / 2;
                    let image_y = 30 + (128 - image_height) / 2;
                    svg.push_str(&format!(
                        "<image x=\"{image_x}\" y=\"{image_y}\" width=\"{image_width}\" height=\"{image_height}\" href=\"{uri}\" preserveAspectRatio=\"none\" style=\"image-rendering:pixelated\"/><line x1=\"18\" y1=\"164\" x2=\"146\" y2=\"164\" stroke=\"#e6c453\" stroke-dasharray=\"3 2\"/><line x1=\"82\" y1=\"30\" x2=\"82\" y2=\"158\" stroke=\"#e6c453\" stroke-dasharray=\"3 2\"/><text x=\"151\" y=\"52\" fill=\"#cbd5e1\" font-family=\"monospace\" font-size=\"9\">rect</text><text x=\"151\" y=\"65\" fill=\"#cbd5e1\" font-family=\"monospace\" font-size=\"9\">{},{}</text><text x=\"151\" y=\"78\" fill=\"#cbd5e1\" font-family=\"monospace\" font-size=\"9\">{}x{}</text>",
                        rect.x, rect.y, rect.width, rect.height
                    ));
                }
                if let Some(anchor) = view.and_then(|view| view.anchor.as_ref()) {
                    svg.push_str(&format!(
                        "<circle cx=\"{}\" cy=\"{}\" r=\"4\" fill=\"#ff6b6b\"/><text x=\"151\" y=\"103\" fill=\"#ff9b9b\" font-family=\"monospace\" font-size=\"9\">anchor {},{}</text>",
                        18 + anchor.x.clamp(0, 128), 30 + anchor.y.clamp(0, 128), anchor.x, anchor.y
                    ));
                }
            } else {
                svg.push_str("<text x=\"28\" y=\"104\" fill=\"#ffca73\" font-family=\"monospace\" font-size=\"14\">MISSING (explicit)</text>");
            }
            if let Some(uri) = &render_uri {
                svg.push_str(&format!("<image x=\"151\" y=\"110\" width=\"54\" height=\"54\" href=\"{uri}\" preserveAspectRatio=\"xMidYMid meet\"/><text x=\"151\" y=\"176\" fill=\"#9fe3b1\" font-family=\"monospace\" font-size=\"8\">voxel compare</text>"));
            }
            svg.push_str("</g>");
        }
    }
    svg.push_str("</svg>\n");
    Ok(svg)
}

fn decode_png(bytes: &[u8], background: &SpriteBackground) -> Result<RgbaImage, String> {
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
    let pixel_count = u64::from(info.width) * u64::from(info.height);
    if pixel_count > u64::from(MAX_SOURCE_DIMENSION) * u64::from(MAX_SOURCE_DIMENSION) {
        return Err("PNG pixel count exceeds bound".to_owned());
    }
    let mut buffer = vec![0; reader.output_buffer_size()];
    let output = reader
        .next_frame(&mut buffer)
        .map_err(|error| format!("PNG frame decode failed: {error}"))?;
    if output.bit_depth != BitDepth::Eight {
        return Err("PNG must decode to 8-bit channels".to_owned());
    }
    let raw = &buffer[..output.buffer_size()];
    let mut pixels =
        Vec::with_capacity(usize::try_from(pixel_count * 4).map_err(|_| "PNG size overflow")?);
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
        if background.is_key(pixel) {
            pixel[3] = 0;
        }
    }
    Ok(RgbaImage {
        width: output.width,
        height: output.height,
        pixels,
    })
}

impl RgbaImage {
    fn crop(&self, rect: &SpriteRect, background: &SpriteBackground) -> Vec<u8> {
        let mut pixels =
            Vec::with_capacity(usize::try_from(rect.width * rect.height * 4).unwrap_or(0));
        for y in rect.y..rect.y + rect.height {
            let start = usize::try_from((y * self.width + rect.x) * 4).unwrap_or(0);
            let end = start + usize::try_from(rect.width * 4).unwrap_or(0);
            pixels.extend_from_slice(&self.pixels[start..end]);
        }
        encode_png(rect.width, rect.height, &pixels, background)
    }
}

fn encode_png(width: u32, height: u32, pixels: &[u8], background: &SpriteBackground) -> Vec<u8> {
    let mut output = Vec::new();
    let mut encoder = png::Encoder::new(&mut output, width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder.write_header().expect("in-memory PNG header");
    let mut pixels = pixels.to_vec();
    for pixel in pixels.chunks_exact_mut(4) {
        if background.is_key(pixel) {
            pixel[3] = 0;
        }
    }
    writer
        .write_image_data(&pixels)
        .expect("in-memory PNG body");
    drop(writer);
    output
}

impl SpriteBackground {
    fn is_key(&self, pixel: &[u8]) -> bool {
        self.color_key.as_ref().is_some_and(|key| pixel == key)
            || self.color_keys.iter().any(|key| pixel == key)
    }
}

fn validate_rect(
    rect: &SpriteRect,
    width: u32,
    height: u32,
    action: &str,
    frame: &str,
    direction: &str,
) -> Result<(), String> {
    if rect.width == 0 || rect.height == 0 {
        return Err(format!(
            "{action}/{frame}/{direction} has an empty rectangle"
        ));
    }
    if rect
        .x
        .checked_add(rect.width)
        .is_none_or(|value| value > width)
        || rect
            .y
            .checked_add(rect.height)
            .is_none_or(|value| value > height)
    {
        return Err(format!(
            "{action}/{frame}/{direction} rectangle is outside source bounds"
        ));
    }
    Ok(())
}

fn rectangles_overlap(left: &SpriteRect, right: &SpriteRect) -> bool {
    left.x < right.x + right.width
        && right.x < left.x + left.width
        && left.y < right.y + right.height
        && right.y < left.y + left.height
}

fn bounded_local_path(root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative_path);
    if !matches!(path.components().next(), Some(std::path::Component::Normal(value)) if value == "local")
    {
        return Err("directional sprite outputs must be under ignored local/".to_owned());
    }
    safe_join(root, relative_path)
}

fn require_identity(value: &str, path: &str) -> Result<(), String> {
    if value.is_empty() || value != value.trim() || value.len() > 64 || value.contains('/') {
        Err(format!(
            "{path} must be short canonical text without '/': {value:?}"
        ))
    } else {
        Ok(())
    }
}

fn data_uri(path: &str, bytes: &[u8]) -> String {
    let mime = if path.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "image/png"
    };
    format!("data:{mime};base64,{}", base64_encode(bytes))
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> DirectionalSpriteLayout {
        DirectionalSpriteLayout {
            schema_version: 1,
            id: "test".to_owned(),
            source: DirectionalSpriteSource {
                path: "local/test.png".to_owned(),
                background: SpriteBackground::default(),
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
                    id: "f0".to_owned(),
                    name: "Frame 0".to_owned(),
                    views: vec![
                        (
                            "front",
                            Some(SpriteRect {
                                x: 0,
                                y: 0,
                                width: 4,
                                height: 4,
                            }),
                        ),
                        (
                            "right",
                            Some(SpriteRect {
                                x: 4,
                                y: 0,
                                width: 4,
                                height: 4,
                            }),
                        ),
                        ("back", None),
                        ("left", None),
                    ]
                    .into_iter()
                    .map(|(direction, rect)| DirectionalSpriteView {
                        direction: direction.to_owned(),
                        rect,
                        anchor: None,
                    })
                    .collect(),
                }],
            }],
        }
    }

    #[test]
    fn rejects_overlap_and_empty_frames() {
        let mut value = layout();
        value.actions[0].frames[0].views[1].rect = Some(SpriteRect {
            x: 2,
            y: 0,
            width: 4,
            height: 4,
        });
        let error = value.validate(16, 16).expect_err("overlap");
        assert!(error.contains("overlap"), "{error}");
        value.actions[0].frames[0].views[0].rect = None;
        value.actions[0].frames[0].views[1].rect = None;
        let error = value.validate(16, 16).expect_err("empty");
        assert!(error.contains("at least one"), "{error}");
    }

    #[test]
    fn accepts_explicit_missing_views_and_bounds() {
        let value = layout();
        value.validate(16, 16).expect("layout");
    }
}
