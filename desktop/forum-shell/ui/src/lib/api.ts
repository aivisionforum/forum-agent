import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { SessionSummary, SnapshotPage, TranslationRecord, PageKey } from '../../../../../packages/contracts/forum.generated';
export type { SessionSummary, SnapshotItem, TranslationRecord } from '../../../../../packages/contracts/forum.generated';

export type MeetingPage = SnapshotPage & { translations: TranslationRecord[] };
export async function getMeetingSessions(): Promise<SessionSummary[]> {
  return isTauri() ? invoke('get_meeting_sessions') : [];
}
export async function getMeetingTranscript(sessionId: string, cursor: number | null = null, after: PageKey | null = null): Promise<MeetingPage> {
  return invoke('get_meeting_transcript', { sessionId, cursor, after });
}
export async function recoverMeeting(sessionId: string): Promise<RuntimeState> {
  return invoke('recover_meeting', { sessionId });
}

export type LanguageCode = 'auto' | 'bilingual' | 'zh' | 'en' | 'ja' | 'fr' | 'none';

export interface TranslationSettings {
  appLanguage: 'zh' | 'en';
  accentTheme: AccentTheme;
  sourceLanguage: LanguageCode;
  targetLanguage: LanguageCode;
  inputDevice: string;
  inputGain: number;
  subtitleSplit: boolean;
  subtitleSideBySide: boolean;
  translationOnly: boolean;
  overlayOpacity: number;
  fontSizePreset: string;
  anchorPositionPreset: string;
  finalIntervalSeconds: number;
  keepAwakeDuringTranslation: boolean;
  recordingEnabled: boolean;
  speakersEnabled: boolean;
  autoSaveTranscript: boolean;
  periodicSaveTranscript: boolean;
  transcriptFileName: string;
  transcriptSaveDir: string | null;
}

export interface SettingsPayload {
  settings: TranslationSettings;
  inputDevices: string[];
  subtitlePreviewVisible: boolean;
  running: boolean;
  runtimeStatus: string;
  runtimeMessage: string;
}

export interface RuntimeState {
  running: boolean;
  status: string;
  message: string;
}

export interface DirectionSwitchState {
  sourceLanguage: string;
  targetLanguage: string;
  activeSourceLanguage: string;
  activeTargetLanguage: string;
  epoch: number;
  activeEpoch: number;
  pending: boolean;
}

export interface ModelStatus {
  coreReady: boolean;
  asrReady?: boolean;
  translationReady?: boolean;
  automaticAsrReady?: boolean;
  automaticAsrDetail?: string;
  translationRuntimeReady?: boolean;
  translationRuntimeDetail?: string;
  downloading: boolean;
  component: 'core' | null;
  progress: number;
  title: string;
  detail: string;
  coreDownloadBytes: number;
}

export interface UsageSnapshot {
  currentSessionSeconds: number;
  monthlySeconds: number;
  lifetimeSeconds: number;
  completedSessions: number;
  running: boolean;
  monthKey: string;
}

export interface Sentence {
  sourceLanguage?: string;
  languageTexts?: Record<string,string>;
  segmentId?: string | null;
  sourceRevision?: number | null;
  sourceText: string;
  translation: string;
}

export interface TranslatingSentence extends Sentence {
  commitId: number;
  complete: boolean;
}

export interface OverlayState {
  appLanguage?: 'zh' | 'en';
  sessionId?: string | null;
  runtimeMessage?: string;
  active: boolean;
  status: string;
  sourceLanguage: string;
  targetLanguage: string;
  subtitleSplit: boolean;
  subtitleSideBySide: boolean;
  translationOnly: boolean;
  fontSize: number;
  anchorPosition: number;
  accentTheme: AccentTheme;
  history: Sentence[];
  translating: TranslatingSentence | null;
  pendingSourceText: string;
}

export type AccentTheme = 'neon-blue' | 'neon-orange' | 'neon-pink' | 'neon-green';

export const previewSettings: SettingsPayload = {
  settings: {
    appLanguage: 'zh',
    accentTheme: 'neon-blue',
    sourceLanguage: 'zh',
    targetLanguage: 'en',
    inputDevice: '__system_audio__',
    inputGain: 1,
    subtitleSplit: false,
    subtitleSideBySide: false,
    translationOnly: false,
    overlayOpacity: 1,
    fontSizePreset: '24',
    anchorPositionPreset: '50',
    finalIntervalSeconds: 3,
    keepAwakeDuringTranslation: true,
    recordingEnabled: true,
    speakersEnabled: false,
    autoSaveTranscript: false,
    periodicSaveTranscript: false,
    transcriptFileName: 'transcript.md',
    transcriptSaveDir: null
  },
  inputDevices: ['__dual_audio__', '__system_audio__', '__default_microphone__', 'MacBook Pro Microphone'],
  subtitlePreviewVisible: true,
  running: false,
  runtimeStatus: 'idle',
  runtimeMessage: 'Browser preview — synthetic data only'
};

