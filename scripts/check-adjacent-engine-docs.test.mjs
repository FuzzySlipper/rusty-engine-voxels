import assert from 'node:assert/strict';
import test from 'node:test';

import { validateAdjacentEngineDocs } from './check-adjacent-engine-docs.mjs';

test('rejects current provider-pin guidance', () => {
  assert.throws(
    () =>
      validateAdjacentEngineDocs({
        'docs/design.md': 'The live gate uses the current provider pin.',
      }),
    /docs\/design\.md contains stale current Engine dependency guidance/,
  );
});

test('rejects a current claim beside historical evidence', () => {
  assert.throws(
    () =>
      validateAdjacentEngineDocs({
        'docs/design.md':
          'Historical evidence records an Engine pin. The current Engine provider is pinned to revision abcdef1.',
      }),
    /stale current Engine dependency guidance/,
  );
});

test('allows negative no-pin guidance', () => {
  assert.doesNotThrow(() =>
    validateAdjacentEngineDocs({
      'README.md': 'This repository has no provider pin or Engine updater.',
    }),
  );
});

test('allows explicitly historical Engine revision evidence', () => {
  assert.doesNotThrow(() =>
    validateAdjacentEngineDocs({
      'docs/evidence.md':
        'Historically, the Engine pin advanced to revision abcdef1.',
    }),
  );
});
