use std::collections::BTreeMap;
use std::path::Path;

use asset_catalog::{
    validate_catalog, AssetCatalog, AtlasInset, AtlasPadding, AtlasRegionDefinition, CatalogEntry,
    MaterialAuthority, MaterialDefinition, MaterialStyle, Rgba, StructuralClass, TextureDefinition,
    TextureFilter as CatalogTextureFilter, TextureWrap as CatalogTextureWrap, UvStrategy,
    VoxelAlphaMode, VoxelAtlasDefinition, VoxelSurfaceBinding, VoxelSurfaceMapping,
};
use core_assets::{AssetHash, AssetId, AssetReference, AssetVersionReq};
use render_model::{
    RenderMaterialDescriptor, TextureDescriptor, TextureFilter, TexturePayloadSource, TextureWrap,
};
use render_projection::project_catalog_material;
use serde::Serialize;

use crate::model::{
    ProjectAtlas, ProjectAtlasPadding, ProjectMaterial, ProjectTextureFilter, ProjectTextureWrap,
    ProjectVoxelAlphaMode, ProjectVoxelSurfaceMapping, VoxelLabProject,
};
use crate::project::{read_bounded, safe_join, sha256, LoadedProject};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureResourceReadout {
    pub resource: String,
    pub content_hash: String,
    pub byte_length: u32,
    pub source_path: String,
}

#[derive(Debug, Clone)]
pub struct SurfaceAssets {
    pub catalog: AssetCatalog,
    pub texture_descriptors: Vec<TextureDescriptor>,
    pub texture_resources: Vec<TextureResourceReadout>,
    pub render_materials: BTreeMap<String, RenderMaterialDescriptor>,
}

pub fn load_surface_assets(loaded: &LoadedProject) -> Result<SurfaceAssets, String> {
    load_surface_assets_from_project(&loaded.root, &loaded.project)
}

pub fn load_surface_assets_from_project(
    root: &Path,
    project: &VoxelLabProject,
) -> Result<SurfaceAssets, String> {
    load_surface_assets_with_pending_texture(root, project, None)
}

