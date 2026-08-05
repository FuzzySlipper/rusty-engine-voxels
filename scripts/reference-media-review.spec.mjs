import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { isAbsolute, join, relative, resolve } from 'node:path';
import { inflateSync } from 'node:zlib';

const providerRoot = requiredEnvironment('RUSTY_STUDIO_PROVIDER_ROOT');
const require = createRequire(import.meta.url);
const { expect, test } = require(join(providerRoot, 'studio/node_modules/@playwright/test/index.js'));

const projectRoot = requiredEnvironment('RUSTY_STUDIO_PROJECT_ROOT');
const manifestPath = requiredEnvironment('RUSTY_STUDIO_REFERENCE_MANIFEST');
const captureRoot = requiredEnvironment('RUSTY_STUDIO_CAPTURE_ROOT');
const providerCommit = requiredEnvironment('RUSTY_STUDIO_PROVIDER_COMMIT');
const engineCommit = requiredEnvironment('RUSTY_STUDIO_ENGINE_COMMIT');

test('reference media review captures candidate views for human and agent comparison', async ({ page }) => {
  test.setTimeout(220_000);
  const manifest = await loadManifest();
  const projectFile = manifest.candidate.projectFile;
  const project = await readProject(projectFile);
  const objectEntry = findObject(project, manifest.candidate.objectAssetId);
  const object = JSON.parse(await readProjectFile(objectEntry.path));
  const candidateClip = object.clips.find((clip) => clip.id === manifest.candidate.clipId);
  if (candidateClip === undefined) throw new Error(`candidate clip ${manifest.candidate.clipId} is not present in ${objectEntry.path}`);
  const references = manifest.reference.entries;
  const cameraEntries = new Map(Object.entries(manifest.cameras));
  for (const reference of references) {
    if (reference.candidateFrameIndex >= candidateClip.frames.length) {
      throw new Error(`reference ${reference.id} selects candidate frame ${reference.candidateFrameIndex}, but clip has ${candidateClip.frames.length} frames`);
    }
  }

  await mkdir(captureRoot, { recursive: true });
  const captures = [];
  let activeCameraId = null;
  let activeState = null;
  for (const reference of references) {
    const camera = cameraEntries.get(reference.camera);
    if (camera === undefined) throw new Error(`reference ${reference.id} names unknown camera ${reference.camera}`);
    if (activeCameraId !== reference.camera) {
      activeState = await openCandidate(page, projectFile, manifest.candidate, camera);
      activeCameraId = reference.camera;
    }
    await selectFrame(activeState, reference.candidateFrameIndex);
    const filename = `${safeName(reference.id)}.png`;
    const screenshotPath = join(captureRoot, filename);
    const png = await activeState.canvas.screenshot({ path: screenshotPath });
    const readback = normalizedPng(png);
    const referenceBytes = await readProjectFile(reference.path);
    captures.push({
      referenceId: reference.id,
      referenceLabel: reference.label,
      referenceKind: manifest.reference.kind,
      referencePath: reference.path,
      referenceSha256: sha256(referenceBytes),
      sourceDirection: reference.direction ?? null,
      sourceFrameIndex: reference.sourceFrameIndex ?? null,
      sourceTimeMicroseconds: reference.timeMicroseconds ?? null,
      candidateFrameIndex: reference.candidateFrameIndex,
      camera: reference.camera,
      candidateRendererFrameHash: await requiredAttribute(activeState.viewport, 'data-authored-frame-hash'),
      cameraRevision: await requiredAttribute(activeState.viewport, 'data-camera-revision'),
      screenshot: filename,
      screenshotSha256: sha256(png),
      screenshotByteCount: png.byteLength,
      ...readback,
    });
  }

  const comparisonSheet = 'reference-comparison.svg';
  await writeFile(
    join(captureRoot, comparisonSheet),
    await buildComparisonSheet(captures),
  );
  const evidence = {
    schemaVersion: 1,
    kind: 'referenceMediaVoxelReview',
    manifestPath,
    project: projectFile,
    projectHash: await requiredAttribute(activeState.shell, 'data-project-hash'),
    providerCommit,
    engineCommit,
    candidate: {
      objectAssetId: objectEntry.assetId,
      objectPath: objectEntry.path,
      objectContentHash: object.contentHash,
      objectVoxelDataHash: object.voxelDataHash,
      clipId: manifest.candidate.clipId,
      frameCount: candidateClip.frames.length,
    },
    reference: {
      kind: manifest.reference.kind,
      source: manifest.reference.source,
      entryCount: references.length,
      entries: references.map(({ id, label, path, direction, sourceFrameIndex, timeMicroseconds }) => ({
        id,
        label,
        path,
        direction: direction ?? null,
        sourceFrameIndex: sourceFrameIndex ?? null,
        timeMicroseconds: timeMicroseconds ?? null,
      })),
    },
    cameras: manifest.cameras,
    captures,
    comparisonSheet,
    interpretation: manifest.interpretation ?? null,
    inspection: [
      'Reference media remains external authoring evidence; it is not inferred into canonical voxel state.',
      'Candidate screenshots are direct captures from the shared Studio renderer after canonical project admission.',
      'This review pack intentionally records visual evidence and hashes, not an automatic image-to-voxel answer.',
    ],
  };
  await writeFile(
    join(captureRoot, 'reference-media-review.json'),
    `${JSON.stringify(evidence, null, 2)}\n`,
  );
  process.stdout.write(`${JSON.stringify({
    kind: evidence.kind,
    projectHash: evidence.projectHash,
    objectContentHash: object.contentHash,
    referenceKind: manifest.reference.kind,
    captures: captures.length,
    comparisonSheet: join(captureRoot, comparisonSheet),
    evidence: join(captureRoot, 'reference-media-review.json'),
  })}\n`);
});

