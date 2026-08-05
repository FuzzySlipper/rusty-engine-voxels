import { createRequire } from 'node:module';
import { join } from 'node:path';

const providerRoot = requiredEnvironment('RUSTY_STUDIO_PROVIDER_ROOT');
const require = createRequire(import.meta.url);
const { defineConfig } = require(join(providerRoot, 'studio/node_modules/@playwright/test/index.js'));
const adapterBinary = requiredEnvironment('RUSTY_STUDIO_ADAPTER_BINARY');
const settingsRoot = requiredEnvironment('RUSTY_STUDIO_SETTINGS_ROOT');
const port = Number(process.env.RUSTY_STUDIO_PORT ?? '4313');
const baseURL = `http://127.0.0.1:${String(port)}`;

export default defineConfig({
  testDir: new URL('.', import.meta.url).pathname,
  testMatch: 'reference-media-review.spec.mjs',
  timeout: 240_000,
  fullyParallel: false,
  workers: 1,
  use: {
    baseURL,
    browserName: 'chromium',
    headless: true,
    launchOptions: {
      args: ['--enable-webgl', '--ignore-gpu-blocklist', '--use-angle=swiftshader'],
    },
  },
  webServer: {
    command: `pnpm --dir ${shellArgument(join(providerRoot, 'studio'))} run host -- --adapter-binary ${shellArgument(adapterBinary)} --settings-root ${shellArgument(settingsRoot)} --port ${String(port)}`,
    url: `${baseURL}/health`,
    reuseExistingServer: false,
    timeout: 30_000,
  },
});

function requiredEnvironment(name) {
  const value = process.env[name];
  if (value === undefined || value.length === 0) throw new Error(`${name} is required`);
  return value;
}

function shellArgument(value) {
  return `'${value.replaceAll("'", "'\\''")}'`;
}
