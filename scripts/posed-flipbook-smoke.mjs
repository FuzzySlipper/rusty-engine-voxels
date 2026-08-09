import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const [decoderPath, adapter, root] = process.argv.slice(2);
if (decoderPath === undefined || adapter === undefined || root === undefined) {
  throw new Error('usage: posed-flipbook-smoke.mjs DECODER ADAPTER ROOT');
}

const { decodeStudioAdapterResponse } = await import(pathToFileURL(decoderPath).href);
const PROTOCOL_VERSION = 15;
const PROJECT = 'content/projects/knight-flipbook.project.json';

const open = {
  type: 'openProject',
  protocolVersion: PROTOCOL_VERSION,
  requestId: 'open-knight-flipbook',
  root,
  projectFile: PROJECT,
};
const responses = runRequests(adapter, [open], 'knight flipbook open');
const opened = responses[0];
if (opened?.type !== 'projectOpened'
  || opened.project.voxelObjectAuthoring.assets.length !== 1
  || opened.project.voxelObjectAuthoring.instances.length !== 1
  || opened.project.voxelObjectAuthoring.instances[0]?.instance.voxelObjectAssetId
    !== 'voxel-object/posed-knight-walk') {
  throw new Error('Studio adapter did not open the knight flipbook project');
}
const projectHash = opened.project.identity.projectHash;

// Scrub the whole walk cycle: every authored frame must resolve and project.
const scrubRequests = [
  open,
  ...[0, 1, 2, 3].map((frame) => ({
    type: 'previewVoxelObjectInstance',
    protocolVersion: PROTOCOL_VERSION,
    requestId: `scrub-walk-${frame}`,
    expectedProjectHash: projectHash,
    sceneId: 'scene/knight-flipbook',
    instanceId: 'knight-posed-walk',
    nowMicroseconds: 1_000_000 + frame * 180_000,
    command: { kind: 'scrub', clipId: 'clip/walk', clipFrame: frame, loopMode: 'repeat' },
  })),
];
const scrubs = runRequests(adapter, scrubRequests, 'knight flipbook scrub').slice(1);
const runtimeFrames = [];
for (const [index, scrubbed] of scrubs.entries()) {
  if (scrubbed?.type !== 'voxelObjectInstancePreviewed'
    || scrubbed.playback.status !== 'paused') {
    throw new Error(`walk frame ${index} did not scrub to a paused pose`);
  }
  runtimeFrames.push(scrubbed.playback.runtimeFrame);
}
if (new Set(runtimeFrames).size !== runtimeFrames.length) {
  throw new Error(`walk frames do not resolve to distinct runtime frames: ${runtimeFrames}`);
}
const frameSwitches = scrubs.filter(
  (scrubbed) => scrubbed.projection.ops.some((op) => op.op === 'setVoxelObjectFrame'),
).length;

console.log(JSON.stringify({
  project: PROJECT,
  projectHash,
  instances: opened.project.voxelObjectAuthoring.instances.length,
  scrubbedRuntimeFrames: runtimeFrames,
  frameSwitchProjectionOps: frameSwitches,
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
