import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { pathToFileURL } from 'node:url';

const [decoderPath, adapter, root] = process.argv.slice(2);
if (decoderPath === undefined || adapter === undefined || root === undefined) {
  throw new Error('usage: protocol-smoke.mjs DECODER ADAPTER ROOT');
}

const { decodeStudioAdapterResponse } = await import(pathToFileURL(decoderPath).href);
const PROTOCOL_VERSION = 11;
const initialRequests = [
  { type: 'describe', protocolVersion: PROTOCOL_VERSION, requestId: 'describe-smoke' },
  {
    type: 'openProject',
    protocolVersion: PROTOCOL_VERSION,
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
const initialLines = initial.stdout.trim().split('\n');
const initialResponses = initialLines.map((line) =>
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
  || opened.project.animatedMeshResources[0]?.clipIds.join(',') !== 'idle,run,jump'
  || opened.project.meshResources?.length !== 1) {
  throw new Error('Studio open response omitted the checked voxel object projection');
}
const baselineResources = validateMeshResources(root, opened.project.meshResources);

const highFidelity = openProject(adapter, root, 'content/projects/retro-character-high-fidelity.project.json', 'high-fidelity');
const parseStarted = performance.now();
const highFidelityRaw = JSON.parse(highFidelity.line);
const nodeJsonParseMilliseconds = performance.now() - parseStarted;
const highFidelityOpened = decodeStudioAdapterResponse(highFidelityRaw);
if (highFidelityOpened.type !== 'projectOpened'
  || highFidelityOpened.project.meshResources?.length !== 1
  || JSON.stringify(highFidelityOpened.project.projection).includes('"positions":[')) {
  throw new Error('high-fidelity project did not use the packed mesh data plane');
}
const highFidelityResources = validateMeshResources(
  root,
  highFidelityOpened.project.meshResources,
);

const failureRoot = mkdtempSync(join(tmpdir(), 'rusty-engine-voxels-protocol-'));
try {
  cpSync(join(root, 'content'), join(failureRoot, 'content'), { recursive: true });
  const projectPath = join(failureRoot, 'content/projects/voxel-lab.project.json');
  const project = JSON.parse(readFileSync(projectPath, 'utf8'));
  project.voxelObjects[0].path = 'content/voxel-objects/missing.voxel-object.json';
  writeFileSync(projectPath, `${JSON.stringify(project, null, 2)}\n`);
  const missing = openFailureProject(adapter, failureRoot, 'missing-object');
  if (missing?.type !== 'rejected' || missing.error.code !== 'project.rejected') {
    throw new Error('Studio adapter did not reject a missing canonical voxel object');
  }

  project.voxelObjects[0].path = 'content/voxel-objects/corrupt.voxel-object.json';
  writeFileSync(projectPath, `${JSON.stringify(project, null, 2)}\n`);
  writeFileSync(join(failureRoot, project.voxelObjects[0].path), '{"schemaVersion":');
  const corrupt = openFailureProject(adapter, failureRoot, 'corrupt-object');
  if (corrupt?.type !== 'rejected' || corrupt.error.code !== 'project.rejected') {
    throw new Error('Studio adapter did not reject a corrupt canonical voxel object');
  }
} finally {
  rmSync(failureRoot, { force: true, recursive: true });
}

const inspectRequests = [
  initialRequests[1],
  {
    type: 'inspectVoxelObjectSource',
    protocolVersion: PROTOCOL_VERSION,
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
    protocolVersion: PROTOCOL_VERSION,
    requestId: 'scrub-smoke',
    expectedProjectHash: opened.project.identity.projectHash,
    sceneId: 'scene/voxel-lab',
    instanceId: 'retro-character',
    nowMicroseconds: 1_000_000,
    command: { kind: 'scrub', clipId: 'clip/run', clipFrame: 1, loopMode: 'repeat' },
  },
  {
    type: 'previewVoxelObjectInstance',
    protocolVersion: PROTOCOL_VERSION,
    requestId: 'play-smoke',
    expectedProjectHash: opened.project.identity.projectHash,
    sceneId: 'scene/voxel-lab',
    instanceId: 'retro-character',
    nowMicroseconds: 1_000_000,
    command: { kind: 'play' },
  },
  {
    type: 'previewVoxelObjectInstance',
    protocolVersion: PROTOCOL_VERSION,
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
const resumed = playbackResponses[2];
const sampled = playbackResponses[3];
if (scrubbed?.type !== 'voxelObjectInstancePreviewed'
  || resumed?.type !== 'voxelObjectInstancePreviewed'
  || sampled?.type !== 'voxelObjectInstancePreviewed'
  || scrubbed.playback.status !== 'paused'
  || sampled.playback.status !== 'playing'
  || scrubbed.playback.runtimeFrame === sampled.playback.runtimeFrame
  || scrubbed.playback.durableFrame.kind !== 'default'
  || scrubbed.projection.ops[0]?.op !== 'setVoxelObjectFrame'
  || resumed.projection.ops.length !== 0
  || sampled.projection.ops[0]?.op !== 'setVoxelObjectFrame') {
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
  steadyStateProjectionOperations: sampled.projection.ops.length,
  steadyStateResponseBytes: Buffer.byteLength(JSON.stringify(playbackResponses[3])),
  baselineControlResponseBytes: Buffer.byteLength(initialLines[1]),
  baselinePackedResourceBytes: baselineResources.byteLength,
  highFidelityControlResponseBytes: Buffer.byteLength(highFidelity.line),
  highFidelityPackedResourceBytes: highFidelityResources.byteLength,
  highFidelityNodeJsonParseMilliseconds: Number(nodeJsonParseMilliseconds.toFixed(3)),
  missingAssetRejected: true,
  corruptAssetRejected: true,
}));

function openProject(adapterPath, projectRoot, projectFile, suffix) {
  const result = spawnSync(adapterPath, [], {
    encoding: 'utf8',
    input: `${JSON.stringify({
      type: 'openProject',
      protocolVersion: PROTOCOL_VERSION,
      requestId: `open-${suffix}`,
      root: projectRoot,
      projectFile,
    })}\n`,
    maxBuffer: 8 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`Studio adapter ${suffix} open exited ${String(result.status)}: ${result.stderr}`);
  }
  return { line: result.stdout.trim() };
}

function validateMeshResources(projectRoot, resources) {
  let byteLength = 0;
  for (const resource of resources ?? []) {
    const bytes = readFileSync(join(projectRoot, resource.sourcePath));
    const hash = `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
    if (bytes.byteLength !== resource.byteLength
      || hash !== resource.contentHash
      || bytes.subarray(0, 8).toString('ascii') !== 'RMSHLE01') {
      throw new Error(`packed mesh resource ${resource.resource} does not match its manifest`);
    }
    byteLength += bytes.byteLength;
  }
  return { byteLength };
}

function openFailureProject(adapterPath, projectRoot, suffix) {
  const result = spawnSync(adapterPath, [], {
    encoding: 'utf8',
    input: `${JSON.stringify({
      type: 'openProject',
      protocolVersion: PROTOCOL_VERSION,
      requestId: `open-${suffix}`,
      root: projectRoot,
      projectFile: 'content/projects/voxel-lab.project.json',
    })}\n`,
    maxBuffer: 8 * 1024 * 1024,
  });
  if (result.status !== 0) {
    throw new Error(`Studio adapter failure probe exited ${String(result.status)}: ${result.stderr}`);
  }
  return decodeStudioAdapterResponse(JSON.parse(result.stdout.trim()));
}
