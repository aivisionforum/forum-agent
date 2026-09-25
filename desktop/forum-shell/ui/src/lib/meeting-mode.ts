import type { TranslationSettings } from './api';

export type MeetingMode = 'mixed' | 'zh' | 'en';

export function meetingMode(settings: TranslationSettings): MeetingMode {
  return settings.sourceLanguage === 'auto' ? 'mixed' : settings.sourceLanguage === 'en' ? 'en' : 'zh';
}

export function settingsForMeetingMode(settings: TranslationSettings, mode: MeetingMode): TranslationSettings {
  return {...settings, sourceLanguage: mode === 'mixed' ? 'auto' : mode,
    targetLanguage: mode === 'mixed' ? 'bilingual' : 'none',
    translationOnly: false, subtitleSplit: mode === 'mixed' ? settings.subtitleSplit : true};
}