async function loadManifest() {
  const bytes = await readProjectFile(manifestPath);
  let manifest;
  try {
    manifest = JSON.parse(bytes.toString('utf8'));
  } catch (error) {
    throw new Error(`reference manifest is not JSON: ${error}`);
  }
  validateManifest(manifest);
  for (const entry of manifest.reference.entries) {
    const bytes = await readProjectFile(entry.path);
    assertPng(bytes, `reference ${entry.id}`);
  }
  return manifest;
}

function validateManifest(manifest) {
  if (manifest?.schemaVersion !== 1) throw new Error('reference manifest schemaVersion must be 1');
  if (!manifest.id || typeof manifest.id !== 'string') throw new Error('reference manifest id is required');
  if (!['directional-sprite', 'image-sequence', 'video-frame-sequence'].includes(manifest.reference?.kind)) {
    throw new Error('reference.kind must be directional-sprite, image-sequence, or video-frame-sequence');
  }
  if (!manifest.reference.source || typeof manifest.reference.source !== 'string') {
    throw new Error('reference.source is required');
  }
  if (!Array.isArray(manifest.reference.entries) || manifest.reference.entries.length === 0) {
    throw new Error('reference.entries must contain at least one entry');
  }
  if (!manifest.candidate?.projectFile || !manifest.candidate?.clipId) {
    throw new Error('candidate.projectFile and candidate.clipId are required');
  }
  if (!Number.isSafeInteger(manifest.candidate.entityId) || manifest.candidate.entityId < 0) {
    throw new Error('candidate.entityId must be a non-negative integer');
  }
  if (!manifest.cameras || typeof manifest.cameras !== 'object') throw new Error('cameras are required');
  const ids = new Set();
  for (const entry of manifest.reference.entries) {
    if (!entry || typeof entry !== 'object') throw new Error('reference entries must be objects');
    if (!entry.id || !entry.label || !entry.path || !entry.camera) throw new Error('reference entries require id, label, path, and camera');
    if (ids.has(entry.id)) throw new Error(`reference id is repeated: ${entry.id}`);
    ids.add(entry.id);
    if (!Number.isSafeInteger(entry.candidateFrameIndex) || entry.candidateFrameIndex < 0) {
      throw new Error(`reference ${entry.id} candidateFrameIndex must be a non-negative integer`);
    }
    if (!(entry.camera in manifest.cameras)) throw new Error(`reference ${entry.id} names unknown camera ${entry.camera}`);
  }
  for (const [id, camera] of Object.entries(manifest.cameras)) {
    if (!camera || typeof camera !== 'object') throw new Error(`camera ${id} must be an object`);
    if (!Number.isSafeInteger(camera.zoomSteps) || camera.zoomSteps < 0 || camera.zoomSteps > 40) {
      throw new Error(`camera ${id} zoomSteps must be 0..40`);
    }
    if (!Array.isArray(camera.orbits) || camera.orbits.length > 16) throw new Error(`camera ${id} orbits must contain 0..16 entries`);
    for (const orbit of camera.orbits) {
      if (!Number.isFinite(orbit.dx) || !Number.isFinite(orbit.dy) || !Number.isSafeInteger(orbit.steps) || orbit.steps < 1 || orbit.steps > 64) {
        throw new Error(`camera ${id} has an invalid orbit`);
      }
    }
  }
}