export const previewOverlay: OverlayState = {
  active: false,
  status: 'idle',
  sourceLanguage: 'zh',
  targetLanguage: 'en',
  subtitleSplit: false,
  subtitleSideBySide: false,
  translationOnly: false,
  fontSize: 24,
  anchorPosition: 50,
  accentTheme: 'neon-blue',
  history: [
    {
      segmentId: '00000000-0000-4000-8000-000000000002',
      sourceRevision: 1,
      sourceText: '这是一段用于调整字幕大小和布局的测试内容。',
      translation: 'This sample helps you adjust subtitle size and layout.'
    },
    {
      segmentId: '00000000-0000-4000-8000-000000000003',
      sourceRevision: 1,
      sourceText: '请确认每句话都清晰、易读，并适合现场屏幕。',
      translation: 'Check that every sentence is clear, readable, and suitable for the venue screen.'
    }
  ],
  translating: null,
  pendingSourceText: ''
};

function previewSentences(count: number): Sentence[] {
  return Array.from({ length: count }, (_, index) => ({
    sourceText: `第 ${index + 1} 句原文会作为独立段落显示，便于观众跟随现场内容。`,
    translation: `Sentence ${index + 1} appears as a separate passage so the audience can follow the live presentation clearly.`
  }));
}

export function isTauri(): boolean {
  return typeof window !== 'undefined' && Boolean(window.__TAURI_INTERNALS__);
}

export async function getSettings(): Promise<SettingsPayload> {
  return isTauri() ? invoke<SettingsPayload>('get_settings') : structuredClone(previewSettings);
}

export async function getModelStatus(): Promise<ModelStatus> {
  if (isTauri()) return invoke<ModelStatus>('get_model_status');
  return {
    coreReady: true,
    asrReady: true,
    translationReady: true,
    automaticAsrReady: true,
    translationRuntimeReady: true,
    downloading: false,
    component: null,
    progress: 1,
    title: 'Preview',
    detail: 'Sample model status; no hardware checked',
    coreDownloadBytes: 4_222_472_192
  };
}

export async function startModelDownload(component: 'core'): Promise<ModelStatus> {
  if (isTauri()) return invoke<ModelStatus>('start_model_download', { component });
  return getModelStatus();
}

export async function updateSettings(settings: TranslationSettings): Promise<void> {
  if (isTauri()) await invoke('update_settings', { settings });
}

export async function getUsage(): Promise<UsageSnapshot> {
  if (isTauri()) return invoke<UsageSnapshot>('get_usage');
  return {
    currentSessionSeconds: 0,
    monthlySeconds: 0,
    lifetimeSeconds: 0,
    completedSessions: 0,
    running: false,
    monthKey: new Date().toISOString().slice(0, 7)
  };
}

export async function startTranslation(settings: TranslationSettings, insightMode: 'gated' | 'automatic' = 'gated'): Promise<RuntimeState> {
  if (!isTauri()) throw new Error('Browser preview only. Open the desktop app to capture audio.');
  return invoke<RuntimeState>('start_translation', { settings, insightMode });
}

export async function stopTranslation(): Promise<RuntimeState> {
  if (!isTauri()) return { running: false, status: 'idle', message: 'Translation stopped' };
  return invoke<RuntimeState>('stop_translation');
}

export async function swapTranslationDirection(settings: TranslationSettings): Promise<DirectionSwitchState> {
  if (isTauri()) return invoke<DirectionSwitchState>('swap_translation_direction', { settings });
  return {
    sourceLanguage: settings.sourceLanguage,
    targetLanguage: settings.targetLanguage,
    activeSourceLanguage: settings.sourceLanguage,
    activeTargetLanguage: settings.targetLanguage,
    epoch: 1,
    activeEpoch: 1,
    pending: false
  };
}

export async function getDirectionSwitchState(): Promise<DirectionSwitchState> {
  if (isTauri()) return invoke<DirectionSwitchState>('get_direction_switch_state');
  return {
    sourceLanguage: previewSettings.settings.sourceLanguage,
    targetLanguage: previewSettings.settings.targetLanguage,
    activeSourceLanguage: previewSettings.settings.sourceLanguage,
    activeTargetLanguage: previewSettings.settings.targetLanguage,
    epoch: 0,
    activeEpoch: 0,
    pending: false
  };
}

