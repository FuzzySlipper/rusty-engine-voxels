use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use render_model::{TextureDescriptor, TextureFilter, TextureWrap};
use render_projection::VoxelObjectRenderProjector;
use rusty_engine::{
    render_model, render_projection, voxel_asset, voxel_convert, voxel_object_runtime,
};
use serde::Deserialize;
use serde_json::{json, Value};
use voxel_asset::{represented_voxel_count, VoxelObjectAsset, VoxelObjectProvenanceKind};
use voxel_convert::{
    apply_voxel_object_conversion, import_animated_mesh_source, import_mesh_source,
    plan_animated_voxel_object_conversion, plan_static_voxel_object_conversion,
    preview_voxel_object_conversion, AnimationProperty, MeshSourceFormat, MeshSourceImportRequest,
    PreparedVoxelObjectConversion, VoxelObjectConversionApplyRequest,
    VoxelObjectConversionPlanRequest, VoxelObjectConversionPreviewRequest,
    VoxelObjectConversionSettings, VoxelObjectFrameSelection,
};
use voxel_object_runtime::{admit_voxel_object, AdmittedVoxelObject, VoxelObjectRuntimeLimits};

use crate::conversion::{publish_project_conversion, PreparedProjectConversion};
use crate::mesh_resource_cache::{
    merge_mesh_resource_readouts, publish_mesh_resources, MeshResourceReadout,
};
use crate::model::{
    experiment_color, ProjectAtlas, ProjectAtlasPadding, ProjectAtlasRegion,
    ProjectCollisionPolicy, ProjectFrameSelection, ProjectMaterial, ProjectMaterialOverride,
    ProjectTexture, ProjectTextureFilter, ProjectTextureWrap, ProjectVoxelAlphaMode,
    ProjectVoxelObjectInstance, ProjectVoxelSurface, ProjectVoxelSurfaceMapping,
    MAX_JSON_SAFE_ENTITY_ID,
};
use crate::project::{
    atomic_write, load_project, read_bounded, safe_join, save_project, stage_project,
    LoadedProject, MAX_SOURCE_BYTES,
};
use crate::runtime::{
    complete_packed_projection, load_runtime_project, load_runtime_project_from_loaded,
    load_runtime_project_from_loaded_with_surface, project_runtime_with_instance_frame,
    projection_for_object, resolve_frame, RuntimeProject,
};
use crate::studio_playback::{PlaybackCommand, StudioPlaybackError, StudioVoxelObjectPlayback};
use crate::surface::{
    atlas_content_hash, canonical_texture_path, load_surface_assets_with_pending_texture,
    material_content_hash,
};

const PROTOCOL_VERSION: u64 = 14;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
const MAX_VOXEL_OBJECT_INSTANCE_BATCH: usize = 32;
const VOXEL_OBJECT_COMPONENT_TYPE_ID: &str = "rusty.voxel-object.instance";
const VOXEL_OBJECT_INSPECTOR_CONTRACT_ID: &str = "rusty.studio.voxel-object-authoring";

const OPERATIONS: &[&str] = &[
    "describe",
    "openProject",
    "createProject",
    "saveProjectAs",
    "readProject",
    "createScene",
    "renameScene",
    "deleteScene",
    "setEntryScene",
    "createSceneObject",
    "deleteSceneObject",
    "renameSceneObject",
    "reparentSceneObject",
    "setSceneObjectTransform",
    "setSceneObjectRenderableTransform",
    "setSceneObjectAppearance",
    "setEntityCollision",
    "setEntityKinematic",
    "setEntityTranslation",
    "upsertMaterial",
    "upsertVoxelSurfaceMaterial",
    "removeVoxelSurfaceMaterial",
    "prepareAssetImport",
    "prepareAssetReimport",
    "applyAssetImport",
    "discardAssetImport",
    "initializeVoxelAsset",
    "duplicateVoxelAsset",
    "attachVoxelInstance",
    "setVoxelInstanceTransform",
    "removeVoxelInstance",
    "replaceVoxelPalette",
    "validateVoxelPick",
    "applyVoxelBrush",
    "applyVoxelPrimitive",
    "initializeVoxelTemplate",
    "importVoxelAssetFile",
    "exportVoxelAssetFile",
    "materializeEnvironment",
    "undoVoxelEdit",
    "redoVoxelEdit",
    "revertVoxelHistory",
    "queryVoxelHistory",
    "prepareVoxelHistoryRevert",
    "applyVoxelHistoryRevert",
    "discardVoxelHistoryRevert",
    "createVoxelAnnotationLayer",
    "editVoxelAnnotation",
    "queryVoxelAnnotation",
    "exportVoxelAnnotation",
    "queryVoxelModel",
    "prepareVoxelConversion",
    "applyVoxelConversion",
    "discardVoxelConversion",
    "inspectVoxelObjectSource",
    "prepareVoxelObjectConversion",
    "previewVoxelObjectConversion",
    "applyVoxelObjectConversion",
    "discardVoxelObjectConversion",
    "prepareVoxelObjectPlacement",
    "attachVoxelObjectInstance",
    "attachVoxelObjectInstances",
    "previewVoxelObjectInstance",
    "closeProject",
];

pub fn run_stdio() -> Result<(), String> {
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut adapter = StudioAdapter::default();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| error.to_string())?;
        let response = if line.len() > MAX_REQUEST_BYTES {
            rejection(
                None,
                AdapterError::new("adapter.requestLimit", "request exceeds 256 KiB"),
            )
        } else {
            match serde_json::from_str::<Value>(&line) {
                Ok(request) => adapter
                    .dispatch(request)
                    .unwrap_or_else(|error| rejection(adapter.last_request_id.as_deref(), error)),
                Err(error) => rejection(
                    None,
                    AdapterError::new(
                        "adapter.requestDecode",
                        format!("request is not JSON: {error}"),
                    ),
                ),
            }
        };
        serde_json::to_writer(&mut stdout, &response).map_err(|error| error.to_string())?;
        stdout.write_all(b"\n").map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[derive(Default)]
pub struct StudioAdapter {
    open: Option<LoadedProject>,
    runtime: Option<RuntimeProject>,
    projector: VoxelObjectRenderProjector,
    mesh_resources: Vec<MeshResourceReadout>,
    prepared: Option<PreparedCandidate>,
    playback: StudioVoxelObjectPlayback,
    last_request_id: Option<String>,
}

struct PreparedCandidate {
    expected_project_hash: String,
    prepared: PreparedVoxelObjectConversion,
    materials: Vec<ProjectMaterial>,
}

