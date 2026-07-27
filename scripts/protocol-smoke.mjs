import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const [decoderPath, adapter, root] = process.argv.slice(2);
if (decoderPath === undefined || adapter === undefined || root === undefined) {
  throw new Error('usage: protocol-smoke.mjs DECODER ADAPTER ROOT');
}

const { decodeStudioAdapterResponse } = await import(pathToFileURL(decoderPath).href);
const initialRequests = [
  { type: 'describe', protocolVersion: 7, requestId: 'describe-smoke' },
  {
    type: 'openProject',
    protocolVersion: 7,
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
  || opened.project.animatedMeshResources[0]?.clipIds.join(',') !== 'idle,run,jump') {
  throw new Error('Studio open response omitted the checked voxel object projection');
}

const inspectRequests = [
  initialRequests[1],
  {
    type: 'inspectVoxelObjectSource',
    protocolVersion: 7,
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

console.log(JSON.stringify({
  protocolVersion: described.adapter.protocolVersion,
  operationCount: described.adapter.operations.length,
  projectionOperations: opened.project.projection.ops.length,
  voxelObjects: opened.project.voxelObjectAuthoring.assets.length,
  voxelInstances: opened.project.voxelObjectAuthoring.instances.length,
  sourceClips: inspection.inspection.clips.map((clip) => clip.name),
}));
