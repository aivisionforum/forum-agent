import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const overlay = readFileSync(new URL('../src/Overlay.svelte', import.meta.url), 'utf8');
const overlayCss = readFileSync(new URL('../src/overlay.css', import.meta.url), 'utf8');
const api = readFileSync(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const app = readFileSync(new URL('../src/App.svelte', import.meta.url), 'utf8');

test('preserves the main translation reveal state machine in all three modes', () => {
  for (const required of [
    'receivedTranslation',
    'visibleTranslation',
    'function sharedPrefix',
    'function scheduleReveal',
    'function revealNext',
    'function syncTranslatingSentence'
  ]) {
    assert.ok(overlay.includes(required), `missing main reveal behavior: ${required}`);
  }
  assert.equal(overlay.match(/\{visibleTranslation\}/g)?.length, 3);
});

test('does not introduce a second caption projection or render-status bridge', () => {
  for (const forbidden of [
    'currentCaption',
    'fitLatestText',
    'caption-feed',
    'caption-renderer',
    'reportCaptionRenderStatus'
  ]) {
    assert.ok(!overlay.includes(forbidden), `unexpected overlay projection: ${forbidden}`);
  }
  assert.ok(!api.includes('CaptionRenderStatus'));
  assert.ok(!app.includes('captionRenderStatus'));
});

test('uses one resizable subtitle window without a fullscreen mode setting', () => {
  assert.ok(!app.includes('overlayFullscreen'));
  assert.ok(!app.includes("tr('窗口模式', 'WINDOW MODE')"));
  assert.ok(!api.includes('overlayFullscreen'));
});

test('normalizes no-translation controls to source-only captions', () => {
  assert.ok(app.includes("disabled={settings.targetLanguage === 'none'}"));
  assert.ok(app.includes("tr('纯中文或纯英文模式只显示原文。'"));
  assert.ok(app.includes("tr('双语', 'DUAL')"));
  assert.ok(app.includes("tr('仅译文', 'TRANSLATION')"));
  assert.ok(app.includes("tr('逐句双行', 'SENTENCE PAIRS')"));
  assert.ok(!app.includes("tr('仅原文', 'SOURCE ONLY')"));
  assert.ok(overlay.includes("state?.targetLanguage === 'none'"));
});

test('applies the configured vertical anchor to caption feeds', () => {
  assert.ok(overlay.includes('--configured-caption-anchor:'));
  assert.ok(overlay.includes('anchor-track'));
  assert.ok(overlay.includes('anchor-tail'));
  assert.ok(api.includes("previewMode.get('anchor')"));
});

test('forces captions to the bottom when the compact tier hides the footer', () => {
  assert.ok(overlayCss.includes('--caption-anchor: var(--configured-caption-anchor)'));
  assert.match(
    overlayCss,
    /\.overlay-shell\.chrome-tier-4\s*\{[^}]*--caption-anchor:\s*100%/s
  );
});

test('offers an enabled-by-default screen wake lock for live translation', () => {
  assert.ok(api.includes('keepAwakeDuringTranslation: boolean'));
  assert.ok(api.includes('keepAwakeDuringTranslation: true'));
  assert.ok(app.includes("tr('实时翻译时保持屏幕唤醒', 'KEEP SCREEN AWAKE WHILE LIVE')"));
  assert.ok(app.includes('keepAwakeDuringTranslation = true'));
  assert.ok(app.includes('keepAwakeDuringTranslation = false'));
});