impl StudioAdapter {
    pub fn dispatch(&mut self, request: Value) -> Result<Value, AdapterError> {
        let request_id = required_string(&request, "requestId")?;
        self.last_request_id = Some(request_id.clone());
        let protocol = request
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AdapterError::at(
                    "adapter.requestDecode",
                    "protocolVersion",
                    "protocolVersion must be an integer",
                )
            })?;
        if protocol != PROTOCOL_VERSION {
            return Err(AdapterError::at(
                "adapter.protocolVersion",
                "protocolVersion",
                format!("expected protocol {PROTOCOL_VERSION}, received {protocol}"),
            ));
        }
        let operation = required_string(&request, "type")?;
        match operation.as_str() {
            "describe" => Ok(self.describe(&request_id)),
            "openProject" => self.open_project(&request_id, &request),
            "readProject" => self.read_project(&request_id),
            "upsertVoxelSurfaceMaterial" => {
                self.upsert_voxel_surface_material(&request_id, request)
            }
            "removeVoxelSurfaceMaterial" => {
                self.remove_voxel_surface_material(&request_id, request)
            }
            "inspectVoxelObjectSource" => self.inspect_source(&request_id, request),
            "prepareVoxelObjectConversion" => self.prepare_conversion(&request_id, request),
            "previewVoxelObjectConversion" => self.preview_conversion(&request_id, request),
            "applyVoxelObjectConversion" => self.apply_conversion(&request_id, request),
            "discardVoxelObjectConversion" => self.discard_conversion(&request_id, request),
            "attachVoxelObjectInstance" => self.attach_instance(&request_id, request),
            "attachVoxelObjectInstances" => self.attach_instances(&request_id, request),
            "previewVoxelObjectInstance" => self.preview_instance(&request_id, request),
            "closeProject" => {
                self.open = None;
                self.runtime = None;
                self.projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
                self.mesh_resources.clear();
                self.prepared = None;
                self.playback.clear();
                Ok(response("projectClosed", &request_id, json!({})))
            }
            _ => Err(AdapterError::at(
                "adapter.unsupportedOperation",
                "type",
                format!("{operation} is not owned by the voxel experiment adapter"),
            )),
        }
    }

    fn describe(&self, request_id: &str) -> Value {
        response(
            "described",
            request_id,
            json!({
                "adapter": {
                    "adapterId": "rusty-engine-voxels.voxel-lab",
                    "adapterVersion": 1,
                    "protocolVersion": PROTOCOL_VERSION,
                    "projectKind": "rustyEngineVoxelLab",
                    "projectSchemaVersion": 1,
                    "operations": OPERATIONS,
                    "entityInspectorContracts": [{
                        "contractId": VOXEL_OBJECT_INSPECTOR_CONTRACT_ID,
                        "contractVersion": 1,
                    }],
                }
            }),
        )
    }

    fn open_project(&mut self, request_id: &str, request: &Value) -> Result<Value, AdapterError> {
        let root = required_string(request, "root")?;
        let project_file = required_string(request, "projectFile")?;
        let runtime =
            load_runtime_project(Path::new(&root), &project_file).map_err(AdapterError::project)?;
        let readout = self.accept_runtime_project(runtime)?;
        self.prepared = None;
        Ok(response(
            "projectOpened",
            request_id,
            json!({ "project": readout }),
        ))
    }

    fn read_project(&mut self, request_id: &str) -> Result<Value, AdapterError> {
        let current = self.require_open()?;
        let root = current.root.clone();
        let relative_path = current.relative_path.clone();
        let runtime = load_runtime_project(&root, &relative_path).map_err(AdapterError::project)?;
        let readout = self.accept_runtime_project(runtime)?;
        Ok(response(
            "projectRead",
            request_id,
            json!({ "project": readout }),
        ))
    }

    fn upsert_voxel_surface_material(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: UpsertVoxelSurfaceMaterialRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        validate_surface_material_intent(&input.material)?;
        if input.assignment.scene_id != self.require_open()?.project.entry_scene {
            return Err(AdapterError::at(
                "project.unknownScene",
                "assignment.sceneId",
                "surface assignment must name the entry scene",
            ));
        }

        let open = self.require_open()?.clone();
        let mut project = open.project.clone();
        let existing_texture = project
            .textures
            .iter()
            .find(|texture| texture.asset_id == input.texture_asset_id)
            .cloned();
        require_expected_hash(
            "texture",
            &input.texture_asset_id,
            input.expected_texture_content_hash.as_deref(),
            existing_texture
                .as_ref()
                .map(|texture| texture.content_hash.as_str()),
        )?;

        let (_, texture_bytes) = self.read_selection(&input.texture_source)?;
        let texture_wrap = match input.material.mapping {
            VoxelSurfaceMappingDraft::Repeat { .. } => ProjectTextureWrap::Repeat,
            VoxelSurfaceMappingDraft::Atlas { .. } => ProjectTextureWrap::Clamp,
        };
        let provisional_version = existing_texture.as_ref().map_or(1, |texture| {
            if texture.filter == input.filter && texture.wrap == texture_wrap {
                texture.version
            } else {
                texture.version.saturating_add(1)
            }
        });
        let descriptor = TextureDescriptor::admit_png_rgba8_resource(
            input.texture_asset_id.clone(),
            &texture_bytes,
            render_texture_filter(input.filter),
            render_texture_wrap(texture_wrap),
            provisional_version,
        )
        .map_err(|error| {
            AdapterError::at(
                "surface.textureRejected",
                "textureSource",
                format!("PNG texture admission failed: {error:?}"),
            )
        })?;
        let texture_content_hash = descriptor.content_hash.clone().ok_or_else(|| {
            AdapterError::at(
                "surface.textureRejected",
                "textureSource",
                "PNG texture admission did not produce a content hash",
            )
        })?;
        let texture_version = existing_texture.as_ref().map_or(1, |texture| {
            if texture.content_hash == texture_content_hash
                && texture.filter == input.filter
                && texture.wrap == texture_wrap
            {
                texture.version
            } else {
                texture.version.saturating_add(1)
            }
        });
        let texture_source_path =
            canonical_texture_path(&texture_content_hash).map_err(AdapterError::project)?;
        let encoded_byte_length = u32::try_from(texture_bytes.len()).map_err(|_| {
            AdapterError::at(
                "surface.textureRejected",
                "textureSource",
                "PNG byte length does not fit the project format",
            )
        })?;
        let texture = ProjectTexture {
            asset_id: input.texture_asset_id.clone(),
            version: texture_version,
            content_hash: texture_content_hash.clone(),
            source_path: texture_source_path.clone(),
            width: descriptor.width,
            height: descriptor.height,
            encoded_byte_length,
            filter: input.filter,
            wrap: texture_wrap,
        };
        project
            .textures
            .retain(|entry| entry.asset_id != texture.asset_id);
        project.textures.push(texture.clone());
        project
            .textures
            .sort_by(|left, right| left.asset_id.cmp(&right.asset_id));

        let (surface_mapping, atlas_receipt) = match input.material.mapping {
            VoxelSurfaceMappingDraft::Repeat {
                tile_scale_cells,
                tile_origin_cells,
            } => (
                ProjectVoxelSurfaceMapping::Repeat {
                    tile_scale_cells,
                    tile_origin_cells,
                },
                None,
            ),
            VoxelSurfaceMappingDraft::Atlas {
                atlas_asset_id,
                expected_atlas_content_hash,
                regions,
                region_id,
                tile_scale_cells,
                tile_origin_cells,
            } => {
                let existing_atlas = project
                    .atlases
                    .iter()
                    .find(|atlas| atlas.asset_id == atlas_asset_id)
                    .cloned();
                require_expected_hash(
                    "atlas",
                    &atlas_asset_id,
                    expected_atlas_content_hash.as_deref(),
                    existing_atlas
                        .as_ref()
                        .map(|atlas| atlas.content_hash.as_str()),
                )?;
                let regions = regions
                    .into_iter()
                    .map(|region| {
                        if region.inset != "halfTexel" {
                            return Err(AdapterError::at(
                                "surface.atlasRejected",
                                "material.mapping.regions.inset",
                                "atlas regions require halfTexel inset",
                            ));
                        }
                        Ok(ProjectAtlasRegion {
                            id: region.id,
                            content_min: region.content_min,
                            content_extent: region.content_extent,
                            padding: region.padding,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut atlas = ProjectAtlas {
                    asset_id: atlas_asset_id.clone(),
                    version: existing_atlas
                        .as_ref()
                        .map_or(1, |atlas| atlas.version.saturating_add(1)),
                    content_hash: String::new(),
                    texture_asset_id: texture.asset_id.clone(),
                    texture_version: texture.version,
                    texture_content_hash: texture.content_hash.clone(),
                    regions,
                };
                if let Some(existing) = &existing_atlas {
                    atlas.version = existing.version;
                    atlas.content_hash =
                        atlas_content_hash(&atlas).map_err(AdapterError::project)?;
                    if atlas.content_hash != existing.content_hash {
                        atlas.version = existing.version.saturating_add(1);
                    }
                }
                atlas.content_hash = atlas_content_hash(&atlas).map_err(AdapterError::project)?;
                let atlas_hash = atlas.content_hash.clone();
                let atlas_version = atlas.version;
                project
                    .atlases
                    .retain(|entry| entry.asset_id != atlas.asset_id);
                project.atlases.push(atlas);
                project
                    .atlases
                    .sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
                (
                    ProjectVoxelSurfaceMapping::Atlas {
                        atlas_asset_id: atlas_asset_id.clone(),
                        atlas_version,
                        atlas_content_hash: atlas_hash.clone(),
                        region_id,
                        tile_scale_cells,
                        tile_origin_cells,
                    },
                    Some((atlas_asset_id, atlas_hash)),
                )
            }
        };

        let existing_material = project
            .materials
            .iter()
            .find(|material| material.asset_id == input.material.material_asset_id)
            .cloned();
        let previous_atlas_asset_id =
            existing_material.as_ref().and_then(|material| {
                match material
                    .voxel_surface
                    .as_ref()
                    .map(|surface| &surface.mapping)
                {
                    Some(ProjectVoxelSurfaceMapping::Atlas { atlas_asset_id, .. }) => {
                        Some(atlas_asset_id.clone())
                    }
                    _ => None,
                }
            });
        require_expected_hash(
            "material",
            &input.material.material_asset_id,
            input.material.expected_material_content_hash.as_deref(),
            existing_material
                .as_ref()
                .and_then(|material| material.content_hash.as_deref()),
        )?;
        let material_version = existing_material
            .as_ref()
            .map_or(1, |material| material.version);
        let mut material = ProjectMaterial {
            asset_id: input.material.material_asset_id.clone(),
            display_name: input.material.material_asset_id.clone(),
            color: input.material.definition.style.color,
            roughness: input.material.definition.style.roughness,
            texture_tint: input.material.definition.style.texture_tint,
            emission_color: input.material.definition.style.emission_color,
            emissive: input.material.definition.style.emissive,
            version: material_version,
            content_hash: None,
            voxel_surface: Some(ProjectVoxelSurface {
                texture_asset_id: texture.asset_id.clone(),
                texture_version: texture.version,
                texture_content_hash: texture.content_hash.clone(),
                alpha_mode: input.material.alpha_mode,
                mapping: surface_mapping,
            }),
        };
        material.content_hash =
            Some(material_content_hash(&material).map_err(AdapterError::project)?);
        if let Some(existing) = &existing_material {
            if existing.content_hash != material.content_hash {
                material.version = existing.version.saturating_add(1);
                material.content_hash =
                    Some(material_content_hash(&material).map_err(AdapterError::project)?);
            }
        }
        let material_hash = material.content_hash.clone().expect("surface hash exists");
        project
            .materials
            .retain(|entry| entry.asset_id != material.asset_id);
        project.materials.push(material.clone());
        project
            .materials
            .sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
        if let Some(previous_atlas_asset_id) = previous_atlas_asset_id {
            let atlas_is_still_referenced = project.materials.iter().any(|material| {
                matches!(
                    material.voxel_surface.as_ref().map(|surface| &surface.mapping),
                    Some(ProjectVoxelSurfaceMapping::Atlas { atlas_asset_id, .. })
                        if atlas_asset_id == &previous_atlas_asset_id
                )
            });
            if !atlas_is_still_referenced {
                project
                    .atlases
                    .retain(|atlas| atlas.asset_id != previous_atlas_asset_id);
            }
        }

        let instance = project
            .instances
            .iter_mut()
            .find(|instance| instance.instance_id == input.assignment.instance_id)
            .ok_or_else(|| {
                AdapterError::at(
                    "project.unknownInstance",
                    "assignment.instanceId",
                    "surface assignment names an unknown voxel-object instance",
                )
            })?;
        instance
            .material_overrides
            .retain(|binding| binding.material_slot != input.assignment.material_slot);
        instance.material_overrides.push(ProjectMaterialOverride {
            material_slot: input.assignment.material_slot,
            material_asset_id: material.asset_id.clone(),
        });
        instance
            .material_overrides
            .sort_by_key(|binding| binding.material_slot);
        project.revision = project
            .revision
            .checked_add(1)
            .ok_or_else(|| AdapterError::project("project revision is exhausted"))?;

        let staged_loaded = stage_project(&open, &project).map_err(AdapterError::project)?;
        let staged_surface = load_surface_assets_with_pending_texture(
            &staged_loaded.root,
            &staged_loaded.project,
            Some((&texture_source_path, &texture_bytes)),
        )
        .map_err(AdapterError::project)?;
        let staged_runtime =
            load_runtime_project_from_loaded_with_surface(staged_loaded.clone(), staged_surface)
                .map_err(AdapterError::project)?;
        let mut staged_projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
        let (readout, staged_mesh_resources) =
            project_readout_from_runtime(&staged_runtime, &mut staged_projector)
                .map_err(AdapterError::project)?;
        let result = response(
            "projectMutationApplied",
            request_id,
            json!({
                "receipt": {
                    "kind": "voxelSurfaceMaterialUpserted",
                    "textureAssetId": texture.asset_id,
                    "textureContentHash": texture.content_hash,
                    "materialAssetId": material.asset_id,
                    "materialContentHash": material_hash,
                    "atlas": atlas_receipt.map(|(asset_id, content_hash)| json!({
                        "atlasAssetId": asset_id,
                        "atlasContentHash": content_hash,
                    })),
                    "sceneId": input.assignment.scene_id,
                    "instanceId": input.assignment.instance_id,
                    "materialSlot": input.assignment.material_slot,
                },
                "project": readout,
            }),
        );
        preflight_response_size(&result)?;
        self.ensure_disk_project_hash(&open, &input.expected_project_hash)?;
        let texture_path =
            safe_join(&open.root, &texture_source_path).map_err(AdapterError::project)?;
        if texture_path.exists() {
            let existing = read_bounded(&texture_path, 16 * 1024 * 1024, "PNG texture")
                .map_err(AdapterError::project)?;
            if existing != texture_bytes {
                return Err(AdapterError::at(
                    "surface.textureIdentityCollision",
                    "textureSource",
                    "content-addressed texture path contains different bytes",
                ));
            }
        } else {
            atomic_write(&texture_path, &texture_bytes).map_err(AdapterError::project)?;
        }
        atomic_write(&staged_loaded.path, staged_loaded.canonical_json.as_bytes())
            .map_err(AdapterError::project)?;
        self.open = Some(staged_loaded);
        self.runtime = Some(staged_runtime);
        self.projector = staged_projector;
        self.mesh_resources = staged_mesh_resources;
        self.playback.clear();
        Ok(result)
    }

    fn remove_voxel_surface_material(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: RemoveVoxelSurfaceMaterialRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        let open = self.require_open()?.clone();
        let mut project = open.project.clone();
        let material = project
            .materials
            .iter()
            .find(|material| material.asset_id == input.material_asset_id)
            .cloned()
            .ok_or_else(|| {
                AdapterError::at(
                    "surface.unknownMaterial",
                    "materialAssetId",
                    "surface material is not present",
                )
            })?;
        require_exact_present_hash(
            "material",
            &input.material_asset_id,
            &input.expected_material_content_hash,
            material.content_hash.as_deref(),
        )?;
        let surface = material.voxel_surface.as_ref().ok_or_else(|| {
            AdapterError::at(
                "surface.notTextured",
                "materialAssetId",
                "material does not own a voxel surface",
            )
        })?;
        if surface.texture_asset_id != input.texture_asset_id
            || surface.texture_content_hash != input.expected_texture_content_hash
        {
            return Err(AdapterError::at(
                "surface.staleTexture",
                "expectedTextureContentHash",
                "removal does not match the material texture closure",
            ));
        }
        let expected_atlas = match &surface.mapping {
            ProjectVoxelSurfaceMapping::Repeat { .. } => None,
            ProjectVoxelSurfaceMapping::Atlas {
                atlas_asset_id,
                atlas_content_hash,
                ..
            } => Some((atlas_asset_id.as_str(), atlas_content_hash.as_str())),
        };
        if expected_atlas
            != input
                .atlas_asset_id
                .as_deref()
                .zip(input.expected_atlas_content_hash.as_deref())
        {
            return Err(AdapterError::at(
                "surface.staleAtlas",
                "expectedAtlasContentHash",
                "removal does not match the material atlas closure",
            ));
        }
        if project.instances.iter().any(|instance| {
            instance
                .material_overrides
                .iter()
                .any(|binding| binding.material_asset_id == input.material_asset_id)
        }) {
            return Err(AdapterError::at(
                "surface.materialInUse",
                "materialAssetId",
                "reassign every voxel-object slot before removing this surface material",
            ));
        }
        project
            .materials
            .retain(|entry| entry.asset_id != input.material_asset_id);
        if let Some(atlas_id) = input.atlas_asset_id.as_deref() {
            let atlas_is_referenced = project.materials.iter().any(|material| {
                matches!(
                    material.voxel_surface.as_ref().map(|surface| &surface.mapping),
                    Some(ProjectVoxelSurfaceMapping::Atlas { atlas_asset_id, .. }) if atlas_asset_id == atlas_id
                )
            });
            if !atlas_is_referenced {
                project.atlases.retain(|atlas| atlas.asset_id != atlas_id);
            }
        }
        let texture_is_referenced = project.materials.iter().any(|material| {
            material
                .voxel_surface
                .as_ref()
                .is_some_and(|surface| surface.texture_asset_id == input.texture_asset_id)
        });
        if !texture_is_referenced {
            project
                .textures
                .retain(|texture| texture.asset_id != input.texture_asset_id);
        }
        project.revision = project
            .revision
            .checked_add(1)
            .ok_or_else(|| AdapterError::project("project revision is exhausted"))?;
        let staged_loaded = stage_project(&open, &project).map_err(AdapterError::project)?;
        let staged_runtime = load_runtime_project_from_loaded(staged_loaded.clone())
            .map_err(AdapterError::project)?;
        let mut staged_projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
        let (readout, staged_mesh_resources) =
            project_readout_from_runtime(&staged_runtime, &mut staged_projector)
                .map_err(AdapterError::project)?;
        let result = response(
            "projectMutationApplied",
            request_id,
            json!({
                "receipt": {
                    "kind": "voxelSurfaceMaterialRemoved",
                    "materialAssetId": input.material_asset_id,
                    "textureAssetId": input.texture_asset_id,
                },
                "project": readout,
            }),
        );
        preflight_response_size(&result)?;
        self.ensure_disk_project_hash(&open, &input.expected_project_hash)?;
        atomic_write(&staged_loaded.path, staged_loaded.canonical_json.as_bytes())
            .map_err(AdapterError::project)?;
        self.open = Some(staged_loaded);
        self.runtime = Some(staged_runtime);
        self.projector = staged_projector;
        self.mesh_resources = staged_mesh_resources;
        self.playback.clear();
        Ok(result)
    }

    fn inspect_source(&mut self, request_id: &str, request: Value) -> Result<Value, AdapterError> {
        let input: InspectSourceRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        let (path_label, bytes) = self.read_selection(&input.source)?;
        let import = MeshSourceImportRequest {
            source_asset_id: input.source_asset_id,
            asset_version: 1,
            source_path: path_label,
            format: MeshSourceFormat::Glb,
            source_bytes: bytes,
            expected_source_sha256: None,
            mesh_primitive: input.mesh_primitive,
        };
        let inspection = match input.source_kind {
            SourceKind::Animated => {
                let imported =
                    import_animated_mesh_source(&import).map_err(AdapterError::conversion)?;
                let clips = imported
                    .model
                    .clips
                    .iter()
                    .map(source_clip_readout)
                    .collect::<Vec<_>>();
                json!({
                    "sourceKind": "animated",
                    "source": imported.source.receipt.source,
                    "sourcePath": imported.source.receipt.source_path,
                    "sourceByteCount": imported.source.receipt.source_byte_count,
                    "metadata": imported.source.receipt.metadata,
                    "clips": clips,
                    "diagnostics": [],
                })
            }
            SourceKind::Static => {
                let imported = import_mesh_source(&import).map_err(AdapterError::conversion)?;
                json!({
                    "sourceKind": "static",
                    "source": imported.receipt.source,
                    "sourcePath": imported.receipt.source_path,
                    "sourceByteCount": imported.receipt.source_byte_count,
                    "metadata": imported.receipt.metadata,
                    "clips": [],
                    "diagnostics": [],
                })
            }
        };
        Ok(response(
            "voxelObjectSourceInspected",
            request_id,
            json!({ "inspection": inspection }),
        ))
    }

    fn prepare_conversion(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: PrepareConversionRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        let (source_path, bytes) = self.read_selection(&input.source)?;
        let license_path = input.license.as_ref().map(|value| value.path.clone());
        let import = MeshSourceImportRequest {
            source_asset_id: input.source_asset_id,
            asset_version: 1,
            source_path: source_path.clone(),
            format: MeshSourceFormat::Glb,
            source_bytes: bytes,
            expected_source_sha256: None,
            mesh_primitive: input.mesh_primitive,
        };
        let prepared = match input.source_kind {
            SourceKind::Animated => {
                let imported =
                    import_animated_mesh_source(&import).map_err(AdapterError::conversion)?;
                plan_animated_voxel_object_conversion(
                    &VoxelObjectConversionPlanRequest {
                        source: imported.source.receipt.source.clone(),
                        source_path,
                        target_asset_id: input.target_asset_id,
                        license_path,
                        settings: input.settings,
                        clips: input.clips,
                        default_clip: input.default_clip,
                    },
                    &imported,
                )
                .map_err(AdapterError::conversion)?
            }
            SourceKind::Static => {
                let imported = import_mesh_source(&import).map_err(AdapterError::conversion)?;
                plan_static_voxel_object_conversion(
                    &VoxelObjectConversionPlanRequest {
                        source: imported.receipt.source.clone(),
                        source_path,
                        target_asset_id: input.target_asset_id,
                        license_path,
                        settings: input.settings,
                        clips: Vec::new(),
                        default_clip: None,
                    },
                    &imported,
                )
                .map_err(AdapterError::conversion)?
            }
        };
        let preview = preview_voxel_object_conversion(
            &VoxelObjectConversionPreviewRequest {
                plan_id: prepared.plan().plan_id.clone(),
                expected_plan_hash: prepared.plan().plan_hash.clone(),
                frame: input.frame,
                max_samples: input.max_preview_samples,
            },
            &prepared,
        )
        .map_err(AdapterError::conversion)?;
        let materials = candidate_materials(
            prepared.candidate().asset.material_palette.as_slice(),
            self.require_open()?,
        );
        let projection = candidate_projection(
            &self.require_open()?.root,
            &prepared,
            &preview.selected_frame.selection,
            &materials,
        )?;
        let response_value = response(
            "voxelObjectConversionPrepared",
            request_id,
            json!({
                "plan": prepared.plan(),
                "preview": preview,
                "projection": projection.frame,
                "projectionReadout": projection_readout(self.require_open()?.project.revision + 1, 1),
                "meshResources": projection.mesh_resources,
            }),
        );
        self.prepared = Some(PreparedCandidate {
            expected_project_hash: input.expected_project_hash,
            prepared,
            materials,
        });
        Ok(response_value)
    }

    fn preview_conversion(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: PreviewConversionRequest = decode_request(request)?;
        let candidate = self.prepared.as_ref().ok_or_else(|| {
            AdapterError::new(
                "conversion.planNotFound",
                "no voxel-object candidate is retained",
            )
        })?;
        let preview = preview_voxel_object_conversion(
            &VoxelObjectConversionPreviewRequest {
                plan_id: input.plan_id,
                expected_plan_hash: input.expected_plan_hash,
                frame: input.frame,
                max_samples: input.max_preview_samples,
            },
            &candidate.prepared,
        )
        .map_err(AdapterError::conversion)?;
        let projection = candidate_projection(
            &self.require_open()?.root,
            &candidate.prepared,
            &preview.selected_frame.selection,
            &candidate.materials,
        )?;
        Ok(response(
            "voxelObjectConversionPreviewed",
            request_id,
            json!({
                "preview": preview,
                "projection": projection.frame,
                "projectionReadout": projection_readout(self.require_open()?.project.revision + 1, 1),
                "meshResources": projection.mesh_resources,
            }),
        ))
    }

    fn apply_conversion(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: ApplyConversionRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        let candidate = self.prepared.as_ref().ok_or_else(|| {
            AdapterError::new(
                "conversion.planNotFound",
                "no voxel-object candidate is retained",
            )
        })?;
        if candidate.expected_project_hash != input.expected_project_hash {
            return Err(AdapterError::new(
                "project.staleHash",
                "candidate belongs to an older project revision",
            ));
        }
        let applied = apply_voxel_object_conversion(
            &VoxelObjectConversionApplyRequest {
                plan_id: input.plan_id,
                expected_plan_hash: input.expected_plan_hash,
                expected_output_hash: Some(input.expected_output_hash),
            },
            &candidate.prepared,
        )
        .map_err(AdapterError::conversion)?;
        let published = publish_project_conversion(PreparedProjectConversion {
            loaded: self.require_open()?.clone(),
            prepared: candidate.prepared.clone(),
            materials: candidate.materials.clone(),
            source_import_microseconds: 0,
            conversion_microseconds: 0,
            quality: None,
        })
        .map_err(AdapterError::project)?;
        let root = self.require_open()?.root.clone();
        let relative_path = self.require_open()?.relative_path.clone();
        let runtime = load_runtime_project(&root, &relative_path).map_err(AdapterError::project)?;
        let readout = self.accept_runtime_project(runtime)?;
        self.prepared = None;
        Ok(response(
            "projectMutationApplied",
            request_id,
            json!({
                "receipt": {
                    "kind": "voxelObjectConversionApplied",
                    "planId": applied.plan_id,
                    "planHash": applied.plan_hash,
                    "assetId": applied.conversion.asset.asset_id,
                    "outputHash": published.content_hash,
                    "storedFrames": applied.conversion.stored_frames,
                    "aggregateVoxels": applied.conversion.aggregate_voxels,
                },
                "project": readout,
            }),
        ))
    }

    fn discard_conversion(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: DiscardConversionRequest = decode_request(request)?;
        let candidate = self.prepared.as_ref().ok_or_else(|| {
            AdapterError::new(
                "conversion.planNotFound",
                "no voxel-object candidate is retained",
            )
        })?;
        if candidate.prepared.plan().plan_id != input.plan_id {
            return Err(AdapterError::new(
                "conversion.planNotFound",
                "plan identity is not retained",
            ));
        }
        self.prepared = None;
        let loaded = self.require_open()?.clone();
        let runtime = load_runtime_project(&loaded.root, &loaded.relative_path)
            .map_err(AdapterError::project)?;
        let projection = publish_projection(
            &loaded.root,
            complete_packed_projection(&runtime).map_err(AdapterError::project)?,
        )
        .map_err(AdapterError::project)?;
        self.mesh_resources = projection.mesh_resources.clone();
        Ok(response(
            "voxelObjectConversionDiscarded",
            request_id,
            json!({
                "planId": input.plan_id,
                "projection": projection.frame,
                "projectionReadout": projection_readout(loaded.project.revision, loaded.project.instances.len()),
                "meshResources": projection.mesh_resources,
            }),
        ))
    }

    fn attach_instance(&mut self, request_id: &str, request: Value) -> Result<Value, AdapterError> {
        let input: AttachInstanceRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        let loaded = self.require_open()?.clone();
        if input.scene_id != loaded.project.entry_scene {
            return Err(AdapterError::at(
                "project.unknownScene",
                "sceneId",
                "scene is not the voxel lab entry scene",
            ));
        }
        if !input.instance.material_overrides.is_empty() {
            return Err(AdapterError::at(
                "adapter.unsupportedMaterialOverride",
                "instance.materialOverrides",
                "voxel lab instances do not yet persist material overrides",
            ));
        }
        let runtime = load_runtime_project(&loaded.root, &loaded.relative_path)
            .map_err(AdapterError::project)?;
        let object = runtime
            .objects
            .get(&input.instance.voxel_object_asset_id)
            .ok_or_else(|| {
                AdapterError::at(
                    "project.unknownAsset",
                    "instance.voxelObjectAssetId",
                    "voxel object is not loaded",
                )
            })?;
        resolve_frame(object, &input.instance.frame).map_err(AdapterError::project)?;
        let mut project = loaded.project.clone();
        let owner_entity_id = project
            .instances
            .iter()
            .find(|entry| entry.instance_id == input.instance.instance_id)
            .map_or_else(
                || {
                    project
                        .instances
                        .iter()
                        .map(|entry| entry.entity_id)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1)
                },
                |entry| entry.entity_id,
            );
        project
            .instances
            .retain(|entry| entry.instance_id != input.instance.instance_id);
        project.instances.push(ProjectVoxelObjectInstance {
            entity_id: owner_entity_id,
            instance_id: input.instance.instance_id.clone(),
            voxel_object_asset_id: input.instance.voxel_object_asset_id.clone(),
            frame: input.instance.frame.clone(),
            translation: input.instance.translation,
            rotation: input.instance.rotation,
            scale: input.instance.scale,
            collision_policy: ProjectCollisionPolicy::None,
            material_overrides: Vec::new(),
        });
        project
            .instances
            .sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        project.revision = project.revision.saturating_add(1);
        let saved = save_project(&loaded, &project).map_err(AdapterError::project)?;
        let runtime = load_runtime_project(&saved.root, &saved.relative_path)
            .map_err(AdapterError::project)?;
        let readout = self.accept_runtime_project(runtime)?;
        Ok(response(
            "projectMutationApplied",
            request_id,
            json!({
                "receipt": {
                    "kind": "voxelObjectInstanceAttached",
                    "sceneId": input.scene_id,
                    "instanceId": input.instance.instance_id,
                    "assetId": input.instance.voxel_object_asset_id,
                    "frameKind": match input.instance.frame {
                        ProjectFrameSelection::Default => "default",
                        ProjectFrameSelection::Clip { .. } => "clip",
                    },
                },
                "project": readout,
            }),
        ))
    }

    fn attach_instances(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: AttachInstancesRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        if input.placements.is_empty() || input.placements.len() > MAX_VOXEL_OBJECT_INSTANCE_BATCH {
            return Err(AdapterError::at(
                "voxelObject.invalidPlacementBatch",
                "placements",
                format!(
                    "voxel-object placement batch must contain 1..={MAX_VOXEL_OBJECT_INSTANCE_BATCH} entries"
                ),
            ));
        }

        let mut request_instance_ids = BTreeSet::new();
        for (index, placement) in input.placements.iter().enumerate() {
            if !request_instance_ids.insert(placement.instance.instance_id.as_str()) {
                return Err(AdapterError::at(
                    "voxelObject.duplicatePlacementIdentity",
                    format!("placements[{index}].instance.instanceId"),
                    format!(
                        "placement repeats instance identity {:?}",
                        placement.instance.instance_id
                    ),
                ));
            }
        }

        let open = self.require_open()?.clone();
        let current =
            load_runtime_project(&open.root, &open.relative_path).map_err(AdapterError::project)?;
        if current.loaded.project_hash != input.expected_project_hash {
            return Err(AdapterError::at(
                "project.staleHash",
                "expectedProjectHash",
                format!(
                    "expected {}, current project is {}",
                    input.expected_project_hash, current.loaded.project_hash
                ),
            ));
        }

        let existing_instance_ids = current
            .loaded
            .project
            .instances
            .iter()
            .map(|instance| instance.instance_id.as_str())
            .collect::<BTreeSet<_>>();
        if let Some((index, placement)) =
            input.placements.iter().enumerate().find(|(_, placement)| {
                existing_instance_ids.contains(placement.instance.instance_id.as_str())
            })
        {
            return Err(AdapterError::at(
                "voxelObject.instanceIdentityCollision",
                format!("placements[{index}].instance.instanceId"),
                format!(
                    "placement collides with existing instance {:?}",
                    placement.instance.instance_id
                ),
            ));
        }

        let mut project = current.loaded.project.clone();
        let mut next_owner_entity_id = project
            .instances
            .iter()
            .map(|instance| instance.entity_id)
            .max()
            .unwrap_or(0);
        let mut receipt_placements = Vec::with_capacity(input.placements.len());
        for (index, placement) in input.placements.into_iter().enumerate() {
            if placement.scene_id != project.entry_scene {
                return Err(AdapterError::at(
                    "project.unknownScene",
                    format!("placements[{index}].sceneId"),
                    "scene is not the voxel lab entry scene",
                ));
            }
            if !placement.instance.material_overrides.is_empty() {
                return Err(AdapterError::at(
                    "adapter.unsupportedMaterialOverride",
                    format!("placements[{index}].instance.materialOverrides"),
                    "voxel lab instances do not yet persist material overrides",
                ));
            }
            let object = current
                .objects
                .get(&placement.instance.voxel_object_asset_id)
                .ok_or_else(|| {
                    AdapterError::at(
                        "project.unknownAsset",
                        format!("placements[{index}].instance.voxelObjectAssetId"),
                        "voxel object is not loaded",
                    )
                })?;
            resolve_frame(object, &placement.instance.frame).map_err(AdapterError::project)?;
            next_owner_entity_id = next_owner_entity_id
                .checked_add(1)
                .filter(|entity_id| *entity_id <= MAX_JSON_SAFE_ENTITY_ID)
                .ok_or_else(|| {
                    AdapterError::at(
                        "voxelObject.ownerIdentityExhausted",
                        format!("placements[{index}]"),
                        "cannot allocate another JSON-safe entity owner",
                    )
                })?;
            let frame_kind = match &placement.instance.frame {
                ProjectFrameSelection::Default => "default",
                ProjectFrameSelection::Clip { .. } => "clip",
            };
            receipt_placements.push(json!({
                "sceneId": placement.scene_id,
                "instanceId": placement.instance.instance_id,
                "assetId": placement.instance.voxel_object_asset_id,
                "frameKind": frame_kind,
                "ownerEntityId": next_owner_entity_id,
            }));
            project.instances.push(ProjectVoxelObjectInstance {
                entity_id: next_owner_entity_id,
                instance_id: placement.instance.instance_id,
                voxel_object_asset_id: placement.instance.voxel_object_asset_id,
                frame: placement.instance.frame,
                translation: placement.instance.translation,
                rotation: placement.instance.rotation,
                scale: placement.instance.scale,
                collision_policy: ProjectCollisionPolicy::None,
                material_overrides: Vec::new(),
            });
        }
        project
            .instances
            .sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        project.revision = project
            .revision
            .checked_add(1)
            .ok_or_else(|| AdapterError::project("project revision is exhausted"))?;

        let staged_loaded =
            stage_project(&current.loaded, &project).map_err(AdapterError::project)?;
        let staged_runtime = load_runtime_project_from_loaded(staged_loaded.clone())
            .map_err(AdapterError::project)?;
        let mut staged_projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
        let (readout, staged_mesh_resources) =
            project_readout_from_runtime(&staged_runtime, &mut staged_projector)
                .map_err(AdapterError::project)?;
        let result = response(
            "projectMutationApplied",
            request_id,
            json!({
                "receipt": {
                    "kind": "voxelObjectInstancesAttached",
                    "placements": receipt_placements,
                },
                "project": readout,
            }),
        );
        let response_bytes = serde_json::to_vec(&result)
            .map_err(|error| AdapterError::new("adapter.responseEncode", error.to_string()))?;
        if response_bytes.len() > MAX_RESPONSE_BYTES {
            return Err(AdapterError::new(
                "adapter.responseTooLarge",
                format!(
                    "batch mutation response is {} bytes, exceeding the {MAX_RESPONSE_BYTES}-byte bound",
                    response_bytes.len()
                ),
            ));
        }

        let final_current =
            load_project(&open.root, &open.relative_path).map_err(AdapterError::project)?;
        if final_current.project_hash != input.expected_project_hash {
            return Err(AdapterError::at(
                "project.staleHash",
                "expectedProjectHash",
                format!(
                    "expected {}, current project is {}",
                    input.expected_project_hash, final_current.project_hash
                ),
            ));
        }
        atomic_write(&staged_loaded.path, staged_loaded.canonical_json.as_bytes())
            .map_err(AdapterError::project)?;
        self.open = Some(staged_loaded);
        self.runtime = Some(staged_runtime);
        self.projector = staged_projector;
        self.mesh_resources = staged_mesh_resources;
        self.playback.clear();
        Ok(result)
    }

    fn preview_instance(
        &mut self,
        request_id: &str,
        request: Value,
    ) -> Result<Value, AdapterError> {
        let input: PreviewInstanceRequest = decode_request(request)?;
        self.ensure_project_hash(&input.expected_project_hash)?;
        let loaded = self.require_open()?.clone();
        let runtime = self.runtime.as_ref().ok_or_else(|| {
            AdapterError::new(
                "project.runtimeUnavailable",
                "open project has no admitted voxel runtime",
            )
        })?;
        if runtime.loaded.project_hash != input.expected_project_hash {
            return Err(AdapterError::at(
                "project.staleHash",
                "expectedProjectHash",
                format!(
                    "expected {}, current project is {}",
                    input.expected_project_hash, runtime.loaded.project_hash
                ),
            ));
        }
        let presentation = self
            .playback
            .present(
                runtime,
                &mut self.projector,
                &input.scene_id,
                &input.instance_id,
                input.now_microseconds,
                &input.command,
            )
            .map_err(AdapterError::studio_playback)?;
        let additions = publish_mesh_resources(&loaded.root, presentation.mesh_resources)
            .map_err(AdapterError::project)?;
        merge_mesh_resource_readouts(&mut self.mesh_resources, additions)
            .map_err(AdapterError::project)?;
        Ok(response(
            "voxelObjectInstancePreviewed",
            request_id,
            json!({
                "playback": presentation.readout,
                "projection": presentation.projection,
                "meshResources": &self.mesh_resources,
                "projectionReadout": projection_readout(
                    loaded.project.revision,
                    loaded.project.instances.len(),
                ),
            }),
        ))
    }

    fn ensure_project_hash(&self, expected: &str) -> Result<(), AdapterError> {
        let loaded = self.require_open()?;
        if expected != loaded.project_hash {
            Err(AdapterError::at(
                "project.staleHash",
                "expectedProjectHash",
                format!(
                    "expected {expected}, current project is {}",
                    loaded.project_hash
                ),
            ))
        } else {
            Ok(())
        }
    }

    fn ensure_disk_project_hash(
        &self,
        open: &LoadedProject,
        expected: &str,
    ) -> Result<(), AdapterError> {
        let current =
            load_project(&open.root, &open.relative_path).map_err(AdapterError::project)?;
        if current.project_hash != expected {
            return Err(AdapterError::at(
                "project.staleHash",
                "expectedProjectHash",
                format!(
                    "expected {expected}, current project is {}",
                    current.project_hash
                ),
            ));
        }
        Ok(())
    }

    fn require_open(&self) -> Result<&LoadedProject, AdapterError> {
        self.open
            .as_ref()
            .ok_or_else(|| AdapterError::new("project.notOpen", "open a voxel lab project first"))
    }

    fn accept_runtime_project(&mut self, runtime: RuntimeProject) -> Result<Value, AdapterError> {
        let mut projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
        let (readout, mesh_resources) = project_readout_from_runtime(&runtime, &mut projector)
            .map_err(AdapterError::project)?;
        self.open = Some(runtime.loaded.clone());
        self.runtime = Some(runtime);
        self.projector = projector;
        self.mesh_resources = mesh_resources;
        self.playback.clear();
        Ok(readout)
    }

    fn read_selection(&self, selection: &FileSelection) -> Result<(String, Vec<u8>), AdapterError> {
        let loaded = self.require_open()?;
        let path = match selection.scope {
            FileScope::Project => {
                safe_join(&loaded.root, &selection.path).map_err(AdapterError::project)?
            }
            FileScope::Host => {
                let path = PathBuf::from(&selection.path);
                if !path.is_absolute() {
                    return Err(AdapterError::at(
                        "host.invalidPath",
                        "source.path",
                        "host source paths must be absolute",
                    ));
                }
                path
            }
        };
        let bytes = read_bounded(&path, MAX_SOURCE_BYTES, "conversion source")
            .map_err(AdapterError::project)?;
        Ok((selection.path.clone(), bytes))
    }
}

#[derive(Debug)]
pub struct AdapterError {
    code: String,
    path: Option<String>,
    message: String,
}

impl AdapterError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: None,
            message: message.into(),
        }
    }

    fn at(code: impl Into<String>, path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            path: Some(path.into()),
            message: message.into(),
        }
    }

    fn project(message: impl Into<String>) -> Self {
        Self::new("project.rejected", message)
    }

    fn conversion(error: voxel_convert::ConversionError) -> Self {
        let diagnostic = &error.diagnostics()[0];
        Self::at(
            diagnostic.code,
            diagnostic.path.clone(),
            diagnostic.message.clone(),
        )
    }

    fn studio_playback(error: StudioPlaybackError) -> Self {
        match error {
            StudioPlaybackError::UnknownScene => Self::at(
                "project.unknownScene",
                "sceneId",
                "scene is not the voxel lab entry scene",
            ),
            StudioPlaybackError::UnknownInstance => Self::at(
                "project.unknownInstance",
                "instanceId",
                "voxel-object instance is not loaded",
            ),
            StudioPlaybackError::UnknownAsset => Self::at(
                "project.unknownAsset",
                "instanceId",
                "voxel-object instance asset is not loaded",
            ),
            StudioPlaybackError::Runtime(message) => Self::project(message),
            StudioPlaybackError::Player(error) => Self::new("playback.rejected", error.to_string()),
            StudioPlaybackError::NotSelected => Self::new(
                "playback.notSelected",
                "scrub an applied voxel-object clip before controlling playback",
            ),
            StudioPlaybackError::TargetMismatch => Self::at(
                "playback.targetMismatch",
                "instanceId",
                "playback command does not name the retained transient target",
            ),
        }
    }
}

