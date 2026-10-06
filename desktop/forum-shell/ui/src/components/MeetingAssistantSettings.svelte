<script lang="ts">
  import { t, pair } from '../lib/i18n';
  import {onMount} from 'svelte';
  import {getOverlayState,isTauri,type TranslationSettings} from '../lib/api';
  import {forumClient} from '../lib/forum/factory';
  import {SpeakerClient,type SpeakerStatus} from '../lib/forum/speakers';
  export let settings: TranslationSettings;
  export let save: () => Promise<void>;
  export let running = false;
  export let active = true;
  const client = new SpeakerClient(forumClient.transport);
  $: tr = $pair;
  let status: SpeakerStatus | null = null;
  let sessionId: string | null = null;
  let busy = false;
  let reading = false;
  let destroyed = false;
  let error = '';
  $: statusLabel = ({disabled:'尚未运行',preparing:'正在检查模型',waiting:'等待实时字幕释放资源',running:'正在分析本场说话人',unavailable:'本地模型尚未就绪',stopping:'正在停止',error:'分析失败',uncontained:'等待分析进程退出',stopped:'已停止'}[status?.state ?? 'disabled'] ?? status?.state);
  async function refresh() {
    if (!isTauri() || reading || destroyed) return;
    reading = true;
    try { const [overlay,next] = await Promise.all([getOverlayState(),client.status()]); if (!destroyed) {sessionId=overlay.sessionId??null;status=next;} }
    catch(e) { if(!destroyed) error=String(e); }
    finally { reading=false; }
  }
  async function toggle(value:boolean) {
    if(busy || (value && !settings.recordingEnabled)) return;
    busy=true;error='';
    const previous = settings.speakersEnabled;
    let saved = false;
    try {
      settings={...settings,speakersEnabled:value};
      await save();
      saved = true;
      if(isTauri()) {
        if(value && running) {await refresh();if(!sessionId) throw new Error('已保存设置；当前场次尚未就绪，请稍后重新开启。');}
        if(value && running && sessionId) status=await client.enable(sessionId,null);
        if(!value) status=await client.disable();
      }
    } catch(e) { if(!saved) settings={...settings,speakersEnabled:previous}; if(!destroyed) error=String(e); }
    finally { busy=false;void refresh(); }
  }
  onMount(()=>{void refresh();const timer=window.setInterval(()=>{if(active)void refresh();},2500);return()=>{destroyed=true;window.clearInterval(timer);};});
</script>
<div class="assistant-preferences">
  <section class="assistant-setting"><div><h3>{$t("匿名说话人")}</h3><p>{$t("为每位发言人添加场次内匿名标签，随会议自动启用。")}</p></div><div class="setting-switch" role="group" aria-label={$t("匿名说话人设置")}><button class:active={!settings.speakersEnabled} disabled={busy} on:click={()=>toggle(false)}>{$t("关")}</button><button class:active={settings.speakersEnabled} disabled={busy||!settings.recordingEnabled} on:click={()=>toggle(true)}>{$t("开")}</button></div><small>{!settings.recordingEnabled ? tr('请先开启保存录音。', 'Enable audio recording first.') : settings.speakersEnabled ? running ? $t(statusLabel ?? '') : $t("下次会议开始时自动启用") : $t("只显示字幕，不区分发言人")}</small>{#if settings.speakersEnabled&&status?.notice}<p class="setting-notice">{$t(status.notice)}</p>{/if}</section>
  {#if error}<p class="setting-error" role="alert">{$t(error)}</p>{/if}
</div>
<style>
.assistant-preferences{display:grid;grid-template-columns:1fr;gap:14px;width:100%}.assistant-setting{display:grid;grid-template-columns:1fr auto;gap:10px;align-items:start}.assistant-setting h3{font-size:14px;margin:0 0 9px;font-weight:500;color:#334961}.assistant-setting p{font-size:12px;line-height:1.8;color:#718092;margin:0}.assistant-setting small{grid-column:1/-1;font-size:11px;line-height:1.6;color:#8290a1}.setting-switch{display:flex;border:1px solid #ced6e0;border-radius:5px;overflow:hidden}.setting-switch button{font:inherit;font-size:11px;padding:7px 10px;border:0;background:white;color:#8592a1;cursor:pointer}.setting-switch button.active{background:#eaf0f7;color:#3d5b7e}.setting-switch button:disabled{opacity:.4;cursor:default}.setting-notice,.setting-error{grid-column:1/-1;color:#956c3d!important;font-size:11px}.setting-error{font-size:12px}button:focus-visible{outline:2px solid #7398bb;outline-offset:-2px}
</style>
