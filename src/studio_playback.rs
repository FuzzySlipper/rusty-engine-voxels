use serde::Deserialize;
use serde_json::{json, Value};
use voxel_object_runtime::{
    VoxelObjectLoopMode, VoxelObjectPlaybackStatus, VoxelObjectPlayer, VoxelObjectPlayerError,
};

use crate::runtime::{complete_projection_with_instance_frame, resolve_frame, RuntimeProject};

#[derive(Default)]
pub(crate) struct StudioVoxelObjectPlayback {
    session: Option<PlaybackSession>,
}

struct PlaybackSession {
    scene_id: String,
    instance_id: String,
    voxel_object_asset_id: String,
    player: VoxelObjectPlayer,
}

pub(crate) struct PlaybackPresentation {
    pub readout: Value,
    pub projection: render_model::RenderFrameDiff,
}

#[derive(Debug)]
pub(crate) enum StudioPlaybackError {
    UnknownScene,
    UnknownInstance,
    UnknownAsset,
    Runtime(String),
    Player(VoxelObjectPlayerError),
    NotSelected,
    TargetMismatch,
}

#[derive(Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub(crate) enum PlaybackCommand {
    Scrub {
        clip_id: String,
        clip_frame: u32,
        loop_mode: VoxelObjectLoopMode,
    },
    Play,
    Pause,
    Sample,
    Stop,
}

impl StudioVoxelObjectPlayback {
    pub fn clear(&mut self) {
        self.session = None;
    }

    pub fn present(
        &mut self,
        runtime: &RuntimeProject,
        scene_id: &str,
        instance_id: &str,
        now_microseconds: u64,
        command: &PlaybackCommand,
    ) -> Result<PlaybackPresentation, StudioPlaybackError> {
        if scene_id != runtime.loaded.project.entry_scene {
            return Err(StudioPlaybackError::UnknownScene);
        }
        let instance = runtime
            .loaded
            .project
            .instances
            .iter()
            .find(|entry| entry.instance_id == instance_id)
            .ok_or(StudioPlaybackError::UnknownInstance)?;
        let object = runtime
            .objects
            .get(&instance.voxel_object_asset_id)
            .ok_or(StudioPlaybackError::UnknownAsset)?;
        let durable_runtime_frame =
            resolve_frame(object, &instance.frame).map_err(StudioPlaybackError::Runtime)?;

        match command {
            PlaybackCommand::Scrub {
                clip_id,
                clip_frame,
                loop_mode,
            } => {
                let mut player = VoxelObjectPlayer::new();
                player
                    .scrub(object, clip_id, *clip_frame, *loop_mode)
                    .map_err(StudioPlaybackError::Player)?;
                self.session = Some(PlaybackSession {
                    scene_id: scene_id.to_owned(),
                    instance_id: instance_id.to_owned(),
                    voxel_object_asset_id: instance.voxel_object_asset_id.clone(),
                    player,
                });
            }
            PlaybackCommand::Play => self
                .require_target_mut(scene_id, instance_id)?
                .player
                .resume(now_microseconds)
                .map_err(StudioPlaybackError::Player)?,
            PlaybackCommand::Pause => self
                .require_target_mut(scene_id, instance_id)?
                .player
                .pause(now_microseconds)
                .map_err(StudioPlaybackError::Player)?,
            PlaybackCommand::Sample => {
                self.ensure_target(scene_id, instance_id)?;
            }
            PlaybackCommand::Stop => {
                self.ensure_target(scene_id, instance_id)?;
                self.clear();
            }
        }

        let (readout, runtime_frame) = if let Some(session) = self.session.as_ref() {
            let sample = session
                .player
                .sample_at(object, now_microseconds)
                .map_err(StudioPlaybackError::Player)?;
            (
                json!({
                    "sceneId": session.scene_id,
                    "instanceId": session.instance_id,
                    "voxelObjectAssetId": session.voxel_object_asset_id,
                    "projectHash": runtime.loaded.project_hash,
                    "objectContentHash": object.content_hash(),
                    "durableFrame": instance.frame,
                    "status": playback_status(sample.status),
                    "clipId": sample.clip,
                    "loopMode": playback_loop_mode(sample.loop_mode),
                    "rate": sample.rate,
                    "elapsedMicroseconds": sample.elapsed_micros,
                    "runtimeFrame": sample.frame,
                    "clipFrame": sample.clip_frame,
                    "ended": sample.ended,
                }),
                sample.frame,
            )
        } else {
            (
                json!({
                    "sceneId": scene_id,
                    "instanceId": instance_id,
                    "voxelObjectAssetId": instance.voxel_object_asset_id,
                    "projectHash": runtime.loaded.project_hash,
                    "objectContentHash": object.content_hash(),
                    "durableFrame": instance.frame,
                    "status": "stopped",
                    "clipId": null,
                    "loopMode": "once",
                    "rate": { "numerator": 1, "denominator": 1 },
                    "elapsedMicroseconds": 0,
                    "runtimeFrame": durable_runtime_frame,
                    "clipFrame": null,
                    "ended": false,
                }),
                durable_runtime_frame,
            )
        };
        let projection =
            complete_projection_with_instance_frame(runtime, Some((instance_id, runtime_frame)))
                .map_err(StudioPlaybackError::Runtime)?;
        Ok(PlaybackPresentation {
            readout,
            projection,
        })
    }

    fn ensure_target(&self, scene_id: &str, instance_id: &str) -> Result<(), StudioPlaybackError> {
        let session = self
            .session
            .as_ref()
            .ok_or(StudioPlaybackError::NotSelected)?;
        if session.scene_id != scene_id || session.instance_id != instance_id {
            return Err(StudioPlaybackError::TargetMismatch);
        }
        Ok(())
    }

    fn require_target_mut(
        &mut self,
        scene_id: &str,
        instance_id: &str,
    ) -> Result<&mut PlaybackSession, StudioPlaybackError> {
        self.ensure_target(scene_id, instance_id)?;
        Ok(self.session.as_mut().expect("playback target was checked"))
    }
}

fn playback_status(status: VoxelObjectPlaybackStatus) -> &'static str {
    match status {
        VoxelObjectPlaybackStatus::Stopped => "stopped",
        VoxelObjectPlaybackStatus::Playing => "playing",
        VoxelObjectPlaybackStatus::Paused => "paused",
    }
}

fn playback_loop_mode(loop_mode: VoxelObjectLoopMode) -> &'static str {
    match loop_mode {
        VoxelObjectLoopMode::Once => "once",
        VoxelObjectLoopMode::Repeat => "repeat",
        VoxelObjectLoopMode::PingPong => "pingPong",
    }
}
