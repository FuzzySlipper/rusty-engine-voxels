import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const [decoderPath, adapter, root] = process.argv.slice(2);
if (decoderPath === undefined || adapter === undefined || root === undefined) {
  throw new Error('usage: protocol-smoke.mjs DECODER ADAPTER ROOT');
}

const { decodeStudioAdapterResponse } = await import(pathToFileURL(decoderPath).href);
const initialRequests = [
  { type: 'describe', protocolVersion: 9, requestId: 'describe-smoke' },
  {
    type: 'openProject',
    protocolVersion: 9,
    requestId: 'open-smoke',
    root,
    projectFile: 'content/projects/voxel-lab.project.json',
  },
];

const initial = spawnSync(adapter, [], {
  encoding: 'utf8',
  input: `${initialRequests.map((request) => JSON.stringify(request)).join('\n')}\n`,
  maxBuffer: 8 * 1024 * 1024,
});
if (initial.status !== 0) {
  throw new Error(`Studio adapter exited ${String(initial.status)}: ${initial.stderr}`);
}
const initialResponses = initial.stdout.trim().split('\n').map((line) =>
  decodeStudioAdapterResponse(JSON.parse(line))
);
const [described, opened] = initialResponses;
if (described?.type !== 'described' || opened?.type !== 'projectOpened') {
  throw new Error('Studio adapter did not describe and open the voxel project');
}
if (opened.project.projection.ops.length !== 3
  || opened.project.voxelObjectAuthoring.assets.length !== 1
  || opened.project.voxelObjectAuthoring.instances.length !== 1
  || opened.project.voxelObjectAuthoring.instances[0]?.ownerEntityId !== 1
  || opened.project.sceneHierarchy.nodes[0]?.entityId !== 1
  || opened.project.animatedMeshResources[0]?.clipIds.join(',') !== 'idle,run,jump') {
  throw new Error('Studio open response omitted the checked voxel object projection');
}

const inspectRequests = [
  initialRequests[1],
  {
    type: 'inspectVoxelObjectSource',
    protocolVersion: 9,
    requestId: 'inspect-smoke',
    expectedProjectHash: opened.project.identity.projectHash,
    sourceKind: 'animated',
    sourceAssetId: 'mesh-animation/retro-character',
    source: {
      scope: 'project',
      path: 'content/sources/kenney-retro-character/character-medium.glb',
    },
  },
];
const inspected = spawnSync(adapter, [], {
  encoding: 'utf8',
  input: `${inspectRequests.map((request) => JSON.stringify(request)).join('\n')}\n`,
  maxBuffer: 8 * 1024 * 1024,
});
if (inspected.status !== 0) {
  throw new Error(`Studio adapter source inspection exited ${String(inspected.status)}: ${inspected.stderr}`);
}
const inspectedResponses = inspected.stdout.trim().split('\n').map((line) =>
  decodeStudioAdapterResponse(JSON.parse(line))
);
const inspection = inspectedResponses[1];
if (inspection?.type !== 'voxelObjectSourceInspected'
  || inspection.inspection.sourceKind !== 'animated'
  || inspection.inspection.clips.length !== 3) {
  throw new Error('Studio adapter did not expose all three source animation clips');
}

const playbackRequests = [
  initialRequests[1],
  {
    type: 'previewVoxelObjectInstance',
    protocolVersion: 9,
    requestId: 'scrub-smoke',
    expectedProjectHash: opened.project.identity.projectHash,
    sceneId: 'scene/voxel-lab',
    instanceId: 'retro-character',
    nowMicroseconds: 1_000_000,
    command: { kind: 'scrub', clipId: 'clip/run', clipFrame: 1, loopMode: 'repeat' },
  },
  {
    type: 'previewVoxelObjectInstance',
    protocolVersion: 9,
    requestId: 'play-smoke',
    expectedProjectHash: opened.project.identity.projectHash,
    sceneId: 'scene/voxel-lab',
    instanceId: 'retro-character',
    nowMicroseconds: 1_000_000,
    command: { kind: 'play' },
  },
  {
    type: 'previewVoxelObjectInstance',
    protocolVersion: 9,
    requestId: 'sample-smoke',
    expectedProjectHash: opened.project.identity.projectHash,
    sceneId: 'scene/voxel-lab',
    instanceId: 'retro-character',
    nowMicroseconds: 1_200_000,
    command: { kind: 'sample' },
  },
];
const played = spawnSync(adapter, [], {
  encoding: 'utf8',
  input: `${playbackRequests.map((request) => JSON.stringify(request)).join('\n')}\n`,
  maxBuffer: 32 * 1024 * 1024,
});
if (played.status !== 0) {
  throw new Error(`Studio adapter playback exited ${String(played.status)}: ${played.stderr}`);
}
const playbackResponses = played.stdout.trim().split('\n').map((line) =>
  decodeStudioAdapterResponse(JSON.parse(line))
);
const scrubbed = playbackResponses[1];
const sampled = playbackResponses[3];
if (scrubbed?.type !== 'voxelObjectInstancePreviewed'
  || sampled?.type !== 'voxelObjectInstancePreviewed'
  || scrubbed.playback.status !== 'paused'
  || sampled.playback.status !== 'playing'
  || scrubbed.playback.runtimeFrame === sampled.playback.runtimeFrame
  || scrubbed.playback.durableFrame.kind !== 'clip'
  || scrubbed.playback.durableFrame.frameIndex !== 0) {
  throw new Error('Studio adapter did not preserve the saved pose beside two Rust-timed poses');
}

console.log(JSON.stringify({
  protocolVersion: described.adapter.protocolVersion,
  operationCount: described.adapter.operations.length,
  projectionOperations: opened.project.projection.ops.length,
  voxelObjects: opened.project.voxelObjectAuthoring.assets.length,
  voxelInstances: opened.project.voxelObjectAuthoring.instances.length,
  ownerEntityId: opened.project.voxelObjectAuthoring.instances[0]?.ownerEntityId,
  sourceClips: inspection.inspection.clips.map((clip) => clip.name),
  playbackFrames: [scrubbed.playback.runtimeFrame, sampled.playback.runtimeFrame],
}));
