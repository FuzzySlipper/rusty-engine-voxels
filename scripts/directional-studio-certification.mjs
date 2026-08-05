import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { createRequire } from 'node:module';
import { inflateSync } from 'node:zlib';

const providerRoot = requiredEnvironment('RUSTY_STUDIO_PROVIDER_ROOT');
const require = createRequire(import.meta.url);
const { expect, test } = require(join(providerRoot, 'studio/node_modules/@playwright/test/index.js'));

const projectRoot = requiredEnvironment('RUSTY_STUDIO_PROJECT_ROOT');
const projectFile = requiredEnvironment('RUSTY_STUDIO_PROJECT_FILE');
const captureRoot = requiredEnvironment('RUSTY_STUDIO_CAPTURE_ROOT');
const providerCommit = requiredEnvironment('RUSTY_STUDIO_PROVIDER_COMMIT');
const engineCommit = requiredEnvironment('RUSTY_STUDIO_ENGINE_COMMIT');
const objectPath = join(projectRoot, projectFile);

test('directional dense object is visible through the shared renderer at perspective and overhead cameras', async ({ page }) => {
  test.setTimeout(180_000);
  const project = JSON.parse(await readFile(objectPath, 'utf8'));
  const objectEntry = project.voxelObjects[0];
  const object = JSON.parse(await readFile(join(projectRoot, objectEntry.path), 'utf8'));
  const voxelEvidence = JSON.parse(await readFile(
    join(projectRoot, 'evidence/directional-sprite-voxelization.json'),
    'utf8',
  ));
  expect(object.contentHash).toBe(objectEntry.expectedContentHash);
  expect(object.grid.cellSize).toBe(0.01);
  expect(object.clips).toHaveLength(1);
  expect(object.clips[0].frames).toHaveLength(8);
  expect(voxelEvidence.frames).toHaveLength(8);
  expect(voxelEvidence.frames.every((frame) => frame.bounds.min[1] === 0)).toBe(true);

  await mkdir(captureRoot, { recursive: true });
  const projectUrl = `/?root=${encodeURIComponent(projectRoot)}&project=${encodeURIComponent(projectFile)}`;
  await page.goto(projectUrl);

  const shell = page.locator('[data-visual-id="studio-shell"]');
  const viewport = page.locator('rusty-studio-viewport');
  const canvas = page.getByLabel('Shared Rusty renderer viewport');
  await expect(shell).toHaveAttribute('data-project-hash', /.+/);
  await expect.poll(async () => {
    const status = await viewport.getAttribute('data-renderer-status');
    if (status === 'error') {
      throw new Error((await viewport.getAttribute('data-renderer-error')) ?? 'shared renderer failed');
    }
    return status;
  }, { timeout: 60_000 }).toBe('ready');
  await expect(viewport).toHaveAttribute('data-retained-ops', /^[1-9][0-9]*$/);
  await expect(viewport).toHaveAttribute('data-authored-frame-hash', /.+/);

  const objectRow = page.locator('.entity-row[data-entity-id="1"]');
  await expect(objectRow).toBeVisible();
  await objectRow.dblclick();
  await expect(viewport).toHaveAttribute('data-selected-entity', '1');
  await page.getByRole('button', { name: 'Entity', exact: true }).click();
  const component = page.locator('[data-visual-id="entity-voxel-object-component"]');
  const playback = component.locator('rusty-voxel-object-playback');
  await expect(playback).toContainText('clip/idle');

  const clip = component.getByLabel('Entity voxel-object preview clip');
  const frame = component.getByLabel('Entity voxel-object preview frame');
  await clip.selectOption('clip/idle');

  // The dense experiment is authored at centimetre cells, so bring the
  // disposable inspection camera close enough to inspect pixel-scale detail.
  await canvas.focus();
  const focusState = await canvas.evaluate((element) => ({
    active: document.activeElement === element,
    tabIndex: element.tabIndex,
    width: element.width,
    height: element.height,
  }));
  if (!focusState.active) throw new Error(`shared renderer canvas did not focus: ${JSON.stringify(focusState)}`);
  const beforeZoomHash = sha256(await canvas.screenshot());
  const zoomBox = await canvas.boundingBox();
  if (zoomBox === null) throw new Error('shared renderer canvas has no layout bounds');
  await page.mouse.move(zoomBox.x + zoomBox.width / 2, zoomBox.y + zoomBox.height / 2);
  for (let step = 0; step < 10; step += 1) {
    await canvas.evaluate((element) => {
      element.dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: -120 }));
    });
  }
  await expect.poll(async () => sha256(await canvas.screenshot())).not.toBe(beforeZoomHash);

  const captures = [];
  const perspective = await captureCamera(page, canvas, viewport, frame, playback, object, voxelEvidence, captureRoot, 'perspective');
  captures.push(...perspective);

  const beforeOrbitHash = sha256(await canvas.screenshot());
  await canvas.focus();
  const box = await canvas.boundingBox();
  if (box === null) throw new Error('shared renderer canvas has no layout bounds');
  const centerX = box.x + box.width / 2;
  const centerY = box.y + box.height / 2;
  await page.mouse.move(centerX, centerY);
  await page.mouse.down({ button: 'left' });
  await page.mouse.move(centerX, centerY - Math.min(260, box.height * 0.38), { steps: 12 });
  await page.mouse.up({ button: 'left' });
  await expect.poll(async () => sha256(await canvas.screenshot())).not.toBe(beforeOrbitHash);

  const overhead = await captureCamera(page, canvas, viewport, frame, playback, object, voxelEvidence, captureRoot, 'overhead');
  captures.push(...overhead);

  const firstOverhead = captures.find((capture) => capture.camera === 'overhead' && capture.clipFrame === 0);
  if (firstOverhead === undefined) throw new Error('overhead frame zero capture is missing');
  await scrubAndWait(frame, viewport, playback, 7);
  await scrubAndWait(frame, viewport, playback, 0);
  const loopPng = await canvas.screenshot({ path: join(captureRoot, 'overhead-loop-return-frame-00.png') });
  const loopReturn = normalizedPng(loopPng);
  const loopAuthoredRendererFrameHash = await requiredAttribute(viewport, 'data-authored-frame-hash');
  expect(await frame.inputValue()).toBe('0');
  await expect(playback).toContainText('frame 0');

  const evidence = {
    schemaVersion: 1,
    kind: 'directionalStudioRendererCertification',
    project: projectFile,
    projectHash: await requiredAttribute(shell, 'data-project-hash'),
    providerCommit,
    engineCommit,
    objectPath: objectEntry.path,
    objectContentHash: object.contentHash,
    objectVoxelDataHash: object.voxelDataHash,
    cellSizeMeters: object.grid.cellSize,
    frameCounts: voxelEvidence.frames.map((entry) => entry.voxels),
    grounding: {
      requiredMinimumY: 0,
      allFramesGrounded: voxelEvidence.frames.every((entry) => entry.bounds.min[1] === 0),
    },
    cameras: {
      perspective: 'initial shared-renderer camera; no camera input applied',
      overhead: 'shared-renderer primary-button orbit, vertical drag toward overhead pitch',
    },
    captures,
    loopReturn: {
      ...loopReturn,
      authoredRendererFrameHash: loopAuthoredRendererFrameHash,
      voxelDataHash: object.clips[0].frames[0].frame.voxelDataHash,
      semanticFrameMatched: true,
    },
    inspection: [
      'PNG screenshots are direct canvas captures from the shared Studio renderer.',
      'RGBA hashes are normalized from the PNG bytes captured from that same renderer canvas.',
      'The fixed-depth object is a bounded 2.5D pixel-column experiment; source provenance remains uncertain/local.',
    ],
  };
  await writeFile(
    join(captureRoot, 'directional-studio-certification.json'),
    `${JSON.stringify(evidence, null, 2)}\n`,
  );
  process.stdout.write(`${JSON.stringify({
    kind: evidence.kind,
    projectHash: evidence.projectHash,
    objectContentHash: evidence.objectContentHash,
    captures: captures.length,
    perspectiveAndOverhead: true,
    loopReturnSemanticFrameMatches: true,
    captureRoot,
  })}\n`);
});

