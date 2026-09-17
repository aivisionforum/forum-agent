import assert from 'node:assert/strict';
import test from 'node:test';
import { nextChromeTier } from '../src/lib/caption-metrics.ts';

test('responsive chrome uses hysteresis near a height boundary', () => {
  assert.equal(nextChromeTier(0, 319), 2);
  assert.equal(nextChromeTier(2, 329), 2);
  assert.equal(nextChromeTier(2, 341), 1);
});

test('responsive chrome restores the full layout after the viewport grows', () => {
  const constrained = nextChromeTier(0, 120);
  assert.equal(constrained, 4);
  assert.equal(nextChromeTier(constrained, 600), 0);
});

test('responsive chrome accounts for large subtitle fonts', () => {
  assert.equal(nextChromeTier(0, 200, 43), 4);
  assert.equal(nextChromeTier(4, 260, 43), 4);
  assert.equal(nextChromeTier(4, 360, 43), 3);
});
