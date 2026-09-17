<script lang="ts">
  import { onMount } from 'svelte';
  import logoUrl from '../../icons/logo-mark.png';
  import { productName, developmentVersion } from './lib/product';
  import {
    getSettings,
    isTauri,
    getModelStatus,
    getUsage,
    getDirectionSwitchState,
    listenDirectionSwitchState,
    listenRuntime,
    listAppleVoices,
    listOutputDevices,
    openAppleVoiceSettings,
    openTranscriptHistory,
    previewSpokenVoice,
    startTranslation,
    startModelDownload,
    stopSpokenVoicePreview,
    stopTranslation,
    swapTranslationDirection,
    toggleSubtitlePreview,
    updateSettings,
    type AccentTheme,
    type RuntimeState,
    type DirectionSwitchState,
    type ModelStatus,
    type UsageSnapshot,
    type SettingsPayload,
    type TranslationSettings
  } from './lib/api';

  type SpokenVoice = {
    id: string;
    name: string;
    locale: string;
  };

  type RequiredAppleVoice = SpokenVoice & {
    target: 'zh' | 'en';
    languageZh: string;
    languageEn: string;
  };

  const languages = [
    { code: 'zh', zh: '中文', en: 'Chinese' },
    { code: 'en', zh: '英语', en: 'English' },
    { code: 'ja', zh: '日语', en: 'Japanese' },
    { code: 'fr', zh: '法语', en: 'French' }
  ] as const;
  const fontSizes = ['16', '20', '24', '30', '36', '44', '52', '64', '80', '96', '120', '160'];
  const chineseSpokenVoices: SpokenVoice[] = [
    { id: 'apple-voice-1', name: 'Yue (Premium)', locale: 'zh_CN' },
    { id: 'apple-voice-2', name: 'Tingting', locale: 'zh_CN' }
  ];
  const englishSpokenVoices: SpokenVoice[] = [1, 2, 3, 4, 5].map((number) => ({
    id: `apple-voice-${number}`,
    name: `Voice ${number}`,
    locale: 'en_US'
  }));
  const requiredAppleVoices: RequiredAppleVoice[] = [
    {
      target: 'zh',
      id: 'apple-voice-1',
      name: 'Yue (Premium)',
      locale: 'zh_CN',
      languageZh: '普通话（中国大陆）',
      languageEn: 'Mandarin Chinese (Mainland China)'
    },
    {
      target: 'en',
      id: 'apple-voice-4',
      name: 'Voice 4',
      locale: 'en_US',
      languageZh: '英语（美国）',
      languageEn: 'English (United States)'
    }
  ];
  const accentThemes: Array<{ id: AccentTheme; zh: string; en: string; color: string }> = [
    { id: 'neon-blue', zh: '电光蓝', en: 'BLUE', color: '#0003FE' },
    { id: 'neon-orange', zh: '霓虹橙', en: 'ORANGE', color: '#FF5705' },
    { id: 'neon-pink', zh: '霓虹粉', en: 'PINK', color: '#FF0073' },
    { id: 'neon-green', zh: '霓虹绿', en: 'GREEN', color: '#51F91B' }
  ];

  let payload: SettingsPayload | null = null;
  let settings: TranslationSettings | null = null;
  let running = false;
  let runtimeStatus = 'idle';
  let runtimeMessage = 'Local AI is ready';
  let directionSwitchPending = false;
  let advancedOpen = false;
  let appSettingsOpen = false;
  let subtitlePreviewVisible = true;
  let subtitlePreviewBusy = false;
  let previewingVoice = '';
  let previewTimer: number | null = null;
  let busy = false;
  let errorMessage = '';
  let modelStatus: ModelStatus | null = null;
  let modelTimer: number | null = null;
  let usage: UsageSnapshot | null = null;
  let usageTimer: number | null = null;
  let refreshingOutputDevices = false;
  let refreshingAppleVoices = false;
  let voiceInstallGuide: RequiredAppleVoice | null = null;
  let voiceCheckTimer: number | null = null;
  let voiceGuideCloseTimer: number | null = null;

  const isEnglish = () => settings?.appLanguage === 'en';
  const tr = (zh: string, en: string) => (isEnglish() ? en : zh);
  const appDisplayName = () => productName;
  const browserPreview = !isTauri();

  function applyAccentTheme(theme: AccentTheme): void {
    document.documentElement.dataset.accentTheme = theme;
  }

  async function selectAccentTheme(theme: AccentTheme): Promise<void> {
    if (!settings) return;
    settings.accentTheme = theme;
    settings = { ...settings };
    applyAccentTheme(theme);
    await persist();
  }

  function languageName(code: string): string {
    if (code === 'none') return tr('不翻译', 'No translation');
    const language = languages.find((item) => item.code === code);
    return language ? (isEnglish() ? language.en : language.zh) : code.toUpperCase();
  }

  function deviceName(value: string): string {
    if (value === '__system_audio__') return tr('系统音频', 'System Audio');
    if (value === '__default_microphone__') return tr('默认麦克风', 'Default Microphone');
    return value;
  }

  function requiredVoiceForTarget(target: string): RequiredAppleVoice | null {
    return requiredAppleVoices.find((voice) => voice.target === target) ?? null;
  }

  function isAppleVoiceInstalled(name: string, locale: string): boolean {
    return payload?.installedAppleVoices.some((voice) => voice.name === name && voice.locale === locale) ?? false;
  }

  function hasWrongLocaleVariant(voice: RequiredAppleVoice): boolean {
    if (isAppleVoiceInstalled(voice.name, voice.locale)) return false;
    return payload?.installedAppleVoices.some((installed) => (
      installed.name === voice.name && installed.locale !== voice.locale
    )) ?? false;
  }

  function voiceStatusLabel(voice: RequiredAppleVoice): string {
    if (isAppleVoiceInstalled(voice.name, voice.locale)) return tr('已安装', 'INSTALLED');
    if (hasWrongLocaleVariant(voice)) return tr('已安装其他语言版本', 'WRONG LANGUAGE INSTALLED');
    return tr('未安装', 'NOT INSTALLED');
  }

  function voiceLanguageName(voice: RequiredAppleVoice): string {
    return isEnglish() ? voice.languageEn : voice.languageZh;
  }

  function voicesForTarget(target: string): ReadonlyArray<SpokenVoice> {
    const candidates = target === 'zh' ? chineseSpokenVoices : target === 'en' ? englishSpokenVoices : [];
    return candidates.filter((voice) => isAppleVoiceInstalled(voice.name, voice.locale));
  }

  function syncVoiceToTarget(): boolean {
    if (!settings) return false;
    const available = voicesForTarget(settings.targetLanguage);
    const required = requiredVoiceForTarget(settings.targetLanguage);
    let changed = false;
    if (available.length === 0) {
      changed = settings.spokenTranslationEnabled || settings.spokenTranslationVoice !== null || changed;
      settings.spokenTranslationEnabled = false;
      settings.spokenTranslationVoice = null;
      return changed;
    }
    const current = settings.spokenTranslationVoice;
    if (current && available.some((voice) => voice.id === current)) return changed;
    settings.spokenTranslationVoice = required && available.some((voice) => voice.id === required.id)
      ? required.id
      : available[0]?.id ?? null;
    return true;
  }

  async function persist(): Promise<void> {
    if (!settings) return;
    errorMessage = '';
    try {
      await updateSettings(settings);
    } catch (error) {
      errorMessage = String(error);
    }
  }

  async function refreshOutputDevices(): Promise<void> {
    if (!payload || !settings || refreshingOutputDevices) return;
    refreshingOutputDevices = true;
    try {
      const devices = await listOutputDevices();
      if (devices.length > 0) {
        payload.outputDevices = devices;
        payload = { ...payload };
        if (
          settings.spokenTranslationOutputDevice !== null &&
          !devices.includes(settings.spokenTranslationOutputDevice)
        ) {
          settings.spokenTranslationOutputDevice = null;
          settings = { ...settings };
          await persist();
        }
      }
    } catch (error) {
      errorMessage = String(error);
    } finally {
      refreshingOutputDevices = false;
    }
  }

  async function refreshAppleVoiceStatus(): Promise<void> {
    if (!payload || !settings || refreshingAppleVoices) return;
    refreshingAppleVoices = true;
    errorMessage = '';
    try {
      const voices = await listAppleVoices();
      payload.installedAppleVoices = voices;
      payload = { ...payload };
      if (syncVoiceToTarget()) {
        settings = { ...settings };
        await persist();
      }
      if (
        voiceInstallGuide &&
        isAppleVoiceInstalled(voiceInstallGuide.name, voiceInstallGuide.locale)
      ) {
        stopVoiceAutoCheck();
        if (voiceGuideCloseTimer === null) {
          voiceGuideCloseTimer = window.setTimeout(() => {
            voiceInstallGuide = null;
            voiceGuideCloseTimer = null;
          }, 1200);
        }
      }
    } catch (error) {
      errorMessage = String(error);
    } finally {
      refreshingAppleVoices = false;
    }
  }

  function stopVoiceAutoCheck(): void {
    if (voiceCheckTimer !== null) {
      window.clearInterval(voiceCheckTimer);
      voiceCheckTimer = null;
    }
  }

  function startVoiceAutoCheck(): void {
    stopVoiceAutoCheck();
    voiceCheckTimer = window.setInterval(() => void refreshAppleVoiceStatus(), 1800);
  }

  function closeVoiceInstallGuide(): void {
    stopVoiceAutoCheck();
    if (voiceGuideCloseTimer !== null) {
      window.clearTimeout(voiceGuideCloseTimer);
      voiceGuideCloseTimer = null;
    }
    voiceInstallGuide = null;
  }

  async function openAppleVoiceDownloads(voice: RequiredAppleVoice): Promise<void> {
    voiceInstallGuide = voice;
    startVoiceAutoCheck();
    try {
      await openAppleVoiceSettings();
    } catch (error) {
      errorMessage = String(error);
    }
  }

  async function reopenAppleVoiceDownloads(): Promise<void> {
    if (voiceInstallGuide) await openAppleVoiceDownloads(voiceInstallGuide);
  }

  async function swapLanguages(): Promise<void> {
    if (!settings || directionSwitchPending || settings.targetLanguage === 'none') return;
    const previous = { ...settings };
    await stopVoicePreview();
    const source = settings.sourceLanguage;
    settings.sourceLanguage = settings.targetLanguage;
    settings.targetLanguage = source;
    syncVoiceToTarget();
    settings = { ...settings };
    if (!running) {
      await persist();
      return;
    }

    directionSwitchPending = true;
    errorMessage = '';
    try {
      applyDirectionSwitchState(await swapTranslationDirection(settings));
    } catch (error) {
      settings = previous;
      directionSwitchPending = false;
      errorMessage = String(error);
    }
  }

  function applyDirectionSwitchState(state: DirectionSwitchState): void {
    directionSwitchPending = state.pending;
    if (!settings) return;
    settings.sourceLanguage = state.sourceLanguage as TranslationSettings['sourceLanguage'];
    settings.targetLanguage = state.targetLanguage as TranslationSettings['targetLanguage'];
    settings = { ...settings };
  }

  async function changeTargetLanguage(): Promise<void> {
    await stopVoicePreview();
    if (settings?.targetLanguage === 'none') {
      settings.translationOnly = false;
      settings.subtitleSplit = true;
    }
    syncVoiceToTarget();
    settings = { ...settings! };
    await persist();
  }

  async function adjustFont(direction: number): Promise<void> {
    if (!settings) return;
    const current = Math.max(0, fontSizes.indexOf(settings.fontSizePreset));
    const next = Math.min(fontSizes.length - 1, Math.max(0, current + direction));
    settings.fontSizePreset = fontSizes[next];
    settings = { ...settings };
    await persist();
  }

  async function toggleTranslation(): Promise<void> {
    if (!settings || busy) return;
    busy = true;
    errorMessage = '';
    try {
      const wasRunning = running;
      const state = wasRunning ? await stopTranslation() : await startTranslation(settings);
      applyRuntime(state);
      if (!wasRunning) subtitlePreviewVisible = false;
    } catch (error) {
      errorMessage = String(error);
    } finally {
      busy = false;
    }
  }

  function clearPreviewTimer(): void {
    if (previewTimer !== null) {
      window.clearTimeout(previewTimer);
      previewTimer = null;
    }
  }

  async function stopVoicePreview(): Promise<void> {
    clearPreviewTimer();
    previewingVoice = '';
    try {
      await stopSpokenVoicePreview();
    } catch (error) {
      errorMessage = String(error);
    }
  }

  async function setSpokenTranslation(enabled: boolean): Promise<void> {
    if (!settings) return;
    if (enabled && voicesForTarget(settings.targetLanguage).length === 0) {
      settings.spokenTranslationEnabled = false;
      settings = { ...settings };
      const required = requiredVoiceForTarget(settings.targetLanguage);
      errorMessage = required
        ? tr(`请先安装 ${required.name} 或任一兼容音色。`, `Install ${required.name} or another compatible voice first.`)
        : tr('当前目标语言暂不支持译文播报。', 'Spoken translation is not supported for this target language.');
      return;
    }
    if (!enabled) await stopVoicePreview();
    settings.spokenTranslationEnabled = enabled;
    settings = { ...settings };
    await persist();
  }

  async function selectSpokenVoice(): Promise<void> {
    await stopVoicePreview();
    await persist();
  }

  async function playVoicePreview(): Promise<void> {
    if (!settings?.spokenTranslationEnabled) return;
    const voice = settings.spokenTranslationVoice ?? 'apple-voice-1';
    if (previewingVoice === voice) {
      await stopVoicePreview();
      return;
    }

    errorMessage = '';
    clearPreviewTimer();
    try {
      await previewSpokenVoice(voice, settings.targetLanguage);
      previewingVoice = voice;
      previewTimer = window.setTimeout(() => {
        previewingVoice = '';
        previewTimer = null;
      }, 4600);
    } catch (error) {
      previewingVoice = '';
      errorMessage = String(error);
    }
  }

  async function toggleTestSubtitles(): Promise<void> {
    if (running || subtitlePreviewBusy) return;
    subtitlePreviewBusy = true;
    errorMessage = '';
    try {
      subtitlePreviewVisible = await toggleSubtitlePreview();
    } catch (error) {
      errorMessage = String(error);
    } finally {
      subtitlePreviewBusy = false;
    }
  }

  function applyRuntime(state: RuntimeState): void {
    running = state.running;
    runtimeStatus = state.status;
    runtimeMessage = state.message;
  }

  function formatDownloadSize(bytes: number): string {
    return `${(bytes / 1_000_000_000).toFixed(1)} GB`;
  }

  function formatDuration(seconds: number): string {
    const rounded = Math.max(0, Math.floor(seconds));
    const hours = Math.floor(rounded / 3600);
    const minutes = Math.floor((rounded % 3600) / 60);
    const remainder = rounded % 60;
    return hours > 0
      ? `${hours}:${String(minutes).padStart(2, '0')}:${String(remainder).padStart(2, '0')}`
      : `${minutes}:${String(remainder).padStart(2, '0')}`;
  }

  async function refreshUsage(): Promise<void> {
    try {
      usage = await getUsage();
    } catch (error) {
      errorMessage = String(error);
    }
  }

  async function refreshModelStatus(): Promise<void> {
    try {
      modelStatus = await getModelStatus();
      if (modelStatus.coreReady && modelTimer !== null) {
        window.clearInterval(modelTimer);
        modelTimer = null;
      }
    } catch (error) {
      errorMessage = String(error);
    }
  }

  async function downloadModels(component: 'core'): Promise<void> {
    errorMessage = '';
    try {
      modelStatus = await startModelDownload(component);
      if (modelTimer === null) {
        modelTimer = window.setInterval(() => void refreshModelStatus(), 750);
      }
    } catch (error) {
      errorMessage = String(error);
    }
  }

  function runtimeDisplayMessage(): string {
    if (browserPreview) return tr('界面预览 · 合成字幕', 'UI preview · synthetic captions');
    if (runtimeStatus === 'error') return runtimeMessage;
    if (directionSwitchPending && settings) {
      return tr(
        `将在下一句切换：${languageName(settings.sourceLanguage)} → ${languageName(settings.targetLanguage)}`,
        `Next sentence: ${languageName(settings.sourceLanguage)} → ${languageName(settings.targetLanguage)}`
      );
    }
    if (runtimeStatus === 'listening') return tr('实时翻译进行中', 'Live translation active');
    if (runtimeStatus === 'warming') return tr('正在启动本地翻译…', 'Starting local translation…');
    return tr('本地 AI 已就绪', 'Local AI is ready');
  }

  onMount(() => {
    let unlisten: () => void = () => undefined;
    let unlistenDirection: () => void = () => undefined;
    const refreshVoicesOnFocus = () => {
      if (payload && settings) void refreshAppleVoiceStatus();
    };
    const refreshVoicesWhenVisible = () => {
      if (document.visibilityState === 'visible') refreshVoicesOnFocus();
    };
    window.addEventListener('focus', refreshVoicesOnFocus);
    document.addEventListener('visibilitychange', refreshVoicesWhenVisible);
    void getSettings().then((data) => {
      payload = data;
      settings = { ...data.settings };
      applyAccentTheme(settings.accentTheme);
      subtitlePreviewVisible = data.subtitlePreviewVisible;
      if (syncVoiceToTarget()) {
        settings = { ...settings };
        void updateSettings(settings);
      }
      running = data.running;
      runtimeStatus = data.runtimeStatus;
      runtimeMessage = data.runtimeMessage;
    }).catch((error) => {
      errorMessage = String(error);
    });
    void listenRuntime(applyRuntime).then((cleanup) => { unlisten = cleanup; });
    void listenDirectionSwitchState(applyDirectionSwitchState).then((cleanup) => { unlistenDirection = cleanup; });
    void getDirectionSwitchState().then(applyDirectionSwitchState).catch((error) => { errorMessage = String(error); });
    void refreshModelStatus();
    void refreshUsage();
    usageTimer = window.setInterval(() => void refreshUsage(), 1000);
    return () => {
      clearPreviewTimer();
      if (modelTimer !== null) window.clearInterval(modelTimer);
      if (usageTimer !== null) window.clearInterval(usageTimer);
      stopVoiceAutoCheck();
      if (voiceGuideCloseTimer !== null) window.clearTimeout(voiceGuideCloseTimer);
      window.removeEventListener('focus', refreshVoicesOnFocus);
      document.removeEventListener('visibilitychange', refreshVoicesWhenVisible);
      void stopSpokenVoicePreview();
      unlisten();
      unlistenDirection();
    };
  });
