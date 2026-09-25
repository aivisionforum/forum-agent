<script lang="ts">
  import { onMount, tick } from 'svelte';
  import MeetingAssistantSettings from './components/MeetingAssistantSettings.svelte';
  import ForumWorkspace from './ForumWorkspace.svelte';
  import logoUrl from '../../icons/logo-mark.png';
  import { productName, developmentVersion } from './lib/product';
  import { meetingMode, settingsForMeetingMode, type MeetingMode } from './lib/meeting-mode';
  import {
    getSettings,
    showMeetingWindow,
    listenWorkspace,
    listenLiveSettings,
    isTauri,
    getModelStatus,
    getUsage,
    listenRuntime,
    listenInputDevices,
    openTranscriptHistory,
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
  const fontSizes = ['16', '20', '24', '30', '36', '44', '52', '64', '80', '96', '120', '160'];
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
  let advancedOpen = false;
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
    if (code === 'auto') return tr('中英自动识别', 'Automatic Chinese / English');
    if (code === 'bilingual') return tr('中文和英文', 'Chinese + English');
    const language = languages.find((item) => item.code === code);
    return language ? (isEnglish() ? language.en : language.zh) : code.toUpperCase();
  }

  function deviceName(value: string): string {
    if (value === '__dual_audio__') return tr('麦克风 + 系统音频', 'Microphone + System Audio');
    if (value === '__system_audio__') return tr('系统音频', 'System Audio');
    if (value === '__default_microphone__') return tr('默认麦克风', 'Default Microphone');
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
    document.getElementById('meeting-assistant-settings')?.scrollIntoView({behavior:'smooth',block:'center'});
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
      if (!wasRunning) settings = settingsForMeetingMode(settings, meetingMode(settings));
      const state = wasRunning ? await stopTranslation() : await startTranslation(settings);
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
    if (['error', 'degraded', 'draining', 'stopping'].includes(runtimeStatus)) return runtimeMessage;
    if (runtimeStatus === 'listening') return settings?.targetLanguage === 'none' ? tr('正在转写 · 仅原文', 'Transcribing · source only') : tr('实时双语进行中', 'Bilingual meeting active');
    if (runtimeStatus === 'warming') return tr('正在准备会议…', 'Preparing meeting…');
    return runtimeMessage || tr('本地 AI 已就绪', 'Local AI is ready');
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
      applyAccentTheme(settings.accentTheme);
      subtitlePreviewVisible = data.subtitlePreviewVisible;
      running = data.running;
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
            browserPreview ? '界面预览' : runtimeStatus === 'listening' ? '会议中' : runtimeStatus === 'warming' ? '正在准备' : runtimeStatus === 'draining' ? '保存尾句中' : runtimeStatus === 'stopping' ? '正在停止' : runtimeStatus === 'degraded' ? '部分功能待恢复' : runtimeStatus === 'error' ? '需要处理' : '已停止',
            browserPreview ? 'UI PREVIEW' : runtimeStatus === 'listening' ? 'LIVE' : runtimeStatus === 'warming' ? 'WARMING UP' : runtimeStatus === 'draining' ? 'SAVING LAST SEGMENTS' : runtimeStatus === 'stopping' ? 'STOPPING' : runtimeStatus === 'degraded' ? 'RECOVERY NEEDED' : runtimeStatus === 'error' ? 'NEEDS ATTENTION' : 'STOPPED'
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

    <nav class="forum-navigation" aria-label="应用导航"><button class:active={!workspaceVisible} on:click={() => workspaceVisible = false}>{tr('实时控制', 'LIVE CONTROLS')}</button><button class:active={workspaceVisible} on:click={() => workspaceVisible = true}>{tr('会议工作台', 'FORUM WORKSPACE')}</button><span class="nav-note">LOCAL FIRST · HUMAN REVIEWED</span></nav>
    <div class="control-view" hidden={workspaceVisible}>
    {#if settings.sourceLanguage === 'auto' && !modelStatus?.automaticAsrReady && !browserPreview}
      <p role="status" class="error-message">{modelStatus?.automaticAsrDetail ?? tr('自动识别模型准备状态待检查', 'Checking automatic ASR readiness')}</p>
    {/if}


    <section class="control-grid">
      <div class="grid-row route-row">
        <div class="row-number">01</div>
        <div class="row-title">
          <strong>{tr('语言与音频', 'LANGUAGE + AUDIO')}</strong>
          <span>{tr('输入路线', 'INPUT ROUTE')}</span>
        </div>
        <div class="row-controls route-controls">
          <div class="meeting-language-mode">
            <span class="control-label">{tr('会议语言', 'MEETING LANGUAGE')}</span>
            <div class="segmented three" role="group" aria-label="会议语言模式">
              <button class:active={meetingMode(settings) === 'mixed'} aria-pressed={meetingMode(settings) === 'mixed'} disabled={running} on:click={() => chooseMeetingMode('mixed')}>{tr('中英混合', 'CHINESE + ENGLISH')}</button>
              <button class:active={meetingMode(settings) === 'zh'} aria-pressed={meetingMode(settings) === 'zh'} disabled={running} on:click={() => chooseMeetingMode('zh')}>{tr('纯中文', 'CHINESE ONLY')}</button>
              <button class:active={meetingMode(settings) === 'en'} aria-pressed={meetingMode(settings) === 'en'} disabled={running} on:click={() => chooseMeetingMode('en')}>{tr('纯英文', 'ENGLISH ONLY')}</button>
            </div>
            <small>{meetingMode(settings) === 'mixed' ? tr('自动识别中英，显示双语字幕', 'Automatic Chinese / English with bilingual captions') : tr('仅语音识别，不运行翻译', 'Transcription only, no translation')}</small>
          </div>
          <label class="meeting-audio-input"><span class="control-label">{tr('输入音频', 'AUDIO INPUT')}</span>
            <select disabled={running} bind:value={settings.inputDevice} on:change={persist}>
              {#each [...new Set(['__dual_audio__',...payload.inputDevices,settings.inputDevice].filter(Boolean))] as device}<option value={device}>{deviceName(device)}</option>{/each}
            </select>
          </label>
          <div class="recording-choice">
            <label><input type="checkbox" bind:checked={settings.recordingEnabled} disabled={running} on:change={persist} />{tr('保存录音', 'SAVE AUDIO')}</label>
            <span>{settings.recordingEnabled ? tr('支持原文恢复与匿名标签', 'Enables recovery and speaker labels') : tr('仅保存文字，未识别音频无法恢复', 'Text is saved; pending audio cannot be recovered')}</span>
          </div>
        </div>
      </div>

      <div class="grid-row subtitle-row">
        <div class="row-number">02</div>
        <div class="row-title">
          <strong>{tr('实时会议窗口', 'LIVE MEETING WINDOW')}</strong>
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
            {#if settings.targetLanguage === 'none'}
              <div class="source-only-layout">{settings.sourceLanguage === 'en' ? tr('仅显示英文原文', 'English transcript') : tr('仅显示中文原文', 'Chinese transcript')}</div>
            {:else}
              <div class="segmented two">
                <button class:active={!settings.subtitleSplit} on:click={async () => { settings!.subtitleSplit = false; settings = { ...settings! }; await persist(); }}>{tr('上下对照', 'STACKED')}</button>
                <button class:active={settings.subtitleSplit} on:click={async () => { settings!.subtitleSplit = true; settings = { ...settings! }; await persist(); }}>{tr('逐句双行', 'SENTENCE PAIRS')}</button>
              </div>
            {/if}
          </div>

          <div class="control-block subtitle-action-control">
            <span class="control-label tools-label">{tr('字幕调整', 'SUBTITLE TOOLS')}<button class="tools-settings" on:click={() => advancedOpen = true}>{tr('高级设置', 'ADVANCED')} ↗</button></span>
            <div class="subtitle-action-pair">
              <button class="wide-outline" on:click={() => showMeetingWindow().catch(e => errorMessage = String(e))}>{tr('打开会议窗口', 'OPEN WINDOW')} ↗</button>
              <button class:active={subtitlePreviewVisible && !running} disabled={running || subtitlePreviewBusy} class="subtitle-preview-button" on:click={toggleTestSubtitles}>
                {running ? tr('实时字幕中', 'LIVE') : subtitlePreviewVisible ? tr('清空字幕', 'CLEAR') : tr('测试字幕', 'TEST')}
              </button>
            </div>
          </div>
        </div>
      </div>

      <div class="grid-row assistant-row" id="meeting-assistant-settings">
        <div class="row-number">03</div><div class="row-title"><strong>会议助理</strong><span>匿名标签 · 实时洞察</span></div>
        <div class="row-controls"><MeetingAssistantSettings bind:settings save={() => persist(true)} {running} active={!workspaceVisible}/></div>
      </div>
    </section>

    <footer class="launch-area">
      <div class="launch-meta">
        <span>{settings.targetLanguage === 'none' ? tr(`${languageName(settings.sourceLanguage)} · 仅转写`, `${languageName(settings.sourceLanguage)} · transcription`) : tr('中英混合 · 双语字幕', 'Chinese + English · bilingual')}</span>
        <span>{deviceName(settings.inputDevice)}</span>
        {#if errorMessage}<strong class="error">{errorMessage}</strong>{:else}<span>{runtimeDisplayMessage()}</span>{/if}
        {#if usage && running}<strong class="session-timer">{formatDuration(usage.currentSessionSeconds)}</strong>{/if}
      </div>
      <button class:running class="launch-button" disabled={busy || browserPreview} on:click={toggleTranslation}>
        <span>{running ? '■' : '▶'}</span>
        {browserPreview ? tr('界面预览 · 请在桌面应用中启动', 'UI PREVIEW · START IN DESKTOP APP') : busy ? tr('请稍候…', 'PLEASE WAIT…') : running ? tr('结束会议', 'END MEETING') : tr('开始会议', 'START MEETING')}
      </button>
    </footer>
    </div>
    <div class="workspace-view" hidden={!workspaceVisible}><ForumWorkspace {running} active={workspaceVisible} requestedSession={workspaceSession} requestVersion={workspaceRequest} on:runtime={(event) => applyRuntime(event.detail)} /></div>
    {#if workspaceVisible}<div class="compact-live-strip"><span>{runtimeDisplayMessage()}</span>{#if running}<button class="stop" disabled={busy} on:click={toggleTranslation}>{tr('结束会议', 'END MEETING')}</button>{:else}<button on:click={() => workspaceVisible = false}>{tr('返回实时控制', 'LIVE CONTROLS')}</button>{/if}</div>{/if}
  </main>

  {#if advancedOpen}
    <div class="modal-backdrop">
      <dialog open class="modal" aria-label={tr('高级字幕设置', 'Advanced subtitle settings')}>
        <header><div><span>02.A</span><h2>{tr('高级字幕设置', 'ADVANCED SUBTITLES')}</h2></div><button on:click={() => advancedOpen = false}>×</button></header>
        <div class="modal-field choice-field">
          <span>{tr('字幕显示内容', 'SUBTITLE CONTENT')}</span>
          <div
            class="segmented two modal-choice"
            title={settings.targetLanguage === 'none' ? tr('纯中文或纯英文模式只显示原文。', 'Single-language meetings show the original transcript.') : undefined}
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
  {#if modelStatus && !(settings.targetLanguage === 'none' ? modelStatus.asrReady : settings.sourceLanguage === 'auto' ? modelStatus.automaticAsrReady && modelStatus.translationReady : modelStatus.coreReady)}
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

<style>
  .route-controls { display:grid;grid-template-columns:minmax(280px,1.35fr) minmax(190px,1fr);gap:8px 20px;align-items:start;padding-top:14px;padding-bottom:12px; }
  .source-only-layout { min-height:42px;display:flex;align-items:center;padding:0 14px;border:1px solid var(--line);font-size:12px;color:var(--muted); }
  .meeting-language-mode,.meeting-audio-input { display:flex;flex-direction:column;gap:8px;min-width:0; }
  .meeting-language-mode .segmented { width:100%; }
  .segmented.three { grid-template-columns:1.15fr 1fr 1fr; }
  .meeting-language-mode small { color:#7c8a9b;font-size:11px;line-height:1.3; }
  .recording-choice { grid-column:1/-1;display:flex;align-items:center;flex-wrap:wrap;gap:10px;padding:0;font-size:12px; }
  .recording-choice label { display:flex;align-items:center;gap:7px; }
  .recording-choice span { color:#8290a1;font-size:11px; }
  .assistant-row .row-controls { min-width:0; }
</style>