async function openCandidate(page, projectFile, candidate, camera) {
  const projectUrl = `/?root=${encodeURIComponent(projectRoot)}&project=${encodeURIComponent(projectFile)}`;
  await page.goto(projectUrl);
  const shell = page.locator('[data-visual-id="studio-shell"]');
  const viewport = page.locator('rusty-studio-viewport');
  const canvas = page.getByLabel('Shared Rusty renderer viewport');
  await expect(shell).toHaveAttribute('data-project-hash', /.+/);
  await expect.poll(async () => {
    const status = await viewport.getAttribute('data-renderer-status');
    if (status === 'error') throw new Error((await viewport.getAttribute('data-renderer-error')) ?? 'shared renderer failed');
    return status;
  }, { timeout: 60_000 }).toBe('ready');
  const objectRow = page.locator(`.entity-row[data-entity-id="${String(candidate.entityId)}"]`);
  await expect(objectRow).toBeVisible();
  await objectRow.dblclick();
  await expect(viewport).toHaveAttribute('data-selected-entity', String(candidate.entityId));
  await page.getByRole('button', { name: 'Entity', exact: true }).click();
  const component = page.locator('[data-visual-id="entity-voxel-object-component"]');
  const playback = component.locator('rusty-voxel-object-playback');
  await expect(playback).toContainText(candidate.clipId);
  const clip = component.getByLabel('Entity voxel-object preview clip');
  const frame = component.getByLabel('Entity voxel-object preview frame');
  await clip.selectOption(candidate.clipId);
  await canvas.focus();
  const box = await canvas.boundingBox();
  if (box === null) throw new Error('shared renderer canvas has no layout bounds');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  for (let step = 0; step < camera.zoomSteps; step += 1) {
    await canvas.evaluate((element) => {
      element.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: -120 }));
    });
  }
  for (const orbit of camera.orbits) {
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down({ button: 'left' });
    await page.mouse.move(
      box.x + box.width / 2 + orbit.dx,
      box.y + box.height / 2 + orbit.dy,
      { steps: orbit.steps },
    );
    await page.mouse.up({ button: 'left' });
  }
  return { shell, viewport, canvas, playback, frame };
}

async function selectFrame(state, value) {
  const before = await requiredAttribute(state.viewport, 'data-authored-frame-hash');
  const current = Number(await state.frame.inputValue());
  await state.frame.evaluate((element, nextValue) => {
    const input = element;
    input.value = String(nextValue);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  }, value);
  await expect(state.playback).toContainText(`frame ${String(value)}`);
  if (current !== value) {
    await expect.poll(() => requiredAttribute(state.viewport, 'data-authored-frame-hash'), { timeout: 30_000 })
      .not.toBe(before);
  }
}

function findObject(project, assetId) {
  const object = project.voxelObjects.find((entry) => assetId === undefined || entry.assetId === assetId);
  if (object === undefined) throw new Error(`candidate object ${assetId ?? '(first)'} is not present in project`);
  return object;
}

async function readProject(relativePath) {
  return JSON.parse((await readProjectFile(relativePath)).toString('utf8'));
}

async function readProjectFile(relativePath) {
  const path = resolveProjectPath(relativePath);
  return readFile(path);
}

function resolveProjectPath(relativePath) {
  if (typeof relativePath !== 'string' || relativePath.length === 0 || isAbsolute(relativePath)) {
    throw new Error(`project-relative path required: ${String(relativePath)}`);
  }
  const path = resolve(projectRoot, relativePath);
  const escaped = relative(projectRoot, path).startsWith('..');
  if (escaped) throw new Error(`path escapes project root: ${relativePath}`);
  return path;
}

