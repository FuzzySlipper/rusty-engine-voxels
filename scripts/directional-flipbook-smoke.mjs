import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const [decoderPath, adapter, root, project = 'content/projects/directional-sprite-experiment.project.json'] = process.argv.slice(2);
if (decoderPath === undefined || adapter === undefined || root === undefined) {
  throw new Error('usage: directional-flipbook-smoke.mjs DECODER ADAPTER ROOT [PROJECT]');
}

const { decodeStudioAdapterResponse } = await import(pathToFileURL(decoderPath).href);
const protocolVersion = 14;
const open = {
  type: 'openProject',
  protocolVersion,
  requestId: 'directional-open',
  root,
  projectFile: project,
};
const opened = runRequests(adapter, [open], 'directional open')[0];
if (opened?.type !== 'projectOpened'
  || opened.project.voxelObjectAuthoring.assets.length !== 1
  || opened.project.voxelObjectAuthoring.instances.length !== 1) {
  throw new Error('directional project did not open one authored voxel object and instance');
}
const asset = opened.project.voxelObjectAuthoring.assets[0];
const instance = opened.project.voxelObjectAuthoring.instances[0];
const clip = asset.clips.find((candidate) => candidate.clipId === 'clip/idle');
if (clip === undefined || clip.frames.length !== 8
  || instance.instance.voxelObjectAssetId !== 'voxel-object/posed-directional-sentinel') {
  throw new Error('directional project omitted the eight-frame directional clip');
}
const voxelCounts = clip.frames.map((frame) => frame.voxelCount);
if (asset.grid.cellSize !== 0.01
  || Math.min(...voxelCounts) < 10_000
  || Math.max(...voxelCounts) > 100_000
  || asset.defaultFrame.voxelCount !== voxelCounts[0]) {
  throw new Error('directional pixel voxel density is outside the 10k..100k target: '
    + voxelCounts.join(','));
}
const projectHash = opened.project.identity.projectHash;
const frameRequests = clip.frames.map((_, frameIndex) => ({
  type: 'previewVoxelObjectInstance',
  protocolVersion,
  requestId: `directional-scrub-${frameIndex}`,
  expectedProjectHash: projectHash,
  sceneId: 'scene/directional-sentinel',
  instanceId: 'directional-sentinel',
  nowMicroseconds: 1_000_000 + frameIndex * 120_000,
  command: { kind: 'scrub', clipId: 'clip/idle', clipFrame: frameIndex, loopMode: 'repeat' },
}));
const scrubs = runRequests(adapter, [open, ...frameRequests], 'directional scrub').slice(1);
const runtimeFrames = scrubs.map((response, frameIndex) => {
  if (response?.type !== 'voxelObjectInstancePreviewed' || response.playback.status !== 'paused') {
    throw new Error(`directional frame ${frameIndex} did not settle as a paused preview`);
  }
  if (response.playback.projectHash !== projectHash) {
    throw new Error(`directional scrub ${frameIndex} changed the project hash`);
  }
  return response.playback.runtimeFrame;
});
if (new Set(runtimeFrames).size !== runtimeFrames.length) {
  throw new Error(`directional frames did not remain distinct: ${runtimeFrames}`);
}
const reopened = runRequests(adapter, [
  { ...open, requestId: 'directional-fresh-open' },
], 'directional fresh reopen')[0];
if (reopened?.type !== 'projectOpened' || reopened.project.identity.projectHash !== projectHash) {
  throw new Error('directional project did not survive a fresh adapter reopen');
}

console.log(JSON.stringify({
  project,
  projectHash,
  asset: asset.assetId,
  directions: ['front', 'right', 'back', 'left'],
  frameCount: clip.frames.length,
  cellSizeMeters: asset.grid.cellSize,
  voxelCounts,
  peakVoxelsPerFrame: Math.max(...voxelCounts),
  runtimeFrames,
  frameSwitchProjectionOps: scrubs.filter((response) =>
    response.projection.ops.some((operation) => operation.op === 'setVoxelObjectFrame')).length,
  freshReopenHash: reopened.project.identity.projectHash,
}));

function runRequests(adapterPath, requests, label) {
  const result = spawnSync(adapterPath, [], {
    encoding: 'utf8',
    input: `${requests.map((request) => JSON.stringify(request)).join('\n')}\n`,
    maxBuffer: 256 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`Studio adapter ${label} exited ${String(result.status)}: ${result.stderr}`);
  }
  return result.stdout.trim().split('\n').map((line) =>
    decodeStudioAdapterResponse(JSON.parse(line))
  );
}