fn rejection(request_id: Option<&str>, error: AdapterError) -> Value {
    let mut response = json!({
        "type": "rejected",
        "protocolVersion": PROTOCOL_VERSION,
        "error": {
            "code": error.code,
            "message": error.message,
        }
    });
    if let Some(request_id) = request_id {
        response["requestId"] = Value::String(request_id.to_owned());
    }
    if let Some(path) = error.path {
        response["error"]["path"] = Value::String(path);
    }
    response
}

fn response(kind: &str, request_id: &str, fields: Value) -> Value {
    let mut value = json!({
        "type": kind,
        "protocolVersion": PROTOCOL_VERSION,
        "requestId": request_id,
    });
    if let (Some(target), Some(fields)) = (value.as_object_mut(), fields.as_object()) {
        target.extend(fields.clone());
    }
    value
}

fn required_string(value: &Value, field: &str) -> Result<String, AdapterError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            AdapterError::at(
                "adapter.requestDecode",
                field,
                format!("{field} must be non-empty text"),
            )
        })
}

fn decode_request<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, AdapterError> {
    serde_json::from_value(value)
        .map_err(|error| AdapterError::new("adapter.requestDecode", error.to_string()))
}

fn validate_surface_material_intent(
    material: &VoxelSurfaceMaterialDraft,
) -> Result<(), AdapterError> {
    let authority = &material.definition.authority;
    if authority.solid
        || authority.collidable
        || authority.occludes
        || authority.structural_class != "decorative"
    {
        return Err(AdapterError::at(
            "surface.materialAuthorityRejected",
            "material.definition.authority",
            "voxel surface authoring does not acquire structural or collision authority",
        ));
    }
    let style = &material.definition.style;
    if style.texture.is_some() {
        return Err(AdapterError::at(
            "surface.duplicateTextureAuthority",
            "material.definition.style.texture",
            "the adapter derives the exact texture reference from textureSource",
        ));
    }
    let expected_uv = match material.mapping {
        VoxelSurfaceMappingDraft::Repeat { .. } => "planar",
        VoxelSurfaceMappingDraft::Atlas { .. } => "atlas",
    };
    if style.uv_strategy != expected_uv {
        return Err(AdapterError::at(
            "surface.uvStrategyMismatch",
            "material.definition.style.uvStrategy",
            format!("{expected_uv} is required for the selected surface mapping"),
        ));
    }
    Ok(())
}