</script>

<svelte:head>
  <title>{appDisplayName()}</title>
</svelte:head>

{#if settings && payload}
  <main class="app-shell">
    <header class="topbar">
      <div class="brand">
        <span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span>
        <div class="brand-copy">
          <h1>{productName}</h1>
          <p>{browserPreview ? tr('界面预览 · 合成数据 · 不采集音频', 'UI PREVIEW · SYNTHETIC DATA · NO AUDIO CAPTURE') : tr('开发版本 · 操作员私有字幕', 'DEVELOPMENT BUILD · PRIVATE OPERATOR CAPTIONS')} · {developmentVersion}</p>
        </div>
      </div>
      <div class="header-actions">
        <div class:active={runtimeStatus === 'listening'} class="status-line">
          <span></span>{tr(
            browserPreview ? '界面预览' : runtimeStatus === 'listening' ? '翻译中' : runtimeStatus === 'warming' ? '正在准备' : '本地 AI 就绪',
            browserPreview ? 'UI PREVIEW' : runtimeStatus === 'listening' ? 'TRANSLATING' : runtimeStatus === 'warming' ? 'WARMING UP' : 'LOCAL AI READY'
          )}
        </div>
        <button class="outline-button" on:click={() => openTranscriptHistory()}>{tr('转录记录', 'TRANSCRIPTS')}</button>
        <button
          class="outline-button square settings-button"
          aria-label={tr('设置', 'Settings')}
          title={tr('设置', 'Settings')}
          on:click={() => appSettingsOpen = true}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M4 7h3M11 7h9M4 17h9M17 17h3"></path>
            <circle cx="9" cy="7" r="2"></circle>
            <circle cx="15" cy="17" r="2"></circle>
          </svg>
        </button>
      </div>
    </header>

    <section class="control-grid">
      <div class="grid-row route-row">
        <div class="row-number">01</div>
        <div class="row-title">
          <strong>{tr('语言与音频', 'LANGUAGE + AUDIO')}</strong>
          <span>{tr('输入路线', 'INPUT ROUTE')}</span>
        </div>
        <div class="row-controls route-controls">
          <label class="route-field">
            <span class="route-heading"><strong>{tr('源语言', 'SOURCE LANGUAGE')}</strong></span>
            <select disabled={running} bind:value={settings.sourceLanguage} on:change={persist}>
              {#each languages as language}
                <option value={language.code}>{isEnglish() ? language.en : language.zh}</option>
              {/each}
            </select>
          </label>

          <button disabled={directionSwitchPending || settings.targetLanguage === 'none'} class="swap-button" aria-label={tr('交换语言', 'Swap languages')} on:click={swapLanguages}>⇄</button>

          <label class="route-field">
            <span class="route-heading"><strong>{tr('目标语言', 'TARGET LANGUAGE')}</strong></span>
            <select disabled={running} bind:value={settings.targetLanguage} on:change={changeTargetLanguage}>
              {#each languages as language}
                <option value={language.code}>{isEnglish() ? language.en : language.zh}</option>
              {/each}
              <option value="none">{tr('不翻译', 'No translation')}</option>
            </select>
          </label>

          <label class="route-field audio-route-field">
            <span class="route-heading"><strong>{tr('输入音频', 'AUDIO INPUT')}</strong></span>
            <select disabled={running} bind:value={settings.inputDevice} on:change={persist}>
              {#each payload.inputDevices as device}
                <option value={device}>{deviceName(device)}</option>
              {/each}
            </select>
          </label>
        </div>
      </div>

      <div class="grid-row subtitle-row">
        <div class="row-number">02</div>
        <div class="row-title">
          <strong>{tr('字幕窗口', 'SUBTITLE WINDOW')}</strong>
          <span>{tr('显示方式', 'DISPLAY')}</span>
        </div>
        <div class="row-controls subtitle-controls">
          <div class="control-block font-control">
            <span class="control-label">{tr('字体大小', 'TYPE SIZE')}</span>
            <div class="stepper">
              <button aria-label={tr('减小字体', 'Decrease type size')} on:click={() => adjustFont(-1)}>−</button>
              <output>{settings.fontSizePreset} PT</output>
              <button aria-label={tr('增大字体', 'Increase type size')} on:click={() => adjustFont(1)}>+</button>
            </div>
          </div>

          <div class="control-block layout-control">
            <span class="control-label">{tr('内容样式', 'LAYOUT')}</span>
            <div
              class="segmented two"
              title={settings.targetLanguage === 'none' ? tr('选择“不翻译”时无法调整此选项。', 'This option cannot be changed when No translation is selected.') : undefined}
            >
              <button disabled={settings.targetLanguage === 'none'} class:active={!settings.subtitleSplit && settings.targetLanguage !== 'none'} on:click={async () => { settings!.subtitleSplit = false; settings = { ...settings! }; await persist(); }}>{tr('上下对照', 'STACKED')}</button>
              <button disabled={settings.targetLanguage === 'none'} class:active={settings.subtitleSplit && settings.targetLanguage !== 'none'} on:click={async () => { settings!.subtitleSplit = true; settings = { ...settings! }; await persist(); }}>{tr('逐句双行', 'SENTENCE PAIRS')}</button>
            </div>
          </div>

          <div class="control-block subtitle-action-control">
            <span class="control-label">{tr('字幕调整', 'SUBTITLE TOOLS')}</span>
            <div class="subtitle-action-pair">
              <button class:active={subtitlePreviewVisible && !running} disabled={running || subtitlePreviewBusy} class="subtitle-preview-button" on:click={toggleTestSubtitles}>
                {running ? tr('实时字幕中', 'LIVE') : subtitlePreviewVisible ? tr('清空字幕', 'CLEAR') : tr('测试字幕', 'TEST')}
              </button>
              <button class="wide-outline" on:click={() => advancedOpen = true}>{tr('高级设置', 'ADVANCED')} <span>↗</span></button>
            </div>
          </div>
        </div>
      </div>

      <div class="grid-row voice-row">
        <div class="row-number">03</div>
        <div class="row-title">
          <strong>{tr('译文播报', 'SPOKEN TRANSLATION')}</strong>
          <span>{tr('音色与输出设备', 'VOICE + OUTPUT')}</span>
        </div>
        <div class="row-controls voice-controls">
          <div class="control-block voice-toggle-control">
            <span class="control-label">{tr('播报开关', 'SPEECH OUTPUT')}</span>
            <div class="segmented two">
              <button class:active={!settings.spokenTranslationEnabled} on:click={() => setSpokenTranslation(false)}>{tr('关', 'OFF')}</button>
              <button disabled={requiredVoiceForTarget(settings.targetLanguage) === null} class:active={settings.spokenTranslationEnabled} on:click={() => setSpokenTranslation(true)}>{tr('开', 'ON')}</button>
            </div>
          </div>

          <div class:is-disabled={!settings.spokenTranslationEnabled} class="control-block voice-picker-control">
            <span class="control-label">{tr('播报音色', 'VOICE')} · {languageName(settings.targetLanguage)}</span>
            <div class="voice-picker-row">
              <select disabled={!settings.spokenTranslationEnabled} bind:value={settings.spokenTranslationVoice} on:change={selectSpokenVoice}>
                {#if voicesForTarget(settings.targetLanguage).length === 0}
                  <option value="">{tr('暂无已选音色', 'NO APPROVED VOICE')}</option>
                {:else}
                  {#each voicesForTarget(settings.targetLanguage) as voice}
                    <option value={voice.id}>{voice.name}</option>
                  {/each}
                {/if}
              </select>
              <button aria-label={previewingVoice === (settings.spokenTranslationVoice ?? 'apple-voice-1') ? tr('停止试听', 'Stop preview') : tr('试听音色', 'Preview voice')} title={previewingVoice === (settings.spokenTranslationVoice ?? 'apple-voice-1') ? tr('停止试听', 'Stop preview') : tr('试听音色', 'Preview voice')} class:playing={previewingVoice === (settings.spokenTranslationVoice ?? 'apple-voice-1')} class="preview-button" type="button" disabled={!settings.spokenTranslationEnabled || voicesForTarget(settings.targetLanguage).length === 0} on:click={playVoicePreview}>
                <span aria-hidden="true">{previewingVoice === (settings.spokenTranslationVoice ?? 'apple-voice-1') ? '■' : '▶'}</span>
              </button>
            </div>
          </div>

          <label class:is-disabled={!settings.spokenTranslationEnabled} class="control-block output-device-control">
            <span class="control-label">{tr('输出设备', 'OUTPUT DEVICE')}</span>
            <select disabled={!settings.spokenTranslationEnabled} bind:value={settings.spokenTranslationOutputDevice} on:mouseenter={refreshOutputDevices} on:focus={refreshOutputDevices} on:change={persist}>
              <option value={null}>{tr('系统默认', 'SYSTEM DEFAULT')}</option>
              {#each payload.outputDevices as device}<option value={device}>{device}</option>{/each}
            </select>
          </label>
        </div>
      </div>
    </section>

    <footer class="launch-area">
      <div class="launch-meta">
        <span>{languageName(settings.sourceLanguage)} → {languageName(settings.targetLanguage)}</span>
        <span>{deviceName(settings.inputDevice)}</span>
        {#if errorMessage}<strong class="error">{errorMessage}</strong>{:else}<span>{runtimeDisplayMessage()}</span>{/if}
        {#if usage && running}<strong class="session-timer">{formatDuration(usage.currentSessionSeconds)}</strong>{/if}
      </div>
      <button class:running class="launch-button" disabled={busy || browserPreview} on:click={toggleTranslation}>
        <span>{running ? '■' : '▶'}</span>
        {browserPreview ? tr('界面预览 · 请在桌面应用中启动', 'UI PREVIEW · START IN DESKTOP APP') : busy ? tr('请稍候…', 'PLEASE WAIT…') : running ? tr('停止实时翻译', 'STOP LIVE TRANSLATION') : tr('启动实时翻译', 'START LIVE TRANSLATION')}
      </button>
    </footer>
  </main>

  {#if advancedOpen}
    <div class="modal-backdrop">
      <dialog open class="modal" aria-label={tr('高级字幕设置', 'Advanced subtitle settings')}>
        <header><div><span>02.A</span><h2>{tr('高级字幕设置', 'ADVANCED SUBTITLES')}</h2></div><button on:click={() => advancedOpen = false}>×</button></header>
        <div class="modal-field choice-field">
          <span>{tr('字幕显示内容', 'SUBTITLE CONTENT')}</span>
          <div
            class="segmented two modal-choice"
            title={settings.targetLanguage === 'none' ? tr('选择“不翻译”时无法调整此选项。', 'This option cannot be changed when No translation is selected.') : undefined}
          >
            <button disabled={settings.targetLanguage === 'none'} class:active={!settings.translationOnly && settings.targetLanguage !== 'none'} on:click={async () => { settings!.translationOnly = false; settings = { ...settings! }; await persist(); }}>{tr('双语', 'DUAL')}</button>
            <button disabled={settings.targetLanguage === 'none'} class:active={settings.translationOnly && settings.targetLanguage !== 'none'} on:click={async () => { settings!.translationOnly = true; settings = { ...settings! }; await persist(); }}>{tr('仅译文', 'TRANSLATION')}</button>
          </div>
        </div>
        <label class="modal-field"><span>{tr('窗口不透明度', 'WINDOW OPACITY')}</span><input type="range" min="0.35" max="1" step="0.05" bind:value={settings.overlayOpacity} on:change={persist} /><output>{Math.round(settings.overlayOpacity * 100)}%</output></label>
        <label class="modal-field"><span>{tr('字幕垂直位置', 'VERTICAL ANCHOR')}</span><select bind:value={settings.anchorPositionPreset} on:change={persist}><option value="35">35%</option><option value="50">50%</option><option value="70">70%</option><option value="100">100%</option></select></label>
        <label class="modal-field" title={tr('连续说话时，最多每隔所选时间生成一次可翻译的稳定字幕。时间越短，字幕更新越快，但语音片段也会更碎。', 'During continuous speech, produce a stable translatable caption at least this often. Shorter intervals update faster but split speech into smaller segments.')}>
          <span>{tr('最长字幕间隔', 'MAX CAPTION INTERVAL')}</span>
          <input type="range" min="3" max="8" step="1" disabled={running} bind:value={settings.finalIntervalSeconds} on:change={persist} />
          <output>{settings.finalIntervalSeconds}{tr(' 秒', 's')}</output>
        </label>
        <div
          class="modal-field choice-field"
          title={tr('实时翻译期间阻止因空闲触发的屏保、显示器休眠和系统休眠；停止翻译后恢复系统原有设置。', 'Prevents idle screen saver, display sleep, and system sleep while live translation is running. System behavior is restored when translation stops.')}
        >
          <span>{tr('实时翻译时保持屏幕唤醒', 'KEEP SCREEN AWAKE WHILE LIVE')}</span>
          <div class="segmented two modal-choice">
            <button class:active={!settings.keepAwakeDuringTranslation} on:click={async () => { settings!.keepAwakeDuringTranslation = false; settings = { ...settings! }; await persist(); }}>{tr('关', 'OFF')}</button>
            <button class:active={settings.keepAwakeDuringTranslation} on:click={async () => { settings!.keepAwakeDuringTranslation = true; settings = { ...settings! }; await persist(); }}>{tr('开', 'ON')}</button>
          </div>
        </div>
        <div class="modal-field choice-field">
          <span>{tr('自动保存转录', 'AUTO-SAVE TRANSCRIPT')}</span>
          <div class="segmented two modal-choice">
            <button class:active={!settings.autoSaveTranscript} on:click={async () => { settings!.autoSaveTranscript = false; settings = { ...settings! }; await persist(); }}>{tr('关', 'OFF')}</button>
            <button class:active={settings.autoSaveTranscript} on:click={async () => { settings!.autoSaveTranscript = true; settings = { ...settings! }; await persist(); }}>{tr('开', 'ON')}</button>
          </div>
        </div>
      </dialog>
    </div>
  {/if}

  {#if appSettingsOpen}
    <div class="modal-backdrop">
      <dialog open class="modal small-modal" aria-label={tr('设置', 'Settings')}>
        <header><div><span>SYS</span><h2>{tr('应用设置', 'APPLICATION')}</h2></div><button on:click={() => appSettingsOpen = false}>×</button></header>
        <div class="modal-field"><span>{tr('界面语言', 'INTERFACE LANGUAGE')}</span><div class="segmented two language-toggle"><button class:active={settings.appLanguage === 'zh'} on:click={async () => { settings!.appLanguage = 'zh'; settings = { ...settings! }; await persist(); }}>中文</button><button class:active={settings.appLanguage === 'en'} on:click={async () => { settings!.appLanguage = 'en'; settings = { ...settings! }; await persist(); }}>EN</button></div></div>
        <div class="modal-field theme-field">
          <span>{tr('主题颜色', 'ACCENT THEME')}</span>
          <div class="theme-options">
            {#each accentThemes as theme}
              <button
                class:active={settings.accentTheme === theme.id}
                aria-label={isEnglish() ? theme.en : theme.zh}
                title={isEnglish() ? theme.en : theme.zh}
                style={`--theme-swatch:${theme.color}`}
                on:click={() => selectAccentTheme(theme.id)}
              >
                <span class="theme-swatch"><span class="theme-mark" style={`--brand-mark:url("${logoUrl}")`}></span></span>
                <small>{isEnglish() ? theme.en : theme.zh}</small>
              </button>
            {/each}
          </div>
        </div>
        {#if modelStatus}
          <section class="model-settings">
            <div class="model-row">
              <div><strong>{tr('核心翻译模型', 'CORE TRANSLATION MODELS')}</strong><small>{formatDownloadSize(modelStatus.coreDownloadBytes)}</small></div>
              <button disabled={modelStatus.coreReady || modelStatus.downloading} on:click={() => downloadModels('core')}>{modelStatus.coreReady ? tr('已安装', 'INSTALLED') : tr('下载', 'DOWNLOAD')}</button>
            </div>
            <section class="voice-requirements">
              <header>
                <div>
                  <strong>{tr('推荐 Apple 音色', 'RECOMMENDED APPLE VOICES')}</strong>
                  <small>{tr('当前译文语言只需一个可用音色；下列为推荐音色，也支持已安装的兼容候选。', 'THE CURRENT TARGET NEEDS ONE AVAILABLE VOICE · RECOMMENDED VOICES ARE SHOWN BELOW, AND INSTALLED COMPATIBLE VOICES ALSO WORK.')}</small>
                </div>
                <button disabled={refreshingAppleVoices} on:click={refreshAppleVoiceStatus}>{refreshingAppleVoices ? tr('检测中…', 'CHECKING…') : tr('重新检测', 'RECHECK')}</button>
              </header>
              <div class="voice-requirement-list">
                {#each requiredAppleVoices as voice}
                  <article
                    class:current={settings.targetLanguage === voice.target}
                    class:installed={isAppleVoiceInstalled(voice.name, voice.locale)}
                    class="voice-requirement-card"
                  >
                    <div class="voice-card-copy">
                      <span>{settings.targetLanguage === voice.target ? tr('当前译文语言', 'CURRENT TARGET') : tr('切换语言时推荐', 'RECOMMENDED WHEN SWITCHING')}</span>
                      <strong>{voice.name}</strong>
                      <small>{voiceLanguageName(voice)} · {voice.locale}</small>
                    </div>
                    <div class="voice-card-status">
                      <span class:warning={hasWrongLocaleVariant(voice)} class:ready={isAppleVoiceInstalled(voice.name, voice.locale)}>
                        {isAppleVoiceInstalled(voice.name, voice.locale) ? '✓ ' : ''}{voiceStatusLabel(voice)}
                      </span>
                      {#if !isAppleVoiceInstalled(voice.name, voice.locale)}
                        <button on:click={() => openAppleVoiceDownloads(voice)}>{tr('前往系统设置安装', 'OPEN SYSTEM SETTINGS')}</button>
                      {:else}
                        <button disabled>{tr('可以使用', 'READY')}</button>
                      {/if}
                    </div>
                  </article>
                {/each}
              </div>
            </section>
            {#if modelStatus.downloading}
              <div class="model-progress"><span style={`width:${Math.round(modelStatus.progress * 100)}%`}></span></div>
              <p class="model-detail">{modelStatus.title} · {modelStatus.detail} · {Math.round(modelStatus.progress * 100)}%</p>
            {/if}
          </section>
        {/if}
        {#if usage}
          <section class="usage-settings">
            <div class="usage-heading">
              <div><strong>{tr('本机使用统计', 'LOCAL USAGE')}</strong><small>{tr('只保存在这台电脑', 'STORED ONLY ON THIS MAC')}</small></div>
              <b>{formatDuration(usage.lifetimeSeconds)}</b>
            </div>
            <div class="usage-grid">
              <div><small>{tr('本月', 'THIS MONTH')}</small><strong>{formatDuration(usage.monthlySeconds)}</strong></div>
              <div><small>{tr('完成会话', 'SESSIONS')}</small><strong>{usage.completedSessions}</strong></div>
              <div><small>{tr('当前会话', 'CURRENT SESSION')}</small><strong>{formatDuration(usage.currentSessionSeconds)}</strong></div>
            </div>
          </section>
        {/if}
        <p class="privacy-note">{tr('语音、字幕和偏好设置均保留在本机。', 'Audio, subtitles, and preferences remain on this device.')}</p>
      </dialog>
    </div>
  {/if}
  {#if voiceInstallGuide}
    <div class="modal-backdrop voice-guide-backdrop">
      <dialog open class="modal voice-guide-modal" aria-label={tr('安装 Apple 音色', 'Install Apple voice')}>
        <header>
          <div><span>VOICE</span><h2>{tr('安装推荐音色', 'INSTALL RECOMMENDED VOICE')}</h2></div>
          <button on:click={closeVoiceInstallGuide}>×</button>
        </header>
        <div class="voice-guide-content">
          <div class="voice-guide-target">
            <div><small>{tr('需要安装', 'VOICE TO INSTALL')}</small><strong>{voiceInstallGuide.name}</strong></div>
            <span>{voiceLanguageName(voiceInstallGuide)} · {voiceInstallGuide.locale}</span>
          </div>
          {#if isAppleVoiceInstalled(voiceInstallGuide.name, voiceInstallGuide.locale)}
            <div class="voice-guide-success"><strong>✓ {tr('检测到正确音色', 'CORRECT VOICE DETECTED')}</strong><small>{tr('安装向导将自动关闭。', 'THIS GUIDE WILL CLOSE AUTOMATICALLY.')}</small></div>
          {:else}
            <ol>
              <li>{tr('在系统设置中进入“辅助功能 → 实时语音”。', 'In System Settings, open Accessibility → Live Speech.')}</li>
              <li>
                {voiceInstallGuide.target === 'zh'
                  ? tr('将系统语音语言选择为“普通话”，点击 Voice 右侧的 ⓘ 打开音色列表。', 'Set System Speech Language to Mandarin, then click the ⓘ beside Voice.')
                  : tr('将系统语音语言选择为“英语”，点击 Voice 右侧的 ⓘ 打开音色列表。', 'Set System Speech Language to English, then click the ⓘ beside Voice.')}
              </li>
              <li>
                {voiceInstallGuide.target === 'zh'
                  ? tr('搜索 Yue，并下载 Yue (Premium)。', 'Search for Yue and download Yue (Premium).')
                  : tr('搜索 Voice 4，并下载英语（美国）版本；不要选择其他国家或语言的 Voice 4。', 'Search for Voice 4 and download the English (United States) version, not another locale.')}
              </li>
            </ol>
            <div class="voice-guide-visual-sequence" aria-hidden="true">
              <section class="voice-guide-visual-card">
                <div class="voice-guide-visual-heading">
                  <span>02</span>
                  <strong>{tr('点击信息按钮', 'CLICK THE INFO BUTTON')}</strong>
                </div>
                <div class="voice-guide-system-preview">
                  <div class="voice-guide-window-dots"><i></i><i></i><i></i></div>
                  <div class="voice-guide-preview-row">
                    <span>System speech language</span>
                    <strong>{voiceInstallGuide.target === 'zh' ? tr('普通话', 'Mandarin') : tr('英语', 'English')}</strong>
                  </div>
                  <div class="voice-guide-preview-row voice-guide-info-row">
                    <span>Voice</span>
                    <b class="voice-guide-info-icon">i</b>
                    <svg class="voice-guide-pointer" viewBox="0 0 76 38">
                      <path d="M4 31 C 25 31, 38 27, 58 14"></path>
                      <path d="M51 12 L 64 10 L 59 22"></path>
                    </svg>
                  </div>
                </div>
              </section>
              <section class="voice-guide-visual-card">
                <div class="voice-guide-visual-heading">
                  <span>03</span>
                  <strong>{tr('搜索并选对地区', 'SEARCH THE EXACT VOICE')}</strong>
                </div>
                <div class="voice-guide-list-preview">
                  <div class="voice-guide-search-preview">
                    <svg viewBox="0 0 20 20">
                      <circle cx="8.5" cy="8.5" r="5.5"></circle>
                      <path d="M12.5 12.5 L17 17"></path>
                    </svg>
                    <strong>{voiceInstallGuide.target === 'zh' ? 'Yue' : 'Voice 4'}</strong>
                  </div>
                  <div class="voice-guide-result-preview">
                    <div>
                      <small>{voiceLanguageName(voiceInstallGuide)}</small>
                      <strong>{voiceInstallGuide.name}</strong>
                    </div>
                    <span>↓</span>
                  </div>
                </div>
              </section>
            </div>
            {#if hasWrongLocaleVariant(voiceInstallGuide)}
              <p class="voice-guide-warning">{tr(`检测到其他语言版本的 ${voiceInstallGuide.name}，仍需安装 ${voiceInstallGuide.locale} 版本。`, `Another ${voiceInstallGuide.name} locale is installed. You still need the ${voiceInstallGuide.locale} version.`)}</p>
            {/if}
            <p class="voice-guide-waiting">{refreshingAppleVoices ? tr('正在自动检测安装状态…', 'CHECKING INSTALLATION…') : tr('安装后返回这里，App 会自动完成检测。', 'RETURN HERE AFTER INSTALLING; THE APP WILL DETECT IT AUTOMATICALLY.')}</p>
          {/if}
        </div>
        <footer class="voice-guide-actions">
          {#if isAppleVoiceInstalled(voiceInstallGuide.name, voiceInstallGuide.locale)}
            <button class="primary" on:click={closeVoiceInstallGuide}>{tr('完成', 'DONE')}</button>
          {:else}
            <button on:click={reopenAppleVoiceDownloads}>{tr('再次打开系统设置', 'OPEN SYSTEM SETTINGS AGAIN')}</button>
            <button class="primary" disabled={refreshingAppleVoices} on:click={refreshAppleVoiceStatus}>{refreshingAppleVoices ? tr('检测中…', 'CHECKING…') : tr('立即检测', 'CHECK NOW')}</button>
          {/if}
        </footer>
      </dialog>
    </div>
  {/if}
  {#if modelStatus && !modelStatus.coreReady}
    <div class="modal-backdrop model-setup-backdrop">
      <section class="model-setup" aria-label={tr('下载本地模型', 'Download local models')}>
        <span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span>
        <div>
          <p class="setup-kicker">{tr('首次使用设置', 'FIRST-TIME SETUP')}</p>
          <h2>{tr('下载本地翻译模型', 'DOWNLOAD LOCAL TRANSLATION MODELS')}</h2>
          <p>{tr('模型约 4.2 GB，只需下载一次。语音和字幕始终在这台电脑上处理。', 'The models are about 4.2 GB and download once. Audio and subtitles stay on this Mac.')}</p>
        </div>
        {#if modelStatus.downloading && modelStatus.component === 'core'}
          <div class="setup-progress">
            <div><span style={`width:${Math.round(modelStatus.progress * 100)}%`}></span></div>
            <strong>{Math.round(modelStatus.progress * 100)}%</strong>
            <small>{modelStatus.title} · {modelStatus.detail}</small>
          </div>
        {:else}
          <button class="launch-button" on:click={() => downloadModels('core')}>{tr('下载并继续', 'DOWNLOAD AND CONTINUE')}</button>
        {/if}
        {#if errorMessage}<p class="setup-error">{errorMessage}</p>{/if}
      </section>
    </div>
  {/if}
{:else}
  <main class="loading-screen"><span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span><p>LOADING AI VISION FORUM</p></main>
{/if}
