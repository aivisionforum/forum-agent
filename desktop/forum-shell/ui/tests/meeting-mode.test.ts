import assert from 'node:assert/strict';
import test from 'node:test';
import type { TranslationSettings } from '../src/lib/api';
import { meetingMode, settingsForMeetingMode } from '../src/lib/meeting-mode.ts';

const saved: TranslationSettings = {
  appLanguage: 'zh', accentTheme: 'neon-blue', sourceLanguage: 'zh', targetLanguage: 'en',
  inputDevice: '__dual_audio__', subtitleSplit: false, translationOnly: true,
  overlayOpacity: 100, fontSizePreset: '44', anchorPositionPreset: 'bottom',
  finalIntervalSeconds: 5, keepAwakeDuringTranslation: true, recordingEnabled: true,
  speakersEnabled: true, autoSaveTranscript: true, periodicSaveTranscript: true,
  transcriptFileName: 'meeting', transcriptSaveDir: null
};

test('single-language meetings disable translation and keep the original visible', () => {
  for (const language of ['zh', 'en'] as const) {
    const result = settingsForMeetingMode(saved, language);
    assert.equal(result.sourceLanguage, language);
    assert.equal(result.targetLanguage, 'none');
    assert.equal(result.translationOnly, false);
    assert.equal(result.subtitleSplit, true);
    assert.equal(meetingMode(result), language);
    for (const key of ['inputDevice', 'recordingEnabled', 'speakersEnabled', 'fontSizePreset', 'finalIntervalSeconds'] as const) {
      assert.equal(result[key], saved[key]);
    }
  }
  assert.equal(saved.targetLanguage, 'en', 'does not mutate saved settings');
});

test('mixed meetings use automatic ASR and bilingual translation after either single-language mode', () => {
  for (const language of ['zh', 'en'] as const) {
    const result = settingsForMeetingMode(settingsForMeetingMode(saved, language), 'mixed');
    assert.equal(result.sourceLanguage, 'auto');
    assert.equal(result.targetLanguage, 'bilingual');
    assert.equal(result.translationOnly, false);
    assert.equal(meetingMode(result), 'mixed');
  }
});

test('legacy fixed translation directions migrate to their matching transcription mode', () => {
  for (const language of ['zh', 'en'] as const) {
    const legacy = {...saved, sourceLanguage: language};
    const updated = settingsForMeetingMode(legacy, meetingMode(legacy));
    assert.equal(updated.sourceLanguage, language);
    assert.equal(updated.targetLanguage, 'none');
    assert.deepEqual(settingsForMeetingMode(updated, meetingMode(updated)), updated);
  }
});