fn require_expected_hash(
    kind: &str,
    asset_id: &str,
    expected: Option<&str>,
    current: Option<&str>,
) -> Result<(), AdapterError> {
    match (expected, current) {
        (None, None) => Ok(()),
        (Some(expected), Some(current)) if expected == current => Ok(()),
        (None, Some(_)) => Err(AdapterError::at(
            format!("surface.{kind}AlreadyExists"),
            format!("expected{}ContentHash", title_case(kind)),
            format!("{asset_id} already exists; supply its exact content hash"),
        )),
        (Some(_), None) => Err(AdapterError::at(
            format!("surface.unknown{0}", title_case(kind)),
            format!("expected{}ContentHash", title_case(kind)),
            format!("{asset_id} does not exist"),
        )),
        (Some(_), Some(_)) => Err(AdapterError::at(
            format!("surface.stale{}", title_case(kind)),
            format!("expected{}ContentHash", title_case(kind)),
            format!("{asset_id} changed since it was read"),
        )),
    }
}

fn require_exact_present_hash(
    kind: &str,
    asset_id: &str,
    expected: &str,
    current: Option<&str>,
) -> Result<(), AdapterError> {
    require_expected_hash(kind, asset_id, Some(expected), current)
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn preflight_response_size(value: &Value) -> Result<(), AdapterError> {
    let response_bytes = serde_json::to_vec(value)
        .map_err(|error| AdapterError::new("adapter.responseEncode", error.to_string()))?;
    if response_bytes.len() > MAX_RESPONSE_BYTES {
        return Err(AdapterError::new(
            "adapter.responseTooLarge",
            format!(
                "mutation response is {} bytes, exceeding the {MAX_RESPONSE_BYTES}-byte bound",
                response_bytes.len()
            ),
        ));
    }
    Ok(())
}

fn render_texture_filter(value: ProjectTextureFilter) -> TextureFilter {
    match value {
        ProjectTextureFilter::Nearest => TextureFilter::Nearest,
        ProjectTextureFilter::Linear => TextureFilter::Linear,
    }
}

fn render_texture_wrap(value: ProjectTextureWrap) -> TextureWrap {
    match value {
        ProjectTextureWrap::Repeat => TextureWrap::Repeat,
        ProjectTextureWrap::Clamp => TextureWrap::Clamp,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum SourceKind {
    Static,
    Animated,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum FileScope {
    Project,
    Host,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileSelection {
    scope: FileScope,
    path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpsertVoxelSurfaceMaterialRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    texture_asset_id: String,
    expected_texture_content_hash: Option<String>,
    texture_source: FileSelection,
    filter: ProjectTextureFilter,
    material: VoxelSurfaceMaterialDraft,
    assignment: VoxelSurfaceAssignmentDraft,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RemoveVoxelSurfaceMaterialRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    material_asset_id: String,
    expected_material_content_hash: String,
    texture_asset_id: String,
    expected_texture_content_hash: String,
    atlas_asset_id: Option<String>,
    expected_atlas_content_hash: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoxelSurfaceMaterialDraft {
    material_asset_id: String,
    expected_material_content_hash: Option<String>,
    definition: StoredMaterialDefinition,
    alpha_mode: ProjectVoxelAlphaMode,
    mapping: VoxelSurfaceMappingDraft,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoxelSurfaceAssignmentDraft {
    scene_id: String,
    instance_id: String,
    material_slot: u16,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum VoxelSurfaceMappingDraft {
    Repeat {
        tile_scale_cells: [f32; 2],
        tile_origin_cells: [f32; 2],
    },
    Atlas {
        atlas_asset_id: String,
        expected_atlas_content_hash: Option<String>,
        regions: Vec<VoxelAtlasRegionDraft>,
        region_id: String,
        tile_scale_cells: [f32; 2],
        tile_origin_cells: [f32; 2],
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoxelAtlasRegionDraft {
    id: String,
    content_min: [u32; 2],
    content_extent: [u32; 2],
    padding: ProjectAtlasPadding,
    inset: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMaterialDefinition {
    authority: StoredMaterialAuthority,
    style: StoredMaterialStyle,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMaterialAuthority {
    solid: bool,
    collidable: bool,
    occludes: bool,
    structural_class: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMaterialStyle {
    color: [f32; 4],
    texture: Option<Value>,
    texture_tint: [f32; 4],
    emission_color: [f32; 4],
    roughness: f32,
    emissive: f32,
    uv_strategy: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InspectSourceRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    source_kind: SourceKind,
    source_asset_id: String,
    source: FileSelection,
    #[serde(default)]
    mesh_primitive: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PrepareConversionRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    source_kind: SourceKind,
    source_asset_id: String,
    source: FileSelection,
    target_asset_id: String,
    #[serde(default)]
    license: Option<FileSelection>,
    #[serde(default)]
    mesh_primitive: Option<String>,
    settings: VoxelObjectConversionSettings,
    #[serde(default)]
    clips: Vec<voxel_convert::VoxelObjectClipConversionRequest>,
    #[serde(default)]
    default_clip: Option<String>,
    frame: VoxelObjectFrameSelection,
    max_preview_samples: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewConversionRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    plan_id: String,
    expected_plan_hash: String,
    frame: VoxelObjectFrameSelection,
    max_preview_samples: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyConversionRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    plan_id: String,
    expected_plan_hash: String,
    expected_output_hash: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscardConversionRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    plan_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachInstanceRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    scene_id: String,
    instance: AttachInstance,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachInstancesRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    placements: Vec<AttachPlacement>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachPlacement {
    scene_id: String,
    instance: AttachInstance,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreviewInstanceRequest {
    #[serde(rename = "type")]
    _type: String,
    #[serde(rename = "protocolVersion")]
    _protocol_version: u64,
    #[serde(rename = "requestId")]
    _request_id: String,
    expected_project_hash: String,
    scene_id: String,
    instance_id: String,
    now_microseconds: u64,
    command: PlaybackCommand,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AttachInstance {
    instance_id: String,
    voxel_object_asset_id: String,
    frame: ProjectFrameSelection,
    translation: [f32; 3],
    rotation: [f32; 4],
    scale: [f32; 3],
    material_overrides: Vec<Value>,
}

fn source_clip_readout(clip: &voxel_convert::ImportedAnimationClip) -> Value {
    let target_nodes = clip
        .channels
        .iter()
        .map(|channel| channel.target_node_index)
        .collect::<BTreeSet<_>>();
    let properties = clip
        .channels
        .iter()
        .map(|channel| match channel.property {
            AnimationProperty::Translation => "translation",
            AnimationProperty::Rotation => "rotation",
            AnimationProperty::Scale => "scale",
            AnimationProperty::MorphWeights => "morphWeights",
        })
        .collect::<BTreeSet<_>>();
    json!({
        "sourceAnimationIndex": clip.source_animation_index,
        "name": clip.name,
        "durationMicroseconds": clip.duration_microseconds,
        "channelCount": clip.channels.len(),
        "targetNodeIndices": target_nodes,
        "properties": properties,
    })
}

fn candidate_materials(
    palette: &[voxel_asset::VoxelAssetMaterialBinding],
    loaded: &LoadedProject,
) -> Vec<ProjectMaterial> {
    let existing = loaded
        .project
        .materials
        .iter()
        .map(|material| (material.asset_id.as_str(), material))
        .collect::<BTreeMap<_, _>>();
    palette
        .iter()
        .enumerate()
        .map(|(index, binding)| {
            existing
                .get(binding.material_asset_id.as_str())
                .map(|material| (*material).clone())
                .unwrap_or_else(|| {
                    ProjectMaterial::flat(
                        binding.material_asset_id.clone(),
                        binding
                            .display_name
                            .clone()
                            .unwrap_or_else(|| binding.material_asset_id.clone()),
                        experiment_color(index),
                        0.82,
                    )
                })
        })
        .collect()
}

struct PublishedProjection {
    frame: render_model::RenderFrameDiff,
    mesh_resources: Vec<MeshResourceReadout>,
}

fn publish_projection(
    project_root: &Path,
    projection: render_projection::VoxelObjectProjectionResult,
) -> Result<PublishedProjection, String> {
    let mesh_resources = publish_mesh_resources(project_root, projection.mesh_resources)?;
    Ok(PublishedProjection {
        frame: projection.frame,
        mesh_resources,
    })
}

fn candidate_projection(
    project_root: &Path,
    candidate: &PreparedVoxelObjectConversion,
    selection: &VoxelObjectFrameSelection,
    materials: &[ProjectMaterial],
) -> Result<PublishedProjection, AdapterError> {
    let object = admit_voxel_object(
        &candidate.candidate().asset,
        VoxelObjectRuntimeLimits::default(),
    )
    .map_err(|error| AdapterError::new("voxelObject.runtimeAdmission", error.to_string()))?;
    let frame = runtime_frame(&object, selection)?;
    let projection = projection_for_object(&object, frame, materials, "Voxel object candidate")
        .map_err(AdapterError::project)?;
    publish_projection(project_root, projection).map_err(AdapterError::project)
}

fn runtime_frame(
    object: &AdmittedVoxelObject,
    selection: &VoxelObjectFrameSelection,
) -> Result<u32, AdapterError> {
    match selection {
        VoxelObjectFrameSelection::Default => Ok(0),
        VoxelObjectFrameSelection::Clip {
            clip_id,
            frame_index,
        } => object
            .clip(clip_id)
            .and_then(|clip| clip.frame_indices.get(*frame_index as usize))
            .copied()
            .ok_or_else(|| {
                AdapterError::new(
                    "voxelObject.frameNotFound",
                    format!("unknown frame {clip_id}[{frame_index}]"),
                )
            }),
    }
}

pub fn project_readout(loaded: &LoadedProject) -> Result<Value, String> {
    let runtime = load_runtime_project(&loaded.root, &loaded.relative_path)?;
    let mut projector = VoxelObjectRenderProjector::with_packed_mesh_resources();
    project_readout_from_runtime(&runtime, &mut projector).map(|(readout, _)| readout)
}

fn project_readout_from_runtime(
    runtime: &RuntimeProject,
    projector: &mut VoxelObjectRenderProjector,
) -> Result<(Value, Vec<MeshResourceReadout>), String> {
    let loaded = &runtime.loaded;
    let projection = publish_projection(
        &loaded.root,
        project_runtime_with_instance_frame(runtime, projector, None)?,
    )?;
    let object_assets = runtime
        .objects
        .values()
        .map(|object| object_authoring_readout(object.source()))
        .collect::<Vec<_>>();
    let object_instances = loaded
        .project
        .instances
        .iter()
        .map(|instance| {
            json!({
                "sceneId": loaded.project.entry_scene,
                "ownerEntityId": instance.entity_id,
                "instance": studio_instance(instance),
            })
        })
        .collect::<Vec<_>>();
    let entity_components = loaded
        .project
        .instances
        .iter()
        .map(|instance| {
            json!({
                "ownerEntityId": instance.entity_id,
                "componentTypeId": VOXEL_OBJECT_COMPONENT_TYPE_ID,
                "inspectorContract": {
                    "contractId": VOXEL_OBJECT_INSPECTOR_CONTRACT_ID,
                    "contractVersion": 1,
                },
            })
        })
        .collect::<Vec<_>>();
    let mut assets = vec![json!({
        "assetId": loaded.project.conversion.source_asset_id,
        "kind": "mesh-animation",
        "version": 1,
        "hash": loaded.project.conversion.expected_source_sha256,
        "sourcePath": loaded.project.conversion.source_path,
        "label": "Kenney retro character source",
        "dependencies": [],
        "dependents": [loaded.project.conversion.target_asset_id],
        "material": false,
        "importedMesh": true,
        "import": null,
    })];
    assets.extend(loaded.project.textures.iter().map(|texture| json!({
        "assetId": texture.asset_id,
        "kind": "texture",
        "version": texture.version,
        "hash": texture.content_hash,
        "sourcePath": texture.source_path,
        "label": texture.asset_id,
        "dependencies": [],
        "dependents": loaded.project.atlases.iter().filter(|atlas| atlas.texture_asset_id == texture.asset_id).map(|atlas| atlas.asset_id.clone()).chain(
            loaded.project.materials.iter().filter(|material| material.voxel_surface.as_ref().is_some_and(|surface| surface.texture_asset_id == texture.asset_id)).map(|material| material.asset_id.clone())
        ).collect::<Vec<_>>(),
        "material": false,
        "importedMesh": false,
        "import": null,
    })));
    assets.extend(loaded.project.atlases.iter().map(|atlas| json!({
        "assetId": atlas.asset_id,
        "kind": "sprite-sheet",
        "version": atlas.version,
        "hash": atlas.content_hash,
        "sourcePath": null,
        "label": atlas.asset_id,
        "dependencies": [atlas.texture_asset_id],
        "dependents": loaded.project.materials.iter().filter(|material| matches!(
            material.voxel_surface.as_ref().map(|surface| &surface.mapping),
            Some(ProjectVoxelSurfaceMapping::Atlas { atlas_asset_id, .. }) if atlas_asset_id == &atlas.asset_id
        )).map(|material| material.asset_id.clone()).collect::<Vec<_>>(),
        "material": false,
        "importedMesh": false,
        "import": null,
    })));
    assets.extend(loaded.project.materials.iter().map(|material| json!({
        "assetId": material.asset_id,
        "kind": "material",
        "version": material.version,
        "hash": material.content_hash,
        "sourcePath": null,
        "label": material.display_name,
        "dependencies": material.voxel_surface.as_ref().map(|surface| match &surface.mapping {
            ProjectVoxelSurfaceMapping::Repeat { .. } => vec![surface.texture_asset_id.clone()],
            ProjectVoxelSurfaceMapping::Atlas { atlas_asset_id, .. } => vec![atlas_asset_id.clone()],
        }).unwrap_or_default(),
        "dependents": loaded.project.voxel_objects.iter().map(|object| object.asset_id.clone()).collect::<Vec<_>>(),
        "material": true,
        "importedMesh": false,
        "import": null,
    })));
    assets.extend(loaded.project.voxel_objects.iter().map(|object| json!({
        "assetId": object.asset_id,
        "kind": "voxel-object",
        "version": 1,
        "hash": object.expected_content_hash,
        "sourcePath": object.path,
        "label": object.asset_id,
        "dependencies": loaded.project.materials.iter().map(|material| material.asset_id.clone()).collect::<Vec<_>>(),
        "dependents": [],
        "material": false,
        "importedMesh": false,
        "import": null,
    })));
    assets.sort_by_key(|asset| asset["assetId"].as_str().unwrap_or_default().to_owned());
    let lock_entries = assets
        .iter()
        .map(|asset| {
            json!({
                "assetId": asset["assetId"],
                "kind": asset["kind"],
                "version": asset["version"],
                "hash": asset["hash"],
                "dependencies": asset["dependencies"],
            })
        })
        .collect::<Vec<_>>();
    let hierarchy = hierarchy_readout(loaded);
    let material_readouts = loaded
        .project
        .materials
        .iter()
        .map(material_authoring_readout)
        .collect::<Vec<_>>();
    let surface_authoring = voxel_surface_authoring_readout(loaded);
    let object_bytes = loaded
        .project
        .voxel_objects
        .iter()
        .filter_map(|entry| safe_join(&loaded.root, &entry.path).ok())
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum::<u64>();
    let texture_bytes = loaded
        .project
        .textures
        .iter()
        .map(|texture| u64::from(texture.encoded_byte_length))
        .sum::<u64>();
    let readout = json!({
        "identity": {
            "projectId": loaded.project.project_id,
            "name": loaded.project.name,
            "entryScene": loaded.project.entry_scene,
            "sourceSchemaVersion": crate::model::PROJECT_SCHEMA_VERSION,
            "currentSchemaVersion": crate::model::PROJECT_SCHEMA_VERSION,
            "projectHash": loaded.project_hash,
            "sceneRevision": loaded.project.revision,
            "relativeProjectFile": loaded.relative_path,
        },
        "canonical": {
            "projectJson": loaded.canonical_json,
            "assetCatalogJson": "{}",
            "authoredSceneJson": "{}",
            "entityStateJson": "{}",
            "contentManifestJson": "{}",
        },
        "inspections": {
            "catalog": {
                "entryCount": assets.len(),
                "dependencyCount": loaded.project.materials.len() + loaded.project.atlases.len(),
                "kinds": [
                    { "name": "mesh-animation", "count": 1 },
                    { "name": "material", "count": loaded.project.materials.len() },
                    { "name": "sprite-sheet", "count": loaded.project.atlases.len() },
                    { "name": "texture", "count": loaded.project.textures.len() },
                    { "name": "voxel-object", "count": loaded.project.voxel_objects.len() },
                ],
                "diagnostics": { "diagnostics": [] },
            },
            "scene": {
                "sceneId": 1,
                "revision": loaded.project.revision,
                "schemaVersion": 1,
                "name": "Voxel Lab",
                "nodeCount": loaded.project.instances.len(),
                "rootCount": loaded.project.instances.len(),
                "dependencyCount": loaded.project.voxel_objects.len(),
                "nodeKinds": [{ "name": "entityInstance", "count": loaded.project.instances.len() }],
                "diagnostics": { "diagnostics": [] },
            },
            "entityState": {
                "schemaVersion": 1,
                "revision": loaded.project.revision,
                "entityCount": loaded.project.instances.len(),
                "lifecycle": [{ "name": "active", "count": loaded.project.instances.len() }],
                "sources": [{ "name": "project", "count": loaded.project.instances.len() }],
                "capabilities": [{ "name": "voxelObject", "count": loaded.project.instances.len() }],
                "relationships": [],
                "entityIds": loaded.project.instances.iter().map(|instance| instance.entity_id).collect::<Vec<_>>(),
                "diagnostics": { "diagnostics": [] },
            },
            "persistence": {
                "schemaVersion": 1,
                "artifactCount": 1 + loaded.project.voxel_objects.len() + loaded.project.textures.len(),
                "requiredArtifactCount": 1 + loaded.project.voxel_objects.len() + loaded.project.textures.len(),
                "declaredByteCount": loaded.canonical_json.len() as u64 + object_bytes + texture_bytes,
                "classes": [{ "name": "durable", "count": 1 + loaded.project.voxel_objects.len() + loaded.project.textures.len() }],
                "roles": [{ "name": "resource:voxel-lab-project", "count": 1 }],
                "loadSteps": [{ "stage": "project", "path": loaded.relative_path }],
                "diagnostics": { "diagnostics": [] },
            },
        },
        "sceneHierarchy": hierarchy,
        "assetBrowser": { "assets": assets, "lockEntries": lock_entries },
        "voxelAuthoring": { "assets": [], "instances": [], "materials": material_readouts },
        "voxelObjectAuthoring": { "assets": object_assets, "instances": object_instances },
        "voxelSurfaceAuthoring": surface_authoring,
        "animatedMeshResources": [{
            "asset": loaded.project.conversion.source_asset_id,
            "contentHash": loaded.project.conversion.expected_source_sha256,
            "clipIds": loaded.project.conversion.clips.iter().map(|clip| clip.source_clip_name.clone()).collect::<Vec<_>>(),
            "sourcePath": loaded.project.conversion.source_path,
        }],
        "entityComponents": entity_components,
        "meshResources": &projection.mesh_resources,
        "textureResources": &runtime.surface.texture_resources,
        "projection": projection.frame,
        "projectionReadout": projection_readout(loaded.project.revision, loaded.project.instances.len()),
    });
    Ok((readout, projection.mesh_resources))
}

fn projection_readout(source_revision: u64, instances: usize) -> Value {
    json!({
        "frameKind": "complete",
        "sourceRevision": source_revision,
        "retainedEntities": instances,
        "retainedLights": 0,
        "retainedVoxelInstances": instances,
        "retainedVoxelChunks": 0,
        "diagnostics": [],
    })
}

fn hierarchy_readout(loaded: &LoadedProject) -> Value {
    let nodes = loaded
        .project
        .instances
        .iter()
        .enumerate()
        .map(|(index, instance)| {
            let transform = json!({
                "translation": instance.translation,
                "rotation": instance.rotation,
                "scale": instance.scale,
            });
            json!({
                "nodeId": instance.entity_id,
                "parentNodeId": null,
                "childOrder": index,
                "displayOrder": index,
                "depth": 0,
                "nodeKind": "entityInstance",
                "label": instance.instance_id,
                "tags": ["voxel-object"],
                "asset": instance.voxel_object_asset_id,
                "entityId": instance.entity_id,
                "localTransform": transform,
                "worldTransform": transform,
                "renderableTransform": {
                    "translation": [0.0, 0.0, 0.0],
                    "rotation": [0.0, 0.0, 0.0, 1.0],
                    "scale": [1.0, 1.0, 1.0],
                },
            })
        })
        .collect::<Vec<_>>();
    json!({
        "sceneId": 1,
        "revision": loaded.project.revision,
        "name": "Voxel Lab",
        "rootNodeIds": loaded.project.instances.iter().map(|instance| instance.entity_id).collect::<Vec<_>>(),
        "nodes": nodes,
    })
}

fn studio_instance(instance: &ProjectVoxelObjectInstance) -> Value {
    json!({
        "instanceId": instance.instance_id,
        "voxelObjectAssetId": instance.voxel_object_asset_id,
        "frame": instance.frame,
        "translation": instance.translation,
        "rotation": instance.rotation,
        "scale": instance.scale,
        "materialOverrides": instance.material_overrides,
    })
}

fn material_authoring_readout(material: &ProjectMaterial) -> Value {
    json!({
        "assetId": material.asset_id,
        "definition": material_definition_readout(material),
    })
}

fn material_definition_readout(material: &ProjectMaterial) -> Value {
    let (texture, uv_strategy) =
        material
            .voxel_surface
            .as_ref()
            .map_or((Value::Null, "flat"), |surface| {
                let uv = match surface.mapping {
                    ProjectVoxelSurfaceMapping::Repeat { .. } => "planar",
                    ProjectVoxelSurfaceMapping::Atlas { .. } => "atlas",
                };
                (
                    json!({
                        "id": surface.texture_asset_id,
                        "version": { "exact": surface.texture_version },
                        "hash": surface.texture_content_hash,
                    }),
                    uv,
                )
            });
    json!({
        "authority": {
            "solid": false,
            "collidable": false,
            "occludes": false,
            "structuralClass": "decorative",
        },
        "style": {
            "color": material.color,
            "texture": texture,
            "textureTint": material.texture_tint,
            "emissionColor": material.emission_color,
            "roughness": material.roughness,
            "emissive": material.emissive,
            "uvStrategy": uv_strategy,
        },
    })
}

fn voxel_surface_authoring_readout(loaded: &LoadedProject) -> Value {
    let textures = loaded
        .project
        .textures
        .iter()
        .map(|texture| {
            json!({
                "textureAssetId": texture.asset_id,
                "version": texture.version,
                "contentHash": texture.content_hash,
                "sourcePath": texture.source_path,
                "width": texture.width,
                "height": texture.height,
                "encodedByteLength": texture.encoded_byte_length,
                "filter": texture.filter,
                "wrap": texture.wrap,
            })
        })
        .collect::<Vec<_>>();
    let atlases = loaded
        .project
        .atlases
        .iter()
        .map(|atlas| {
            json!({
                "atlasAssetId": atlas.asset_id,
                "version": atlas.version,
                "contentHash": atlas.content_hash,
                "textureAssetId": atlas.texture_asset_id,
                "textureVersion": atlas.texture_version,
                "textureContentHash": atlas.texture_content_hash,
                "regions": atlas.regions.iter().map(|region| json!({
                    "id": region.id,
                    "contentMin": region.content_min,
                    "contentExtent": region.content_extent,
                    "padding": region.padding,
                    "inset": "halfTexel",
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let materials = loaded
        .project
        .materials
        .iter()
        .filter_map(|material| {
            let surface = material.voxel_surface.as_ref()?;
            let mapping = match &surface.mapping {
                ProjectVoxelSurfaceMapping::Repeat {
                    tile_scale_cells,
                    tile_origin_cells,
                } => json!({
                    "kind": "repeat",
                    "tileScaleCells": tile_scale_cells,
                    "tileOriginCells": tile_origin_cells,
                }),
                ProjectVoxelSurfaceMapping::Atlas {
                    atlas_asset_id,
                    atlas_version,
                    atlas_content_hash,
                    region_id,
                    tile_scale_cells,
                    tile_origin_cells,
                } => json!({
                    "kind": "atlas",
                    "atlasAssetId": atlas_asset_id,
                    "atlasVersion": atlas_version,
                    "atlasContentHash": atlas_content_hash,
                    "regionId": region_id,
                    "tileScaleCells": tile_scale_cells,
                    "tileOriginCells": tile_origin_cells,
                }),
            };
            let assignments = loaded
                .project
                .instances
                .iter()
                .flat_map(|instance| {
                    instance
                        .material_overrides
                        .iter()
                        .filter(|binding| binding.material_asset_id == material.asset_id)
                        .map(|binding| {
                            json!({
                                "sceneId": loaded.project.entry_scene,
                                "instanceId": instance.instance_id,
                                "materialSlot": binding.material_slot,
                            })
                        })
                })
                .collect::<Vec<_>>();
            Some(json!({
                "materialAssetId": material.asset_id,
                "version": material.version,
                "contentHash": material.content_hash,
                "definition": material_definition_readout(material),
                "textureAssetId": surface.texture_asset_id,
                "textureVersion": surface.texture_version,
                "textureContentHash": surface.texture_content_hash,
                "alphaMode": surface.alpha_mode,
                "mapping": mapping,
                "assignments": assignments,
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "textures": textures,
        "atlases": atlases,
        "materials": materials,
    })
}

fn object_authoring_readout(object: &VoxelObjectAsset) -> Value {
    let default_frame = object_frame_readout(&object.default_frame, None);
    let clips = object
        .clips
        .iter()
        .map(|clip| {
            let default_duration = (1_000_000.0 / clip.frames_per_second).round() as u64;
            json!({
                "clipId": clip.id,
                "name": clip.name,
                "framesPerSecond": clip.frames_per_second,
                "frames": clip.frames.iter().map(|frame| {
                    object_frame_readout(
                        &frame.frame,
                        Some(frame.duration_seconds.map_or(default_duration, |seconds| (seconds * 1_000_000.0).round() as u64)),
                    )
                }).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let kind = match object.provenance.kind {
        VoxelObjectProvenanceKind::Authored => "authored",
        VoxelObjectProvenanceKind::ConvertedStaticMesh => "convertedStaticMesh",
        VoxelObjectProvenanceKind::ConvertedAnimatedMesh => "convertedAnimatedMesh",
    };
    json!({
        "assetId": object.asset_id,
        "contentHash": object.content_hash,
        "grid": {
            "coordinateSystem": "rightHandedYUp",
            "cellSize": object.grid.cell_size,
            "chunkSize": object.grid.chunk_size,
            "pivot": object.grid.pivot,
        },
        "bounds": object.bounds,
        "defaultFrame": default_frame,
        "clips": clips,
        "defaultClip": object.default_clip,
        "materialPalette": object.material_palette,
        "materialMap": object.material_map,
        "provenance": {
            "kind": kind,
            "sourcePath": object.provenance.source_path,
            "sourceSha256": object.provenance.source_sha256,
            "sourceByteCount": object.provenance.source_byte_count,
            "converter": object.provenance.converter,
            "settingsSha256": object.provenance.settings_sha256,
            "licensePath": object.provenance.license_path,
            "sourceClips": object.provenance.source_clips,
        },
    })
}

fn object_frame_readout(frame: &voxel_asset::VoxelFrame, duration: Option<u64>) -> Value {
    json!({
        "bounds": frame.bounds,
        "voxelDataHash": frame.voxel_data_hash,
        "voxelCount": represented_voxel_count(frame),
        "sparseRunCount": frame.representation.sparse_runs.len(),
        "durationMicroseconds": duration,
    })
}