pub fn load_surface_assets_with_pending_texture(
    root: &Path,
    project: &VoxelLabProject,
    pending_texture: Option<(&str, &[u8])>,
) -> Result<SurfaceAssets, String> {
    project.validate()?;
    let mut catalog_entries = Vec::with_capacity(
        project.textures.len() + project.atlases.len() + project.materials.len(),
    );
    let mut texture_descriptors = Vec::with_capacity(project.textures.len());
    let mut texture_resources = Vec::with_capacity(project.textures.len());

    for texture in &project.textures {
        let bytes = if pending_texture
            .as_ref()
            .is_some_and(|(source_path, _)| *source_path == texture.source_path)
        {
            pending_texture
                .expect("pending texture was checked")
                .1
                .to_vec()
        } else {
            let path = safe_join(root, &texture.source_path)?;
            read_bounded(&path, 16 * 1024 * 1024, "PNG texture")?
        };
        let descriptor = TextureDescriptor::admit_png_rgba8_resource(
            texture.asset_id.clone(),
            &bytes,
            render_filter(texture.filter),
            render_wrap(texture.wrap),
            texture.version,
        )
        .map_err(|error| format!("{}: {error:?}", texture.source_path))?;
        if descriptor.width != texture.width
            || descriptor.height != texture.height
            || descriptor.content_hash.as_deref() != Some(texture.content_hash.as_str())
            || descriptor
                .payload
                .as_ref()
                .is_none_or(|payload| payload.byte_length != texture.encoded_byte_length)
        {
            return Err(format!(
                "{} does not match its admitted texture metadata",
                texture.source_path
            ));
        }
        let resource = match descriptor.payload.as_ref().map(|payload| &payload.source) {
            Some(TexturePayloadSource::Resource { resource }) => resource.clone(),
            _ => return Err("admitted texture did not retain a resource payload".to_owned()),
        };
        texture_resources.push(TextureResourceReadout {
            resource,
            content_hash: texture.content_hash.clone(),
            byte_length: texture.encoded_byte_length,
            source_path: texture.source_path.clone(),
        });
        texture_descriptors.push(descriptor);
        catalog_entries.push(
            CatalogEntry::new(asset_id(&texture.asset_id)?, texture.version)
                .with_hash(asset_hash(&texture.content_hash)?)
                .with_source(texture.source_path.clone())
                .with_texture(TextureDefinition {
                    width: texture.width,
                    height: texture.height,
                    filter: catalog_filter(texture.filter),
                    wrap: catalog_wrap(texture.wrap),
                }),
        );
    }

    for atlas in &project.atlases {
        if atlas_content_hash(atlas)? != atlas.content_hash {
            return Err(format!("atlas {} content hash drifted", atlas.asset_id));
        }
        let texture_reference = pinned_reference(
            &atlas.texture_asset_id,
            atlas.texture_version,
            &atlas.texture_content_hash,
        )?;
        catalog_entries.push(
            CatalogEntry::new(asset_id(&atlas.asset_id)?, atlas.version)
                .with_hash(asset_hash(&atlas.content_hash)?)
                .with_dependencies(vec![texture_reference.clone()])
                .with_voxel_atlas(VoxelAtlasDefinition {
                    schema_version: 1,
                    texture: texture_reference,
                    regions: atlas.regions.iter().map(atlas_region).collect(),
                }),
        );
    }

    for material in &project.materials {
        if material.voxel_surface.is_some()
            && material_content_hash(material)? != material.content_hash.clone().unwrap_or_default()
        {
            return Err(format!(
                "material {} content hash drifted",
                material.asset_id
            ));
        }
        let mut definition = material_definition(material);
        let mut dependencies = Vec::new();
        if let Some(surface) = &material.voxel_surface {
            let texture = pinned_reference(
                &surface.texture_asset_id,
                surface.texture_version,
                &surface.texture_content_hash,
            )?;
            definition.style.texture = Some(texture.clone());
            let mapping = match &surface.mapping {
                ProjectVoxelSurfaceMapping::Repeat {
                    tile_scale_cells,
                    tile_origin_cells,
                } => {
                    dependencies.push(texture.clone());
                    definition.style.uv_strategy = UvStrategy::Planar;
                    VoxelSurfaceMapping::Repeat {
                        texture,
                        tile_scale_cells: *tile_scale_cells,
                        tile_origin_cells: *tile_origin_cells,
                    }
                }
                ProjectVoxelSurfaceMapping::Atlas {
                    atlas_asset_id,
                    atlas_version,
                    atlas_content_hash,
                    region_id,
                    tile_scale_cells,
                    tile_origin_cells,
                } => {
                    let atlas =
                        pinned_reference(atlas_asset_id, *atlas_version, atlas_content_hash)?;
                    dependencies.push(atlas.clone());
                    definition.style.uv_strategy = UvStrategy::Atlas;
                    VoxelSurfaceMapping::Atlas {
                        atlas,
                        region: region_id.clone(),
                        tile_scale_cells: *tile_scale_cells,
                        tile_origin_cells: *tile_origin_cells,
                    }
                }
            };
            definition.style.voxel_surface = Some(VoxelSurfaceBinding {
                schema_version: 1,
                mapping,
                alpha_mode: alpha_mode(surface.alpha_mode),
            });
        }
        let mut entry = CatalogEntry::new(asset_id(&material.asset_id)?, material.version)
            .with_dependencies(dependencies)
            .with_material(definition);
        if let Some(hash) = &material.content_hash {
            entry = entry.with_hash(asset_hash(hash)?);
        }
        catalog_entries.push(entry);
    }

    let catalog = AssetCatalog::from_entries(catalog_entries).canonical();
    let report = validate_catalog(&catalog);
    if !report.is_ok() {
        return Err(format!(
            "voxel surface catalog rejected: {}",
            report
                .errors
                .iter()
                .map(|error| error.code())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let render_materials = project
        .materials
        .iter()
        .map(|material| {
            let id = asset_id(&material.asset_id)?;
            let descriptor = project_catalog_material(&catalog, &id).map_err(|error| {
                format!(
                    "material {} projection rejected: {error:?}",
                    material.asset_id
                )
            })?;
            Ok((material.asset_id.clone(), descriptor))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    texture_descriptors.sort_by(|left, right| left.id.cmp(&right.id));
    texture_resources.sort_by(|left, right| left.resource.cmp(&right.resource));
    Ok(SurfaceAssets {
        catalog,
        texture_descriptors,
        texture_resources,
        render_materials,
    })
}

pub fn canonical_texture_path(content_hash: &str) -> Result<String, String> {
    let digest = content_hash
        .strip_prefix("sha256:")
        .filter(|digest| digest.len() == 64)
        .ok_or_else(|| "texture content hash is not canonical SHA-256".to_owned())?;
    Ok(format!(".rusty-engine/textures/{digest}.png"))
}

pub fn atlas_content_hash(atlas: &ProjectAtlas) -> Result<String, String> {
    canonical_hash(&(
        &atlas.asset_id,
        atlas.version,
        &atlas.texture_asset_id,
        atlas.texture_version,
        &atlas.texture_content_hash,
        &atlas.regions,
    ))
}

pub fn material_content_hash(material: &ProjectMaterial) -> Result<String, String> {
    canonical_hash(&(
        &material.asset_id,
        &material.display_name,
        material.color,
        material.roughness,
        material.texture_tint,
        material.emission_color,
        material.emissive,
        material.version,
        &material.voxel_surface,
    ))
}

fn canonical_hash(value: &impl Serialize) -> Result<String, String> {
    serde_json::to_vec(value)
        .map(|bytes| sha256(&bytes))
        .map_err(|error| error.to_string())
}

fn asset_id(value: &str) -> Result<AssetId, String> {
    AssetId::parse(value).map_err(|error| format!("invalid asset id {value}: {error:?}"))
}

fn asset_hash(value: &str) -> Result<AssetHash, String> {
    let digest = value.strip_prefix("sha256:").unwrap_or(value);
    AssetHash::parse(digest).map_err(|error| format!("invalid asset hash {value}: {error:?}"))
}

fn pinned_reference(value: &str, version: u32, hash: &str) -> Result<AssetReference, String> {
    Ok(AssetReference::new(
        asset_id(value)?,
        AssetVersionReq::Exact(version),
        Some(asset_hash(hash)?),
    ))
}

fn atlas_region(region: &crate::model::ProjectAtlasRegion) -> AtlasRegionDefinition {
    AtlasRegionDefinition {
        id: region.id.clone(),
        content_min: region.content_min,
        content_extent: region.content_extent,
        padding: atlas_padding(region.padding),
        inset: AtlasInset::HalfTexel,
    }
}

fn atlas_padding(padding: ProjectAtlasPadding) -> AtlasPadding {
    AtlasPadding {
        left: padding.left,
        right: padding.right,
        bottom: padding.bottom,
        top: padding.top,
    }
}

fn material_definition(material: &ProjectMaterial) -> MaterialDefinition {
    MaterialDefinition {
        authority: MaterialAuthority {
            solid: false,
            collidable: false,
            occludes: false,
            structural_class: StructuralClass::Decorative,
        },
        style: MaterialStyle {
            color: rgba(material.color),
            texture: None,
            roughness: material.roughness,
            texture_tint: rgba(material.texture_tint),
            emission_color: rgba(material.emission_color),
            emissive: material.emissive,
            uv_strategy: UvStrategy::Flat,
            voxel_surface: None,
        },
    }
}

fn rgba(value: [f32; 4]) -> Rgba {
    Rgba {
        r: value[0],
        g: value[1],
        b: value[2],
        a: value[3],
    }
}

fn alpha_mode(value: ProjectVoxelAlphaMode) -> VoxelAlphaMode {
    match value {
        ProjectVoxelAlphaMode::Opaque => VoxelAlphaMode::Opaque,
        ProjectVoxelAlphaMode::Mask { cutoff } => VoxelAlphaMode::Mask { cutoff },
        ProjectVoxelAlphaMode::Blend => VoxelAlphaMode::Blend,
    }
}

fn render_filter(value: ProjectTextureFilter) -> TextureFilter {
    match value {
        ProjectTextureFilter::Nearest => TextureFilter::Nearest,
        ProjectTextureFilter::Linear => TextureFilter::Linear,
    }
}

fn render_wrap(value: ProjectTextureWrap) -> TextureWrap {
    match value {
        ProjectTextureWrap::Repeat => TextureWrap::Repeat,
        ProjectTextureWrap::Clamp => TextureWrap::Clamp,
    }
}

fn catalog_filter(value: ProjectTextureFilter) -> CatalogTextureFilter {
    match value {
        ProjectTextureFilter::Nearest => CatalogTextureFilter::Nearest,
        ProjectTextureFilter::Linear => CatalogTextureFilter::Linear,
    }
}

fn catalog_wrap(value: ProjectTextureWrap) -> CatalogTextureWrap {
    match value {
        ProjectTextureWrap::Repeat => CatalogTextureWrap::Repeat,
        ProjectTextureWrap::Clamp => CatalogTextureWrap::Clamp,
    }
}