export async function getOverlayState(): Promise<OverlayState> {
  if (isTauri()) return invoke<OverlayState>('get_overlay_state');
  const previewMode = new URLSearchParams(window.location.search);
  if (previewMode.get('idle') === '1') {
    return { ...structuredClone(previewOverlay), active: false, status: 'idle', history: [], pendingSourceText: '' };
  }
  const preview = structuredClone(previewOverlay);
  if (previewMode.get('long') === '1') {
    preview.history = previewSentences(12);
    preview.pendingSourceText = '';
  }
  if (previewMode.get('flow') === '1') {
    preview.history = previewSentences(3);
    preview.pendingSourceText = '';
  }
  preview.subtitleSideBySide = previewMode.get('columns') === '1';
  if (previewMode.get('stacked') === '1') {
    preview.subtitleSplit = false;
  }
  if (previewMode.get('translationOnly') === '1') {
    preview.translationOnly = true;
  }
  if (previewMode.get('sourceOnly') === '1') {
    preview.targetLanguage = 'none';
    preview.subtitleSplit = true;
    preview.translationOnly = false;
    preview.history = preview.history.map((sentence) => ({ ...sentence, translation: '' }));
  }
  const anchorPosition = Number(previewMode.get('anchor'));
  if ([35, 50, 70, 100].includes(anchorPosition)) {
    preview.anchorPosition = anchorPosition;
  }
  if (previewMode.get('pending') === '1') {
    preview.history = [];
    preview.translating = null;
    preview.pendingSourceText = '正在等待译文';
  }
  return preview;
}

export async function openTranscriptHistory(): Promise<void> {
  if (isTauri()) await invoke('open_transcript_history');
}

let browserSubtitlePreviewVisible = true;

export async function toggleSubtitlePreview(): Promise<boolean> {
  if (isTauri()) return invoke<boolean>('toggle_subtitle_preview');
  browserSubtitlePreviewVisible = !browserSubtitlePreviewVisible;
  return browserSubtitlePreviewVisible;
}

export async function startWindowDrag(): Promise<void> {
  if (isTauri()) await getCurrentWindow().startDragging();
}

export async function listenRuntime(handler: (state: RuntimeState) => void): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<RuntimeState>('runtime-state', ({ payload }) => handler(payload));
}

export async function listenInputDevices(handler: (devices: string[]) => void): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<string[]>('input-devices', ({ payload }) => handler(payload));
}

export async function listenDirectionSwitchState(
  handler: (state: DirectionSwitchState) => void
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<DirectionSwitchState>('direction-switch-state', ({ payload }) => handler(payload));
}

export async function listenOverlay(handler: (state: OverlayState) => void): Promise<UnlistenFn> {
  if (!isTauri()) {
    const previewMode = new URLSearchParams(window.location.search);
    if (previewMode.get('flow') === '1') {
      const flowState = (count: number): OverlayState => ({
        ...structuredClone(previewOverlay),
        subtitleSplit: previewMode.get('stacked') !== '1',
        history: previewSentences(count),
        pendingSourceText: ''
      });
      const firstTimer = window.setTimeout(() => {
        handler(flowState(6));
      }, 450);
      const secondTimer = window.setTimeout(() => {
        handler(flowState(12));
      }, 1050);
      return () => {
        window.clearTimeout(firstTimer);
        window.clearTimeout(secondTimer);
      };
    }
    return () => undefined;
  }
  return listen<OverlayState>('overlay-state', ({ payload }) => handler(payload));
}

export async function showMeetingWindow(): Promise<void> {
  if (isTauri()) await invoke('show_meeting_window');
}
export async function showMeetingWorkspace(sessionId: string | null, view: 'forum' | 'settings' = 'forum'): Promise<void> {
  if (isTauri()) await invoke('show_meeting_workspace', { sessionId, view });
}
export async function listenWorkspace(handler: (sessionId: string | null) => void): Promise<UnlistenFn> {
  return isTauri() ? listen<string | null>('open-forum-session', ({payload}) => handler(payload)) : () => {};
}
export async function meetingWindowAction(action: 'close' | 'minimize' | 'fullscreen' | 'pin', pinned = false): Promise<void> {
  if (!isTauri()) return;
  const window = getCurrentWindow();
  if (action === 'close') await window.close();
  if (action === 'minimize') await window.minimize();
  if (action === 'fullscreen') await window.setFullscreen(!(await window.isFullscreen()));
  if (action === 'pin') await window.setAlwaysOnTop(pinned);
}

export async function listenLiveSettings(handler: () => void): Promise<UnlistenFn> {
  return isTauri() ? listen('open-live-settings', handler) : () => {};
}

export interface AudioLevelSample { rms: number; peak: number }
export async function listenAudioLevel(handler: (samples: AudioLevelSample[]) => void): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<AudioLevelSample[]>('audio-level', ({payload}) => handler(payload));
}
