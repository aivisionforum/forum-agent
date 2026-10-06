<script lang="ts">
  import { translate } from './lib/i18n/locale';
  import { t, uiLocale, pair, setLocale, initializeLocale, type Locale } from './lib/i18n';
  import { onMount, tick } from 'svelte';
  import LanguageSwitch from './components/LanguageSwitch.svelte';
  import StageControls from './components/StageControls.svelte';
  import ApplicationSettings from './components/ApplicationSettings.svelte';
  import InsightMode from './components/InsightMode.svelte';
  import InputLevel from './components/InputLevel.svelte';
  import './console.css';
  import LiveInsights from './components/LiveInsights.svelte';
  import './live-insights.css';
  import MeetingAssistantSettings from './components/MeetingAssistantSettings.svelte';
  import ForumWorkspace from './ForumWorkspace.svelte';
  import { productName, brandLogoUrl as logoUrl } from './lib/product';
  import { meetingMode, settingsForMeetingMode, type MeetingMode } from './lib/meeting-mode';
  import { needsModelDownload, runtimeIssue } from './lib/model-readiness';
  import {
    getSettings,
    listenWorkspace,
    listenLiveSettings,
    isTauri,
    getModelStatus,
    getUsage,
    listenRuntime,
    listenInputDevices,
    startTranslation,
    startModelDownload,
    stopTranslation,
    toggleSubtitlePreview,
    updateSettings,
    type AccentTheme,
    type RuntimeState,
    type ModelStatus,
    type UsageSnapshot,
    type SettingsPayload,
    type TranslationSettings
  } from './lib/api';

  const languages = [
    { code: 'zh', zh: '中文', en: 'Chinese' },
    { code: 'en', zh: '英语', en: 'English' }
  ] as const;
  let payload: SettingsPayload | null = null;
  let settings: TranslationSettings | null = null;
  let running = false;
  let liveSession: string | null = null;
  let nextInsightMode: 'gated' | 'automatic' = 'gated';
  let runtimeStatus = 'idle';
  let runtimeMessage = 'Local AI is ready';
  let settingsSection = 'general';
  function openSettings(section = 'general') { settingsSection = section; appSettingsOpen = true; }
  let workspaceSession: string | null = new URLSearchParams(window.location.search).get('session') || null;
  let workspaceRequest = new URLSearchParams(window.location.search).get('view') === 'forum' ? 1 : 0;
  let workspaceVisible = new URLSearchParams(window.location.search).get('view') === 'forum';
  let appSettingsOpen = false;
  let subtitlePreviewVisible = true;
  let subtitlePreviewBusy = false;
  let busy = false;
  let errorMessage = '';
  let modelStatus: ModelStatus | null = null;
  let modelTimer: number | null = null;
  let usage: UsageSnapshot | null = null;
  let usageTimer: number | null = null;

  const previewLocale = initializeLocale();
  $: tr = $pair;
  let languageSaving = false;
  $: setupIssue = settings ? runtimeIssue(modelStatus, settings) : '';
  async function selectUiLanguage(locale: Locale) {
    if (!settings || languageSaving || locale === settings.appLanguage) return;
    const previous = settings.appLanguage;
    languageSaving = true;
    settings = {...settings, appLanguage: locale};
    setLocale(locale);
    try { await persist(true); }
    catch { settings = {...settings, appLanguage: previous}; setLocale(previous); }
    finally { languageSaving = false; }
  }
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

  function languageName(code: string, locale: Locale): string {
    if (code === 'none') return translate(locale, '不翻译');
    if (code === 'auto') return translate(locale, '中英自动识别');
    if (code === 'bilingual') return translate(locale, '中文和英文');
    const language = languages.find((item) => item.code === code);
    return language ? (locale === 'en' ? language.en : language.zh) : code.toUpperCase();
  }

  function deviceName(value: string, locale: Locale): string {
    if (value === '__dual_audio__') return translate(locale, '麦克风 + 系统音频');
    if (value === '__system_audio__') return translate(locale, '系统音频');
    if (value === '__default_microphone__') return translate(locale, '默认麦克风');
    return value;
  }

  async function chooseMeetingMode(mode: MeetingMode) {
    if (!settings || running) return;
    settings = settingsForMeetingMode(settings, mode);
    await persist();
  }
  async function focusAssistantSettings() {
    workspaceVisible = false;
    await tick();
    const section = document.getElementById('meeting-assistant-settings') as HTMLDetailsElement | null;
    if (section) { section.open = true; section.scrollIntoView({behavior:'smooth',block:'center'}); }
  }

  async function persist(rethrow: unknown = false): Promise<void> {
    if (settings && !settings.recordingEnabled) settings.speakersEnabled = false;
    if (!settings) return;
    errorMessage = '';
    try {
      await updateSettings(settings);
    } catch (error) {
      errorMessage = String(error);
      if (rethrow === true) throw error;
    }
  }

  async function toggleTranslation(): Promise<void> {
    if (!settings || busy) return;
    busy = true;
    errorMessage = '';
    try {
      const wasRunning = running;
      if (!wasRunning) settings = settingsForMeetingMode(settings, meetingMode(settings));
      const state = wasRunning ? await stopTranslation() : await startTranslation(settings, nextInsightMode);
      applyRuntime(state);
      if (!wasRunning) subtitlePreviewVisible = false;
    } catch (error) {
      errorMessage = String(error);
    } finally {
      busy = false;
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
      if (!modelStatus.downloading && modelTimer !== null) {
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

  function runtimeDisplayMessage(locale: Locale, status: string, message: string, target?: string): string {
    if (browserPreview) return translate(locale, '界面预览 · 合成字幕');
    if (['error', 'degraded', 'draining', 'stopping'].includes(status)) return message;
    if (status === 'listening') return translate(locale, target === 'none' ? '正在转写 · 仅原文' : '实时双语进行中');
    if (status === 'warming') return translate(locale, '正在准备会议…');
    return !message || message === 'Local AI is ready' ? translate(locale, '本地 AI 已就绪') : message;
  }

  onMount(() => {
    let unlisten: () => void = () => undefined;
    let unlistenInputs = () => {};
    let discoveredInputs: string[] | null = null;
    let workspaceDisposed = false;
    void listenInputDevices(devices => {
      discoveredInputs = devices;
      if (payload) payload = {...payload, inputDevices: devices};
    }).then(cleanup => {
      if (workspaceDisposed) cleanup(); else unlistenInputs = cleanup;
      return getSettings();
    }).then((data) => {
      if (workspaceDisposed) return;
      payload = {...data, inputDevices: discoveredInputs ?? data.inputDevices};
      const normalized = settingsForMeetingMode(data.settings, meetingMode(data.settings));
      settings = data.running ? {...data.settings} : normalized;
      if (!data.running && JSON.stringify(settings) !== JSON.stringify(data.settings)) {
        void updateSettings(settings).catch(e => errorMessage = String(e));
      }
      if (browserPreview) settings = {...settings, appLanguage: previewLocale};
      setLocale(settings.appLanguage);
      applyAccentTheme(settings.accentTheme);
      subtitlePreviewVisible = data.subtitlePreviewVisible;
      running = data.running || (browserPreview && new URLSearchParams(window.location.search).get('preview') === 'live');
      runtimeStatus = data.runtimeStatus;
      runtimeMessage = data.runtimeMessage;
    }).catch((error) => {
      errorMessage = String(error);
    });
    let unlistenWorkspace = () => {};
    let unlistenSettings = () => {};
    void listenLiveSettings(() => void focusAssistantSettings()).then(cleanup => { if (workspaceDisposed) cleanup(); else unlistenSettings = cleanup; });
    if (new URLSearchParams(window.location.search).get('view') === 'settings') void focusAssistantSettings();
    void listenWorkspace(id => { workspaceSession = id; workspaceRequest++; workspaceVisible = true; })
      .then(cleanup => { if (workspaceDisposed) cleanup(); else unlistenWorkspace = cleanup; });
    void listenRuntime(applyRuntime).then((cleanup) => { unlisten = cleanup; });
    void refreshModelStatus();
    void refreshUsage();
    usageTimer = window.setInterval(() => void refreshUsage(), 1000);
    return () => {
      workspaceDisposed = true;
      unlistenWorkspace();
      unlistenSettings();
      unlistenInputs();
      if (modelTimer !== null) window.clearInterval(modelTimer);
      if (usageTimer !== null) window.clearInterval(usageTimer);
      unlisten();
    };
  });
</script>

<svelte:head>
  <title>{appDisplayName()}</title>
</svelte:head>

{#if settings && payload}
  <main class="app-shell forum-console" inert={appSettingsOpen}>
    <header class="topbar">
      <div class="brand"><span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span><div class="brand-copy"><h1>{productName}</h1><p>{browserPreview ? tr('界面预览 · 合成数据 · 不采集音频', 'UI preview · Synthetic data · No audio capture') : tr('操作员控制台 · 私有', 'Operator console · Private')}</p></div></div>
      <div class="header-actions"><LanguageSwitch value={$uiLocale} onchange={selectUiLanguage} disabled={languageSaving} /><button class="app-settings-trigger" on:click={() => openSettings()}><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h3M11 7h9M4 17h9M17 17h3"/><circle cx="9" cy="7" r="2"/><circle cx="15" cy="17" r="2"/></svg>{tr('应用设置', 'App settings')}</button></div>
    </header>
    <nav class="forum-navigation" aria-label={$t("应用导航")}><button class:active={!workspaceVisible} on:click={() => workspaceVisible = false}>{running ? tr('本场会议', 'Current meeting') : tr('会前准备', 'Meeting setup')}</button><button class:active={workspaceVisible} on:click={() => workspaceVisible = true}>{tr('会议工作台', 'Meeting workspace')}</button><span class="nav-note">LOCAL FIRST</span></nav>
    <div class="console-content">
      <div class="control-view" class:is-live={running} hidden={workspaceVisible}>
        <div class="meeting-heading"><div><h2>{running ? tr('本场会议', 'Current meeting') : tr('准备开始会议', 'Prepare your meeting')}</h2><p>{running ? tr('在这里将洞察一键上墙；本窗口留在操作员电脑上。', 'Publish insights here. Keep this private console on your Mac.') : tr('确认本场的语言、输入和洞察发布方式。', 'Choose the language, audio input and insight approval mode for this meeting.')}</p></div>{#if running}<span class="live-badge">● {tr('会议进行中', 'Meeting in progress')}</span>{/if}</div>
        {#if !running}
          <div class="meeting-setup-grid">
            <section class="setup-card"><h3>{tr('语言与音频', 'Language & audio')}</h3>
              <div class="setup-field"><span>{tr('会议语言', 'Meeting language')}</span><div class="segmented three" role="group" aria-label={$t("会议语言模式")}>{#each [['mixed','中英混合','Chinese + English'],['zh','纯中文','Chinese only'],['en','纯英文','English only']] as mode}<button class:active={meetingMode(settings) === mode[0]} aria-pressed={meetingMode(settings) === mode[0]} disabled={busy} on:click={() => chooseMeetingMode(mode[0] as MeetingMode)}>{tr(mode[1],mode[2])}</button>{/each}</div></div>
              <label class="setup-field"><span>{tr('输入音频', 'Audio input')}</span><select disabled={busy} bind:value={settings.inputDevice} on:change={persist}>{#each [...new Set(['__dual_audio__',...payload.inputDevices,settings.inputDevice].filter(Boolean))] as device}<option value={device}>{deviceName(device, $uiLocale)}</option>{/each}</select></label>
              <label class="record-audio"><input type="checkbox" bind:checked={settings.recordingEnabled} disabled={busy} on:change={persist} /><span>{tr('保存本场录音', 'Save audio for this meeting')}<small>{settings.recordingEnabled ? tr('支持断点恢复与匿名说话人分析', 'Enables recovery and anonymous speaker analysis') : tr('只保存文字；未识别的音频无法恢复', 'Text only; unrecognized audio cannot be recovered')}</small></span></label>
              <details class="meeting-options"><summary>{tr('输入增益', 'Input gain')}</summary><InputLevel bind:settings save={() => persist(true)} {running}/></details>
              <details class="meeting-options" id="meeting-assistant-settings"><summary>{tr('匿名说话人', 'Anonymous speakers')} · {settings.speakersEnabled ? tr('开', 'On') : tr('关', 'Off')}</summary><MeetingAssistantSettings bind:settings save={() => persist(true)} {running} active={!workspaceVisible}/></details>
            </section>
            <section class="setup-card mode-card"><h3>{tr('洞察发布', 'Insight approval')}</h3><InsightMode bind:mode={nextInsightMode} locked={busy}/><p class="scope-note">{tr('本场模式在开始会议时保存；下一场可重新选择。', 'Saved for this session when it starts. Choose again for your next meeting.')}</p></section>
          </div>
        {/if}
        <aside class="meeting-sidebar" aria-label={tr('本场控制', 'Session controls')}>
          <div class="audience-screens">{#if running}<h3>{tr('会场屏幕', 'Audience screens')}</h3>{/if}<StageControls {running} starting={busy || runtimeStatus === 'warming'} bind:sessionId={liveSession}/></div>
          {#if running}
            <div class="session-approval">{#key liveSession}<InsightMode sessionId={liveSession} locked={busy || !liveSession} compact />{/key}</div>
            <section class="session-controls"><h3>{tr('语言与音频', 'Language & audio')}</h3><div class="session-summary"><span>{settings.targetLanguage === 'none' ? tr('仅原文', 'Transcript only') : tr('中英双语', 'Chinese + English')}</span><span>{deviceName(settings.inputDevice, $uiLocale)}</span><span>{settings.recordingEnabled ? tr('保存录音', 'Audio saved') : tr('仅文字记录', 'Text records only')}</span></div><details class="meeting-options"><summary>{tr('调整音频与字幕', 'Adjust audio & captions')}</summary><InputLevel bind:settings save={() => persist(true)} {running}/><button class="text-link" on:click={() => openSettings('subtitles')}>{tr('字幕外观', 'Caption appearance')} ↗</button></details><details class="meeting-options" id="meeting-assistant-settings"><summary>{tr('匿名说话人', 'Anonymous speakers')}</summary><MeetingAssistantSettings bind:settings save={() => persist(true)} {running} active={!workspaceVisible}/></details></section>
          {/if}
        </aside>
        {#if !running}<div class="screen-tools"><button on:click={() => openSettings('subtitles')}>{tr('字幕外观', 'Caption appearance')}</button><button disabled={subtitlePreviewBusy || browserPreview} on:click={toggleTestSubtitles}>{subtitlePreviewVisible ? tr('清空测试字幕', 'Clear test captions') : tr('显示测试字幕', 'Show test captions')}</button><span>{tr('将这两个公开窗口放到会场屏幕；本操作台留在电脑上。', 'Move the public windows to venue screens; keep this console on your Mac.')}</span></div>{/if}
        {#if running}<div class="private-insights">{#key liveSession}<LiveInsights sessionId={liveSession} {running}/>{/key}</div>{/if}
      </div>
      <div class="workspace-view" hidden={!workspaceVisible}><ForumWorkspace {running} active={workspaceVisible} requestedSession={workspaceSession} requestVersion={workspaceRequest} on:runtime={(event) => applyRuntime(event.detail)} /></div>
    </div>
    <footer class="meeting-footer"><div class="meeting-readiness">
      {#if errorMessage}<strong class="error" role="alert">{errorMessage}</strong>
      {:else if !running && !modelStatus}<span>{tr('正在检查本地 AI…', 'Checking local AI…')}</span>
      {:else if setupIssue && !browserPreview}<strong class="error">{tr('本地运行时未就绪', 'Local runtime unavailable')}</strong><button on:click={() => openSettings('models')}>{tr('查看详情', 'View details')}</button>
      {:else if modelStatus && needsModelDownload(modelStatus,settings)}<strong class="error">{tr('本场所需模型尚未安装', 'Models required for this meeting are missing')}</strong><button on:click={() => openSettings('models')}>{tr('设置本地 AI', 'Set up local AI')}</button>
      {:else}<span class:ready={!running}>{runtimeDisplayMessage($uiLocale, runtimeStatus, runtimeMessage, settings.targetLanguage)}</span>{/if}
      {#if running && usage}<b class="session-timer">{formatDuration(usage.currentSessionSeconds)}</b>{/if}
      {#if !running && !workspaceVisible}<small>{tr('点击开始后才会采集音频。', 'Audio capture starts only when you press Start.')}</small>{/if}
    </div><button class:running class="launch-button" disabled={busy || browserPreview || (!running && (!modelStatus || !!setupIssue || needsModelDownload(modelStatus, settings)))} on:click={toggleTranslation}><span>{running ? '■' : '▶'}</span>{busy ? tr('请稍候…', 'Please wait…') : running ? tr('结束会议', 'End meeting') : tr('开始会议', 'Start meeting')}</button></footer>
  </main>
  {#if appSettingsOpen}<ApplicationSettings bind:settings bind:section={settingsSection} {running} {modelStatus} {usage} error={errorMessage} save={() => persist()} close={() => appSettingsOpen = false} language={selectUiLanguage} theme={selectAccentTheme} download={() => downloadModels('core')} refreshModels={refreshModelStatus}/>{/if}
{:else}<main class="loading-screen"><p>{tr('正在加载', 'Loading')} {productName}…</p></main>{/if}
