use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

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
use crate::model::{
    experiment_color, ProjectCollisionPolicy, ProjectFrameSelection, ProjectMaterial,
    ProjectVoxelObjectInstance,
};
use crate::project::{
    load_project, read_bounded, safe_join, save_project, LoadedProject, MAX_SOURCE_BYTES,
};
use crate::runtime::{
    complete_projection, load_runtime_project, projection_for_object, resolve_frame,
};

const PROTOCOL_VERSION: u64 = 7;
const MAX_REQUEST_BYTES: usize = 256 * 1024;

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
    "setSceneObjectAppearance",
    "setEntityCollision",
    "setEntityKinematic",
    "setEntityTranslation",
    "upsertMaterial",
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
    "attachVoxelObjectInstance",
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
    prepared: Option<PreparedCandidate>,
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
            "inspectVoxelObjectSource" => self.inspect_source(&request_id, request),
            "prepareVoxelObjectConversion" => self.prepare_conversion(&request_id, request),
            "previewVoxelObjectConversion" => self.preview_conversion(&request_id, request),
            "applyVoxelObjectConversion" => self.apply_conversion(&request_id, request),
            "discardVoxelObjectConversion" => self.discard_conversion(&request_id, request),
            "attachVoxelObjectInstance" => self.attach_instance(&request_id, request),
            "closeProject" => {
                self.open = None;
                self.prepared = None;
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
                }
            }),
        )
    }

    fn open_project(&mut self, request_id: &str, request: &Value) -> Result<Value, AdapterError> {
        let root = required_string(request, "root")?;
        let project_file = required_string(request, "projectFile")?;
        let loaded =
            load_project(Path::new(&root), &project_file).map_err(AdapterError::project)?;
        let readout = project_readout(&loaded).map_err(AdapterError::project)?;
        self.open = Some(loaded);
        self.prepared = None;
        Ok(response(
            "projectOpened",
            request_id,
            json!({ "project": readout }),
        ))
    }

    fn read_project(&mut self, request_id: &str) -> Result<Value, AdapterError> {
        let current = self.require_open()?;
        let loaded =
            load_project(&current.root, &current.relative_path).map_err(AdapterError::project)?;
        let readout = project_readout(&loaded).map_err(AdapterError::project)?;
        self.open = Some(loaded);
        Ok(response(
            "projectRead",
            request_id,
            json!({ "project": readout }),
        ))
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
        let projection =
            candidate_projection(&prepared, &preview.selected_frame.selection, &materials)?;
        let response_value = response(
            "voxelObjectConversionPrepared",
            request_id,
            json!({
                "plan": prepared.plan(),
                "preview": preview,
                "projection": projection,
                "projectionReadout": projection_readout(self.require_open()?.project.revision + 1, 1),
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
            &candidate.prepared,
            &preview.selected_frame.selection,
            &candidate.materials,
        )?;
        Ok(response(
            "voxelObjectConversionPreviewed",
            request_id,
            json!({
                "preview": preview,
                "projection": projection,
                "projectionReadout": projection_readout(self.require_open()?.project.revision + 1, 1),
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
            elapsed_milliseconds: 0,
        })
        .map_err(AdapterError::project)?;
        let loaded = load_project(
            &self.require_open()?.root,
            &self.require_open()?.relative_path,
        )
        .map_err(AdapterError::project)?;
        let readout = project_readout(&loaded).map_err(AdapterError::project)?;
        self.open = Some(loaded);
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
        let loaded = self.require_open()?;
        let runtime = load_runtime_project(&loaded.root, &loaded.relative_path)
            .map_err(AdapterError::project)?;
        let projection = complete_projection(&runtime).map_err(AdapterError::project)?;
        Ok(response(
            "voxelObjectConversionDiscarded",
            request_id,
            json!({
                "planId": input.plan_id,
                "projection": projection,
                "projectionReadout": projection_readout(loaded.project.revision, loaded.project.instances.len()),
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
        project
            .instances
            .retain(|entry| entry.instance_id != input.instance.instance_id);
        project.instances.push(ProjectVoxelObjectInstance {
            instance_id: input.instance.instance_id.clone(),
            voxel_object_asset_id: input.instance.voxel_object_asset_id.clone(),
            frame: input.instance.frame.clone(),
            translation: input.instance.translation,
            rotation: input.instance.rotation,
            scale: input.instance.scale,
            collision_policy: ProjectCollisionPolicy::None,
        });
        project
            .instances
            .sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        project.revision = project.revision.saturating_add(1);
        let saved = save_project(&loaded, &project).map_err(AdapterError::project)?;
        let readout = project_readout(&saved).map_err(AdapterError::project)?;
        self.open = Some(saved);
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

    fn require_open(&self) -> Result<&LoadedProject, AdapterError> {
        self.open
            .as_ref()
            .ok_or_else(|| AdapterError::new("project.notOpen", "open a voxel lab project first"))
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
                .unwrap_or_else(|| ProjectMaterial {
                    asset_id: binding.material_asset_id.clone(),
                    display_name: binding
                        .display_name
                        .clone()
                        .unwrap_or_else(|| binding.material_asset_id.clone()),
                    color: experiment_color(index),
                    roughness: 0.82,
                })
        })
        .collect()
}

fn candidate_projection(
    candidate: &PreparedVoxelObjectConversion,
    selection: &VoxelObjectFrameSelection,
    materials: &[ProjectMaterial],
) -> Result<render_model::RenderFrameDiff, AdapterError> {
    let object = admit_voxel_object(
        &candidate.candidate().asset,
        VoxelObjectRuntimeLimits::default(),
    )
    .map_err(|error| AdapterError::new("voxelObject.runtimeAdmission", error.to_string()))?;
    let frame = runtime_frame(&object, selection)?;
    projection_for_object(&object, frame, materials, "Voxel object candidate")
        .map_err(AdapterError::project)
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
    let projection = complete_projection(&runtime)?;
    let object_assets = runtime
        .objects
        .values()
        .map(|object| object_authoring_readout(object.source()))
        .collect::<Vec<_>>();
    let object_instances = loaded
        .project
        .instances
        .iter()
        .map(|instance| json!({ "sceneId": loaded.project.entry_scene, "instance": studio_instance(instance) }))
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
    assets.extend(loaded.project.materials.iter().map(|material| json!({
        "assetId": material.asset_id,
        "kind": "material",
        "version": 1,
        "hash": null,
        "sourcePath": null,
        "label": material.display_name,
        "dependencies": [],
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
    let object_bytes = loaded
        .project
        .voxel_objects
        .iter()
        .filter_map(|entry| safe_join(&loaded.root, &entry.path).ok())
        .filter_map(|path| std::fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .sum::<u64>();
    Ok(json!({
        "identity": {
            "projectId": loaded.project.project_id,
            "name": loaded.project.name,
            "entryScene": loaded.project.entry_scene,
            "sourceSchemaVersion": 1,
            "currentSchemaVersion": 1,
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
                "dependencyCount": loaded.project.materials.len(),
                "kinds": [
                    { "name": "mesh-animation", "count": 1 },
                    { "name": "material", "count": loaded.project.materials.len() },
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
                "nodeKinds": [{ "name": "marker", "count": loaded.project.instances.len() }],
                "diagnostics": { "diagnostics": [] },
            },
            "entityState": {
                "schemaVersion": 1,
                "revision": loaded.project.revision,
                "entityCount": 0,
                "lifecycle": [],
                "sources": [],
                "capabilities": [],
                "relationships": [],
                "entityIds": [],
                "diagnostics": { "diagnostics": [] },
            },
            "persistence": {
                "schemaVersion": 1,
                "artifactCount": 1 + loaded.project.voxel_objects.len(),
                "requiredArtifactCount": 1 + loaded.project.voxel_objects.len(),
                "declaredByteCount": loaded.canonical_json.len() as u64 + object_bytes,
                "classes": [{ "name": "durable", "count": 1 + loaded.project.voxel_objects.len() }],
                "roles": [{ "name": "resource:voxel-lab-project", "count": 1 }],
                "loadSteps": [{ "stage": "project", "path": loaded.relative_path }],
                "diagnostics": { "diagnostics": [] },
            },
        },
        "sceneHierarchy": hierarchy,
        "assetBrowser": { "assets": assets, "lockEntries": lock_entries },
        "voxelAuthoring": { "assets": [], "instances": [], "materials": material_readouts },
        "voxelObjectAuthoring": { "assets": object_assets, "instances": object_instances },
        "animatedMeshResources": [{
            "asset": loaded.project.conversion.source_asset_id,
            "contentHash": loaded.project.conversion.expected_source_sha256,
            "clipIds": loaded.project.conversion.clips.iter().map(|clip| clip.source_clip_name.clone()).collect::<Vec<_>>(),
            "sourcePath": loaded.project.conversion.source_path,
        }],
        "loadingBay": {
            "sceneName": "Voxel Lab",
            "entityCount": 0,
            "doorCount": 0,
            "switchCount": 0,
            "enemyCount": 0,
            "encounterCount": 0,
            "extractionBeaconCount": 0,
            "navigatorCount": 0,
            "playerControllerCount": 0,
            "weaponCount": 0,
            "voxelEnvironment": "voxelObjectExperiment",
        },
        "projection": projection,
        "projectionReadout": projection_readout(loaded.project.revision, loaded.project.instances.len()),
    }))
}

fn projection_readout(source_revision: u64, instances: usize) -> Value {
    json!({
        "frameKind": "complete",
        "sourceRevision": source_revision,
        "retainedEntities": 0,
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
                "nodeId": index + 1,
                "parentNodeId": null,
                "childOrder": index,
                "displayOrder": index,
                "depth": 0,
                "nodeKind": "marker",
                "label": instance.instance_id,
                "tags": ["voxel-object"],
                "asset": instance.voxel_object_asset_id,
                "entityId": null,
                "localTransform": transform,
                "worldTransform": transform,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "sceneId": 1,
        "revision": loaded.project.revision,
        "name": "Voxel Lab",
        "rootNodeIds": (1..=nodes.len()).collect::<Vec<_>>(),
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
        "materialOverrides": [],
    })
}

fn material_authoring_readout(material: &ProjectMaterial) -> Value {
    json!({
        "assetId": material.asset_id,
        "definition": {
            "authority": {
                "solid": false,
                "collidable": false,
                "occludes": false,
                "structuralClass": "decorative",
            },
            "style": {
                "color": material.color,
                "texture": null,
                "textureTint": [1.0, 1.0, 1.0, 1.0],
                "emissionColor": [0.0, 0.0, 0.0, 1.0],
                "roughness": material.roughness,
                "emissive": 0.0,
                "uvStrategy": "flat",
            },
        },
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