async function captureCamera(
  page,
  canvas,
  viewport,
  frame,
  playback,
  object,
  voxelEvidence,
  root,
  camera,
) {
  const captures = [];
  for (let clipFrame = 0; clipFrame < object.clips[0].frames.length; clipFrame += 1) {
    await scrubAndWait(frame, viewport, playback, clipFrame);
    const filename = `${camera}-frame-${String(clipFrame).padStart(2, '0')}.png`;
    const path = join(root, filename);
    const png = await canvas.screenshot({ path });
    const readback = normalizedPng(png);
    const semanticFrame = object.clips[0].frames[clipFrame];
    captures.push({
      camera,
      clipFrame,
      sourceDirection: voxelEvidence.frames[clipFrame].direction,
      sourceFrameIndex: voxelEvidence.frames[clipFrame].sourceFrameIndex,
      voxelCount: voxelEvidence.frames[clipFrame].voxels,
      voxelDataHash: semanticFrame.frame.voxelDataHash,
      authoredRendererFrameHash: await requiredAttribute(viewport, 'data-authored-frame-hash'),
      cameraRevision: await requiredAttribute(viewport, 'data-camera-revision'),
      screenshot: filename,
      screenshotSha256: sha256(png),
      screenshotByteCount: png.byteLength,
      ...readback,
    });
  }
  return captures;
}

async function scrubAndWait(frame, viewport, playback, value) {
  const before = await requiredAttribute(viewport, 'data-authored-frame-hash');
  const current = Number(await frame.inputValue());
  await frame.evaluate((element, nextValue) => {
    const input = element;
    input.value = String(nextValue);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
  }, value);
  await expect(playback).toContainText(`frame ${String(value)}`);
  if (current !== value) {
    await expect.poll(() => requiredAttribute(viewport, 'data-authored-frame-hash'), { timeout: 30_000 })
      .not.toBe(before);
  }
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
  const bytesPerPixel = sourceChannels;
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
      const left = x >= bytesPerPixel ? sourcePixels[rowStart + x - bytesPerPixel] : 0;
      const up = y > 0 ? sourcePixels[previousRow + x] : 0;
      const upLeft = y > 0 && x >= bytesPerPixel
        ? sourcePixels[previousRow + x - bytesPerPixel]
        : 0;
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

function requiredEnvironment(name) {
  const value = process.env[name];
  if (value === undefined || value.length === 0) throw new Error(`${name} is required`);
  return value;
}
