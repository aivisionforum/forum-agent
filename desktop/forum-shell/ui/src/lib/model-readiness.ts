import type { ModelStatus, TranslationSettings } from './api';

// Missing Python adapters are repaired separately from model weight downloads.
export function needsModelDownload(status: ModelStatus, settings: Pick<TranslationSettings, 'targetLanguage'>): boolean {
  return !(status.asrReady ?? status.coreReady)
    || (settings.targetLanguage !== 'none' && !(status.translationReady ?? status.coreReady));
}

export function runtimeIssue(status: ModelStatus | null, settings: Pick<TranslationSettings, 'targetLanguage'>): string {
  if (!status || needsModelDownload(status, settings)) return '';
  // All speech modes now use the Python ASR adapter, including fixed languages.
  if (status.automaticAsrReady === false) return status.automaticAsrDetail || 'ASR runtime is unavailable';
  if (settings.targetLanguage !== 'none' && status.translationRuntimeReady === false) {
    return status.translationRuntimeDetail || 'Translation runtime is unavailable';
  }
  return '';
}
