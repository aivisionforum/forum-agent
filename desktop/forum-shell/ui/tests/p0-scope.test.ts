import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const overlay = readFileSync(new URL('../src/Overlay.svelte', import.meta.url), 'utf8');
const overlayCss = readFileSync(new URL('../src/overlay.css', import.meta.url), 'utf8');
const api = readFileSync(new URL('../src/lib/api.ts', import.meta.url), 'utf8');
const settings = readFileSync(new URL('../src/components/ApplicationSettings.svelte', import.meta.url), 'utf8');
const app = readFileSync(new URL('../src/App.svelte', import.meta.url), 'utf8');

test('audience captions never import or render the private analysis console', () => {
  for (const forbidden of ['LiveInsights', 'SpeakerClient', 'captionLabels', 'runtimeMessage', 'meeting-divider', 'review_artifact', 'forumClient']) {
    assert.ok(!overlay.includes(forbidden), `private console leaked to audience captions: ${forbidden}`);
  }
  assert.ok(app.includes('<LiveInsights'));
  assert.ok(app.includes('<StageControls'));
  assert.ok(!overlayCss.includes('--insight-share'));
});

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

test('source-only meetings disable bilingual content and layout controls', () => {
  assert.ok(settings.includes("disabled={settings.targetLanguage === 'none'}"));
  assert.ok(settings.includes("disabled={settings.targetLanguage === 'none' || settings.translationOnly}"));
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
  assert.ok(settings.includes('bind:checked={settings.keepAwakeDuringTranslation}'));
});
