<script lang="ts">
  import { pair, uiLocale, type Locale } from '../lib/i18n';
  import LanguageSwitch from './LanguageSwitch.svelte';
  import { focusSettings } from '../lib/settings-dialog';
  import { productName, developmentVersion } from '../lib/product';
  import type { TranslationSettings, ModelStatus, UsageSnapshot, AccentTheme } from '../lib/api';
  export let settings: TranslationSettings;
  export let section = 'general';
  export let running = false;
  export let modelStatus: ModelStatus | null = null;
  export let usage: UsageSnapshot | null = null;
  export let error = '';
  export let save: () => Promise<void>;
  export let close: () => void;
  export let language: (locale: Locale) => Promise<void>;
  export let theme: (theme: AccentTheme) => Promise<void>;
  export let download: () => Promise<void>;
  export let refreshModels: () => Promise<void>;
  $: tr = $pair;
  $: categories = [
    {id:'general', title:tr('通用', 'General'), description:tr('界面语言、主题与运行行为', 'Language, theme and app behavior')},
    {id:'subtitles', title:tr('字幕显示', 'Captions'), description:tr('公开字幕窗口的外观；调整立即生效', 'Appearance of the public caption window; changes apply immediately')},
    {id:'models', title:tr('本地 AI', 'Local AI'), description:tr('模型安装与运行环境', 'Installed models and runtime readiness')},
    {id:'storage', title:tr('记录与统计', 'Records & usage'), description:tr('文字导出与本机使用情况', 'Transcript exports and usage on this Mac')}
  ];
  const themes: Array<[AccentTheme,string,string,string]> = [['neon-blue','蓝色','Blue','#0003fe'],['neon-orange','橙色','Orange','#ff5705'],['neon-pink','粉色','Pink','#ff0073'],['neon-green','绿色','Green','#51f91b']];
  const fonts = ['16','20','24','30','36','44','52','64','80','96','120','160'];
  async function layout(value: string) {
    settings = {...settings, subtitleSideBySide: value === 'columns', subtitleSplit: value === 'pairs'};
    await save();
  }
  function duration(seconds: number) { return `${Math.floor(seconds/3600)}h ${Math.floor(seconds%3600/60)}m`; }
</script>

