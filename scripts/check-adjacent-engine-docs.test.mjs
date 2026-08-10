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
  for (const [historical, current] of [
    [
      'Historical evidence records an Engine pin',
      'production now uses the Engine provider pin',
    ],
    [
      'Production now uses the Engine provider pin',
      'although historical evidence records an older Engine pin',
    ],
  ]) {
    for (const separator of ['. ', '; ', ', but ']) {
      assert.throws(
        () =>
          validateAdjacentEngineDocs({
            'docs/design.md': `${historical}${separator}${current}.`,
          }),
        /stale current Engine dependency guidance/,
      );
    }
  }
});

test('rejects qualified no-pin wording beside an affirmative pin', () => {
  for (const guidance of [
    'There is no provider pin rollback, but production uses the Engine provider pin.',
    'Without a provider pin migration, the Engine provider remains pinned to revision abcdef1.',
    'Production uses the Engine provider pin, although there is no provider pin rollback.',
    'The Engine provider remains pinned to revision abcdef1, despite no provider pin migration.',
  ]) {
    assert.throws(
      () =>
        validateAdjacentEngineDocs({
          'docs/design.md': guidance,
        }),
      /stale current Engine dependency guidance/,
    );
  }
});

test('rejects unrelated negation of an unpinned provider', () => {
  assert.throws(
    () =>
      validateAdjacentEngineDocs({
        'docs/design.md':
          'The Engine provider is not unpinned; it remains pinned to revision abcdef1.',
      }),
    /stale current Engine dependency guidance/,
  );
});

test('allows negative no-pin guidance', () => {
  for (const guidance of [
    'This repository has no provider pin.',
    'There is no Engine pin.',
    'Without a provider pin.',
  ]) {
    assert.doesNotThrow(() =>
      validateAdjacentEngineDocs({ 'README.md': guidance }),
    );
  }
});

test('allows explicitly historical Engine revision evidence', () => {
  assert.doesNotThrow(() =>
    validateAdjacentEngineDocs({
      'docs/evidence.md':
        'Historically, the Engine pin advanced to revision abcdef1.',
    }),
  );
});
