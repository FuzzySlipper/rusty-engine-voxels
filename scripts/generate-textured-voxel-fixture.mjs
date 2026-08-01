#!/usr/bin/env node

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(root, "content/textures/directional-atlas.png");
const width = 16;
const height = 8;

function crc32(bytes) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit += 1) {
      crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(kind, payload) {
  const name = Buffer.from(kind, "ascii");
  const length = Buffer.alloc(4);
  length.writeUInt32BE(payload.length);
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(crc32(Buffer.concat([name, payload])));
  return Buffer.concat([length, name, payload, checksum]);
}

function adler32(bytes) {
  let a = 1;
  let b = 0;
  for (const byte of bytes) {
    a = (a + byte) % 65521;
    b = (b + a) % 65521;
  }
  return ((b << 16) | a) >>> 0;
}

function deterministicZlibStore(bytes) {
  const parts = [Buffer.from([0x78, 0x01])];
  for (let offset = 0; offset < bytes.length; offset += 65_535) {
    const end = Math.min(offset + 65_535, bytes.length);
    const length = end - offset;
    const header = Buffer.alloc(5);
    header[0] = end === bytes.length ? 1 : 0;
    header.writeUInt16LE(length, 1);
    header.writeUInt16LE((~length) & 0xffff, 3);
    parts.push(header, bytes.subarray(offset, end));
  }
  const checksum = Buffer.alloc(4);
  checksum.writeUInt32BE(adler32(bytes));
  parts.push(checksum);
  return Buffer.concat(parts);
}

function directionalPixel(region, x, y) {
  const localX = Math.min(5, Math.max(0, x - (region === 0 ? 1 : 9)));
  const localY = Math.min(5, Math.max(0, y - 1));
  const palette = region === 0
    ? { base: [188, 46, 31, 255], edge: [255, 190, 30, 255], mark: [255, 255, 255, 255], corner: [44, 12, 8, 255] }
    : { base: [22, 86, 176, 255], edge: [22, 220, 190, 255], mark: [255, 240, 56, 255], corner: [8, 20, 60, 255] };
  if (localX === 0 && localY === 0) return palette.corner;
  if (localX === 5 || localY === 5) return palette.edge;
  if (localX === localY || (localY === 2 && localX >= 2)) return palette.mark;
  return palette.base;
}

const raw = Buffer.alloc((width * 4 + 1) * height);
for (let y = 0; y < height; y += 1) {
  const row = y * (width * 4 + 1);
  raw[row] = 0;
  for (let x = 0; x < width; x += 1) {
    const region = x < 8 ? 0 : 1;
    const [r, g, b, a] = directionalPixel(region, x, y);
    raw.set([r, g, b, a], row + 1 + x * 4);
  }
}

const header = Buffer.alloc(13);
header.writeUInt32BE(width, 0);
header.writeUInt32BE(height, 4);
header.set([8, 6, 0, 0, 0], 8);
const png = Buffer.concat([
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
  chunk("IHDR", header),
  chunk("IDAT", deterministicZlibStore(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);

if (process.argv.includes("--check")) {
  const existing = readFileSync(output);
  if (!existing.equals(png)) {
    throw new Error(`${output} is stale; run scripts/generate-textured-voxel-fixture.mjs`);
  }
} else {
  mkdirSync(dirname(output), { recursive: true });
  writeFileSync(output, png);
}