async function buildComparisonSheet(captures) {
  const columns = Math.min(4, Math.max(1, captures.length));
  const cardWidth = 420;
  const cardHeight = 350;
  const rows = Math.ceil(captures.length / columns);
  const cards = [];
  for (let index = 0; index < captures.length; index += 1) {
    const capture = captures[index];
    const x = (index % columns) * cardWidth;
    const y = Math.floor(index / columns) * cardHeight;
    const reference = await readProjectFile(capture.referencePath);
    const candidate = await readFile(join(captureRoot, capture.screenshot));
    cards.push(`
      <g transform="translate(${x},${y})">
        <rect width="${cardWidth - 8}" height="${cardHeight - 8}" rx="8" fill="#17202b" stroke="#40556d"/>
        <text x="14" y="24" fill="#f7fbff" font-family="monospace" font-size="14">${escapeXml(capture.referenceLabel)}</text>
        <text x="14" y="44" fill="#a9c5de" font-family="monospace" font-size="11">${escapeXml(`${capture.referenceId} · camera ${capture.camera} · frame ${capture.candidateFrameIndex}`)}</text>
        <text x="14" y="62" fill="#a9c5de" font-family="monospace" font-size="10">target</text>
        <image x="14" y="70" width="184" height="250" preserveAspectRatio="xMidYMid meet" href="data:image/png;base64,${reference.toString('base64')}"/>
        <text x="214" y="62" fill="#a9c5de" font-family="monospace" font-size="10">candidate</text>
        <image x="214" y="70" width="184" height="250" preserveAspectRatio="xMidYMid meet" href="data:image/png;base64,${candidate.toString('base64')}"/>
      </g>`);
  }
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${columns * cardWidth}" height="${rows * cardHeight}" viewBox="0 0 ${columns * cardWidth} ${rows * cardHeight}">
  <rect width="100%" height="100%" fill="#0e141c"/>
  ${cards.join('\n')}
</svg>\n`;
}

function assertPng(bytes, label) {
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  if (!bytes.subarray(0, 8).equals(signature)) throw new Error(`${label} must be a PNG reference image`);
}

function normalizedPng(bytes) {
  const png = Buffer.from(bytes);
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  if (!png.subarray(0, 8).equals(signature)) throw new Error('canvas screenshot is not a PNG');
  let offset = 8;
  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  const compressed = [];
  while (offset + 12 <= png.length) {
    const length = png.readUInt32BE(offset);
    const type = png.toString('ascii', offset + 4, offset + 8);
    const dataStart = offset + 8;
    const dataEnd = dataStart + length;
    if (dataEnd + 4 > png.length) throw new Error('truncated PNG chunk');
    if (type === 'IHDR') {
      width = png.readUInt32BE(dataStart);
      height = png.readUInt32BE(dataStart + 4);
      bitDepth = png[dataStart + 8];
      colorType = png[dataStart + 9];
      if (png[dataStart + 12] !== 0) throw new Error('interlaced PNG is unsupported');
    } else if (type === 'IDAT') {
      compressed.push(png.subarray(dataStart, dataEnd));
    } else if (type === 'IEND') {
      break;
    }
    offset = dataEnd + 4;
  }
  if (width === 0 || height === 0 || bitDepth !== 8 || ![2, 6].includes(colorType)) {
    throw new Error(`expected an 8-bit RGB/RGBA PNG, got ${width}x${height} depth ${bitDepth} type ${colorType}`);
  }
  const sourceChannels = colorType === 6 ? 4 : 3;
  const stride = width * sourceChannels;
  const decoded = inflateSync(Buffer.concat(compressed));
  const sourcePixels = Buffer.alloc(height * stride);
  let sourceOffset = 0;
  for (let y = 0; y < height; y += 1) {
    const filter = decoded[sourceOffset++];
    const rowStart = y * stride;
    const previousRow = rowStart - stride;
    for (let x = 0; x < stride; x += 1) {
      const raw = decoded[sourceOffset++];
      const left = x >= sourceChannels ? sourcePixels[rowStart + x - sourceChannels] : 0;
      const up = y > 0 ? sourcePixels[previousRow + x] : 0;
      const upLeft = y > 0 && x >= sourceChannels ? sourcePixels[previousRow + x - sourceChannels] : 0;
      let value;
      if (filter === 0) value = raw;
      else if (filter === 1) value = raw + left;
      else if (filter === 2) value = raw + up;
      else if (filter === 3) value = raw + Math.floor((left + up) / 2);
      else if (filter === 4) value = raw + paeth(left, up, upLeft);
      else throw new Error(`unsupported PNG filter ${filter}`);
      sourcePixels[rowStart + x] = value & 0xff;
    }
  }
  const rgba = Buffer.alloc(width * height * 4);
  for (let index = 0; index < width * height; index += 1) {
    const source = index * sourceChannels;
    const target = index * 4;
    rgba[target] = sourcePixels[source];
    rgba[target + 1] = sourcePixels[source + 1];
    rgba[target + 2] = sourcePixels[source + 2];
    rgba[target + 3] = sourceChannels === 4 ? sourcePixels[source + 3] : 255;
  }
  return {
    rgbaSha256: sha256(rgba),
    rgbaWidth: width,
    rgbaHeight: height,
    rgbaByteCount: rgba.byteLength,
  };
}

function paeth(left, up, upLeft) {
  const estimate = left + up - upLeft;
  const leftDistance = Math.abs(estimate - left);
  const upDistance = Math.abs(estimate - up);
  const upLeftDistance = Math.abs(estimate - upLeft);
  if (leftDistance <= upDistance && leftDistance <= upLeftDistance) return left;
  if (upDistance <= upLeftDistance) return up;
  return upLeft;
}

function sha256(bytes) {
  return `sha256:${createHash('sha256').update(bytes).digest('hex')}`;
}

async function requiredAttribute(locator, name) {
  const value = await locator.getAttribute(name);
  if (value === null || value.length === 0) throw new Error(`${name} is unavailable`);
  return value;
}

function safeName(value) {
  return value.replaceAll(/[^a-zA-Z0-9._-]+/g, '-').replaceAll(/^-+|-+$/g, '') || 'reference';
}

function escapeXml(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;');
}

function requiredEnvironment(name) {
  const value = process.env[name];
  if (value === undefined || value.length === 0) throw new Error(`${name} is required`);
  return value;
}
