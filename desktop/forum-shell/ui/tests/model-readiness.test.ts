import assert from 'node:assert/strict';
import test from 'node:test';
import type { ModelStatus } from '../src/lib/api';
import { needsModelDownload, runtimeIssue } from '../src/lib/model-readiness.ts';
const ready: ModelStatus = {coreReady:true, asrReady:true, translationReady:true, automaticAsrReady:true,
  translationRuntimeReady:true, downloading:false, component:null, progress:1, title:'Ready', detail:'', coreDownloadBytes:0};

test('existing models with a missing ASR runtime never trigger another model download', () => {
  const status = {...ready, automaticAsrReady:false, automaticAsrDetail:'Missing Python adapter'};
  for (const targetLanguage of ['none','bilingual','en'] as const) {
    assert.equal(needsModelDownload(status, {targetLanguage}), false);
    assert.equal(runtimeIssue(status, {targetLanguage}), 'Missing Python adapter');
  }
});

test('transcription needs neither translation weights nor a translation runtime', () => {
  const status = {...ready, coreReady:false, translationReady:false, translationRuntimeReady:false};
  assert.equal(needsModelDownload(status, {targetLanguage:'none'}), false);
  assert.equal(runtimeIssue(status, {targetLanguage:'none'}), '');
  assert.equal(needsModelDownload(status, {targetLanguage:'bilingual'}), true);
  const weightsPresent = {...status, translationReady:true, translationRuntimeDetail:'Missing translation adapter'};
  assert.equal(needsModelDownload(weightsPresent, {targetLanguage:'bilingual'}), false);
  assert.equal(runtimeIssue(weightsPresent, {targetLanguage:'bilingual'}), 'Missing translation adapter');
});

test('missing speech weights require setup in every meeting mode; ready models and runtimes do not', () => {
  for (const targetLanguage of ['none','bilingual','en'] as const) {
    assert.equal(needsModelDownload({...ready, asrReady:false}, {targetLanguage}), true);
    assert.equal(needsModelDownload(ready, {targetLanguage}), false);
    assert.equal(runtimeIssue(ready, {targetLanguage}), '');
  }
});