<svelte:window on:keydown={event => {if (event.key === 'Escape') {event.preventDefault(); close();}}} />
<div class="modal-backdrop">
  <dialog open aria-modal="true" aria-labelledby="application-settings-title" class="application-settings" use:focusSettings>
    <header><div><h2 id="application-settings-title">{tr('应用设置', 'Application settings')}</h2><p>{tr('保存在这台电脑，供后续会议使用。', 'Saved on this Mac for future meetings.')}</p></div><button class="close" aria-label={tr('关闭设置', 'Close settings')} on:click={close}>×</button></header>
    <div class="settings-body">
      <nav aria-label={tr('设置分类', 'Settings categories')}>
        {#each categories as category}<button aria-current={section === category.id ? 'page' : undefined} on:click={() => section = category.id}>{category.title}</button>{/each}
        <small>{productName}<br />{developmentVersion}</small>
      </nav>
      {#key section}<section class="settings-pane" aria-label={categories.find(c => c.id === section)?.title}>
        <h3>{categories.find(c => c.id === section)?.title}</h3><p class="intro">{categories.find(c => c.id === section)?.description}</p>
        {#if error}<p class="settings-error" role="alert">{error}</p>{/if}
        {#if section === 'general'}
          <div class="setting"><span>{tr('界面语言', 'Interface language')}</span><LanguageSwitch value={$uiLocale} onchange={language} /></div>
          <div class="setting"><span>{tr('主题颜色', 'Accent color')}</span><div class="swatches">{#each themes as item}<button aria-label={tr(item[1],item[2])} aria-pressed={settings.accentTheme === item[0]} style={`--swatch:${item[3]}`} on:click={() => theme(item[0])}><i></i></button>{/each}</div></div>
          <label class="setting"><span>{tr('会议期间保持屏幕唤醒', 'Keep the screen awake during meetings')}<small>{tr('结束会议后恢复系统原有设置。', 'Normal system behavior resumes when the meeting ends.')}</small></span><input type="checkbox" bind:checked={settings.keepAwakeDuringTranslation} on:change={save} /></label>
          <div class="about"><strong>{productName}</strong><span>{developmentVersion} · {tr('开发版本', 'Development build')}</span><p>{tr('当前为论坛专用版本，更新由操作员安装。', 'This Forum edition is updated by the operator.')}</p></div>
          <p class="help">{tr('语音、字幕与偏好设置保留在本机。', 'Audio, captions and preferences remain on this Mac.')}</p>
        {:else if section === 'subtitles'}
          <label class="setting"><span>{tr('字体大小', 'Type size')}</span><select bind:value={settings.fontSizePreset} on:change={save}>{#each fonts as size}<option value={size}>{size} pt</option>{/each}</select></label>
          {#if settings.targetLanguage === 'none'}<p class="help">{tr('纯中文或纯英文模式只显示原文。', 'Single-language meetings show the original transcript.')}</p>{/if}
          <label class="setting"><span>{tr('字幕内容', 'Caption content')}</span><select disabled={settings.targetLanguage === 'none'} bind:value={settings.translationOnly} on:change={save}><option value={false}>{tr('双语', 'Bilingual')}</option><option value={true}>{tr('仅译文', 'Translation only')}</option></select></label>
          <fieldset disabled={settings.targetLanguage === 'none' || settings.translationOnly}><legend>{tr('双语布局', 'Bilingual layout')}</legend><div class="layout-options">{#each [['stacked','上下对照','Stacked','═'],['pairs','逐句双行','Sentence pairs','☷'],['columns','左右对照','Side by side','Ⅱ']] as item}<button class:chosen={item[0] === (settings.subtitleSideBySide ? 'columns' : settings.subtitleSplit ? 'pairs' : 'stacked')} aria-pressed={item[0] === (settings.subtitleSideBySide ? 'columns' : settings.subtitleSplit ? 'pairs' : 'stacked')} on:click={() => layout(item[0])}><b aria-hidden="true">{item[3]}</b>{tr(item[1],item[2])}</button>{/each}</div></fieldset>
          <label class="setting"><span>{tr('窗口不透明度', 'Window opacity')}</span><input type="range" min="0.35" max="1" step="0.05" bind:value={settings.overlayOpacity} on:change={save} /><output>{Math.round(settings.overlayOpacity*100)}%</output></label>
          <label class="setting"><span>{tr('字幕垂直位置', 'Vertical anchor')}</span><select bind:value={settings.anchorPositionPreset} on:change={save}>{#each ['35','50','70','100'] as anchor}<option value={anchor}>{anchor}%</option>{/each}</select></label>
          <p class="help">{tr('这些设置只影响公开字幕窗口。洞察墙按内容自动翻页。', 'These settings apply to public captions. The insight wall paginates automatically.')}</p>
        {:else if section === 'models'}
          {#if modelStatus}
            <div class="model"><div><strong>Qwen3-ASR · 1.7B</strong><small>{tr('中英语音识别', 'Chinese / English speech recognition')}</small></div><span class:ready={modelStatus.asrReady ?? modelStatus.coreReady}>{(modelStatus.asrReady ?? modelStatus.coreReady) ? tr('已安装','Installed') : tr('需下载','Download required')}</span></div>
            <div class="model"><div><strong>Hy-MT2 · 1.8B · MLX 4-bit</strong><small>{tr('本地中英翻译', 'Local Chinese / English translation')}</small></div><span class:ready={modelStatus.translationReady ?? modelStatus.coreReady}>{(modelStatus.translationReady ?? modelStatus.coreReady) ? tr('已安装','Installed') : tr('需下载','Download required')}</span></div>
            <p class="help">{tr('本版使用已验证的 Forum 翻译引擎。单语会议仅运行语音识别。', 'This edition uses the validated Forum translation engine. Single-language meetings use speech recognition only.')}</p>
            <div class="model-actions"><button disabled={running || modelStatus.downloading || modelStatus.coreReady} on:click={download}>{modelStatus.downloading ? tr('下载中…','Downloading…') : modelStatus.coreReady ? tr('核心模型已安装','Core models installed') : tr('下载缺失模型','Download missing models')}</button><button disabled={modelStatus.downloading} on:click={refreshModels}>{tr('重新检查','Recheck')}</button></div>
            {#if modelStatus.downloading}<progress max="1" value={modelStatus.progress}></progress><p class="help">{modelStatus.title} · {modelStatus.detail} · {Math.round(modelStatus.progress*100)}%</p>{/if}
            {#if !modelStatus.downloading && !modelStatus.coreReady && modelStatus.detail}<p class="help" role="status">{modelStatus.title} · {modelStatus.detail}</p>{/if}
            <details open={modelStatus.automaticAsrReady === false || modelStatus.translationRuntimeReady === false}><summary>{tr('运行环境详情', 'Runtime details')}</summary><p>{tr('语音运行时', 'Speech runtime')} · {modelStatus.automaticAsrReady === true ? tr('就绪','Ready') : modelStatus.automaticAsrReady === false ? tr('未就绪','Unavailable') : tr('待检测','Not checked')}</p><p>{modelStatus.automaticAsrDetail}</p><p>{tr('翻译运行时','Translation runtime')} · {modelStatus.translationRuntimeReady === true ? tr('就绪','Ready') : modelStatus.translationRuntimeReady === false ? tr('未就绪','Unavailable') : tr('待检测','Not checked')}</p><p>{modelStatus.translationRuntimeDetail}</p><p class="help">{tr('模型已安装但运行时未就绪时，无需重复下载模型。', 'If weights are installed but the runtime is unavailable, downloading weights again will not repair it.')}</p></details>
          {:else}<p role="status">{tr('正在检查本地模型…','Checking local models…')}</p>{/if}
          <label class="setting"><span>{tr('最长字幕间隔', 'Maximum caption interval')}<small>{tr('间隔越短，语音片段越短；下次开会生效。', 'Shorter intervals split speech sooner. Applies to the next meeting.')}{running ? tr('结束会议后可调整。', 'Adjust after ending the meeting.') : ''}</small></span><input type="range" min="3" max="8" step="1" disabled={running} bind:value={settings.finalIntervalSeconds} on:change={save} /><output>{settings.finalIntervalSeconds}s</output></label>
        {:else if section === 'storage'}
          <label class="setting"><span>{tr('自动导出文字记录', 'Automatically export transcripts')}<small>{tr('会议记录始终保存在工作台；此选项额外导出文字文件。', 'Meeting records remain in the workspace. This additionally exports a text file.')}</small></span><input type="checkbox" bind:checked={settings.autoSaveTranscript} on:change={save} /></label>
          <p class="help">{tr('是否保存录音，在每场会议开始前选择。', 'Choose whether to save audio before each meeting.')}</p>
          {#if usage}<h4>{tr('本机使用统计','Usage on this Mac')}</h4><div class="usage-cards"><div><small>{tr('累计','Total')}</small><strong>{duration(usage.lifetimeSeconds)}</strong></div><div><small>{tr('本月','This month')}</small><strong>{duration(usage.monthlySeconds)}</strong></div><div><small>{tr('完成会议','Completed')}</small><strong>{usage.completedSessions}</strong></div></div>{/if}
        {/if}
      </section>{/key}
    </div>
  </dialog>
</div>

<style>
.application-settings{margin:auto;width:min(900px,94vw);height:min(620px,90vh);max-height:90vh;padding:0;display:flex;flex-direction:column;border:1px solid #d6dbe2;border-radius:14px;box-shadow:0 24px 80px #14203830;background:#fff;color:#202a38}.application-settings header{display:flex;align-items:center;justify-content:space-between;padding:20px 24px;border-bottom:1px solid #e5e8ed}h2{font-size:20px;margin:0}header p{margin:6px 0 0;color:#687487;font-size:12px}.close{border:0;background:#f1f3f6;border-radius:6px;width:32px;height:32px;font-size:24px}.settings-body{display:flex;min-height:0;flex:1}nav{width:172px;flex-shrink:0;display:flex;flex-direction:column;gap:6px;padding:18px 12px;background:#f8f9fb;border-right:1px solid #e5e8ed}nav button{text-align:left;font-size:13px;border:0;background:transparent;padding:12px;border-radius:7px;color:#526072}nav button[aria-current]{background:var(--accent-soft);color:var(--accent-text);font-weight:650}nav small{margin-top:auto;padding:12px;font-size:10px;line-height:1.7;color:#788294}.settings-pane{flex:1;min-width:0;padding:24px 28px;overflow-y:auto}h3{font-size:20px;margin:0 0 8px}.intro{font-size:12px;color:#728095;margin:0 0 22px}.setting{display:flex;align-items:center;justify-content:space-between;gap:16px;padding:18px 0;border-bottom:1px solid #edf0f4;font-size:13px}.setting>span:first-child{flex:1}.setting small{display:block;color:#7a8594;font-size:11px;line-height:1.6;margin-top:6px}.setting select{max-width:190px;min-width:84px;padding:8px;border:1px solid #d4dae2;border-radius:6px;background:white}.setting input[type=range]{width:120px;accent-color:var(--accent)}input[type=checkbox]{width:18px;height:18px;accent-color:var(--accent)}output{font-size:12px;min-width:34px;text-align:right}.swatches{display:flex;gap:8px}.swatches button{padding:5px;background:white;border:2px solid transparent;border-radius:50%}.swatches button[aria-pressed=true]{border-color:var(--swatch)}.swatches i{display:block;width:20px;height:20px;border-radius:50%;background:var(--swatch)}fieldset{margin:20px 0;padding:0;border:0}legend{font-size:13px;margin-bottom:10px}.layout-options{display:grid;grid-template-columns:repeat(3,1fr);gap:10px}.layout-options button{border:1px solid #d6dde6;border-radius:8px;background:#fff;font-size:12px;padding:12px 4px}.layout-options b{display:block;font-size:27px;color:#8491a4;margin-bottom:8px}.layout-options button.chosen{border-color:var(--accent);background:var(--accent-soft);color:var(--accent-text)}fieldset:disabled{opacity:.45}.help,details{font-size:12px;color:#687487;line-height:1.7}.about{display:flex;flex-direction:column;gap:7px;margin-top:28px}.about span,.about p{font-size:12px;color:#6d7888}.model{display:flex;gap:12px;justify-content:space-between;padding:16px 0;border-bottom:1px solid #edf0f4;font-size:13px}.model small{display:block;color:#788597;font-size:11px;margin-top:6px}.model>span{font-size:11px;color:#ab6632}.model .ready{color:#3d8063}.model-actions{display:flex;gap:10px;margin:16px 0}.model-actions button{background:#f6f8fb;border:1px solid #d4dce8;border-radius:6px;padding:9px 12px;font-size:12px}button:disabled{opacity:.5;cursor:default}details{margin:18px 0}summary{cursor:pointer}details p{overflow-wrap:anywhere}progress{width:100%;accent-color:var(--accent)}.settings-error{font-size:12px;background:#fff2ed;color:#a03c29;padding:10px;border-radius:6px;overflow-wrap:anywhere}.usage-cards{display:grid;grid-template-columns:repeat(3,1fr);gap:12px}.usage-cards div{background:#f5f7fa;border-radius:8px;padding:16px}.usage-cards small,.usage-cards strong{display:block}.usage-cards small{color:#7c8695;font-size:11px;margin-bottom:8px}.usage-cards strong{font-size:20px}h4{font-size:13px;margin:24px 0 12px}@media(max-width:640px){nav{width:120px;padding:12px 6px}.settings-pane{padding:18px 14px}.setting{flex-wrap:wrap}.layout-options{gap:5px}.application-settings header{padding:16px}.setting input[type=range]{width:100px}}
</style>
