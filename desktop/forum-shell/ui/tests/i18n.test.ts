import assert from 'node:assert/strict';
import test from 'node:test';
import { initialLocale, localeKey, translate, displayUrlWithLocale } from '../src/lib/i18n/locale.ts';
import { english } from '../src/lib/i18n/messages.ts';
import { speakerText } from '../src/lib/forum/speakers.ts';
import { wallMessage } from '../src/lib/forum/insight-wall.ts';
import type { PublicSnapshot } from '../src/lib/forum/client.ts';
type WallPhase = NonNullable<PublicSnapshot['wall']>['phase'];

test('operator and wall preferences are independent, with explicit URL selection and safe fallbacks', () => {
  const stored = new Map([[localeKey('operator'), 'zh'], [localeKey('wall'), 'en']]);
  const storage = { getItem: (key: string) => stored.get(key) ?? null };
  assert.equal(initialLocale('operator', '', storage), 'zh');
  assert.equal(initialLocale('wall', '', storage), 'en');
  assert.equal(initialLocale('wall', '?lang=zh', storage), 'zh');
  assert.equal(initialLocale('wall', '?lang=fr', storage), 'en');
  assert.equal(initialLocale('operator', '?lang=en', {getItem: () => { throw new Error('blocked'); }}), 'en');
  assert.equal(initialLocale('wall', '', {getItem: () => { throw new Error('blocked'); }}), 'zh');
  assert.equal(initialLocale('operator', '', {getItem: () => 'invalid'}), 'zh');
});

test('locale-bearing display links retain credentials, session and recap view', () => {
  const input = 'http://127.0.0.1:4242/display.html?session=abc&view=recap#token=private-token';
  const url = new URL(displayUrlWithLocale(input, 'en'));
  assert.equal(url.searchParams.get('session'), 'abc');
  assert.equal(url.searchParams.get('view'), 'recap');
  assert.equal(url.searchParams.get('lang'), 'en');
  assert.equal(url.hash, '#token=private-token');
});

test('all catalog placeholders agree across languages and interpolation preserves supplied content', () => {
  const placeholders = (text: string) => [...text.matchAll(/\{\d+\}/g)].map(m => m[0]).sort();
  for (const [zh, en] of Object.entries(english)) {
    assert.ok(en.trim(), `Empty translation: ${zh}`);
    assert.deepEqual(placeholders(en), placeholders(zh), zh);
    assert.ok(!/\p{Script=Han}/u.test(en), `Untranslated English interface text: ${zh}`);
  }
  assert.equal(translate('en', '责任人：{0}', '中文姓名 {1}'), 'Owner: 中文姓名 {1}');
  assert.equal(translate('zh', '责任人：{0}', 'Alice'), '责任人：Alice');
  assert.equal(translate('en', '未知诊断'), '未知诊断');
  assert.equal(translate('en', 'constructor'), 'constructor');
});

test('every public wall phase has translated status text, without modifying snapshots', () => {
  for (const phase of ['listening','working','paused','delayed','finished'] as WallPhase[]) {
    const snapshot: PublicSnapshot = {cursor:1, artifacts:[], wall:{phase,next_update_at_ms:180000,server_time_ms:0}};
    const before = structuredClone(snapshot);
    const {label, detail} = wallMessage(snapshot, 0);
    assert.ok(!/\p{Script=Han}/u.test(translate('en', label)));
    assert.ok(!/\p{Script=Han}/u.test(translate('en', detail)));
    assert.deepEqual(snapshot, before);
  }
  assert.equal(translate('en', wallMessage(null, 0).detail), 'Waiting for new discussion highlights');
  assert.equal(speakerText({kind:'unknown'}, 'en'), 'Unknown speaker');
  assert.equal(speakerText({kind:'overlap'}, 'zh'), '重叠发言');
});
