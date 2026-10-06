<script lang="ts">
  import { onMount } from 'svelte';
  import { pair } from '../lib/i18n';
  import { listenAudioLevel, type TranslationSettings } from '../lib/api';
  export let settings: TranslationSettings;
  export let running = false;
  export let save: () => Promise<void>;
  let levels: Array<{rms:number;peak:number}> = [];
  let last = 0;
  let now = Date.now();
  let saving = false;
  let error = '';
  $: tr = $pair;
  $: fresh = running && now - last < 1200;
  $: clipping = fresh && levels.slice(-8).some(level => level.peak >= .95);
  async function change() { if (saving) return; saving=true;error='';try {await save();} catch(e) {error=String(e);} finally {saving=false;} }
  onMount(() => {
    let disposed=false; let unlisten=()=>{};
    void listenAudioLevel(samples => {if(!disposed && running){levels=[...levels,...samples].slice(-72);last=Date.now();}}).then(cleanup=>{if(disposed)cleanup();else unlisten=cleanup;}).catch(e=>error=String(e));
    const timer=window.setInterval(()=>now=Date.now(),400);
    return()=>{disposed=true;unlisten();window.clearInterval(timer);};
  });
</script>
<div class="input-level">
  <label>{tr('输入增益', 'Input gain')}<input aria-label={tr('输入增益', 'Input gain')} type="range" min="0" max="3" step="0.05" bind:value={settings.inputGain} disabled={saving} on:change={change}/><output>{Number(settings.inputGain).toFixed(2)}×</output></label>
  {#if running}<div class="waveform" aria-label={tr('输入音量波形', 'Input level waveform')} aria-hidden="true">{#each levels as level}<i class:clip={level.peak >= .95} style={`height:${fresh ? Math.max(2,Math.sqrt(level.rms)*36) : 2}px`}></i>{/each}</div><span class:clipping>{clipping ? tr('音量过高，请降低增益', 'Clipping — lower the gain') : fresh ? tr('正在接收音频', 'Receiving audio') : tr('等待音频', 'Waiting for audio')}</span>{:else}<small>{tr('开始会议后显示音量波形；默认 1×。', 'Levels appear once the meeting starts. Default: 1×.')}</small>{/if}
  {#if error}<p role="alert">{error}</p>{/if}
</div>
<style>
.input-level{display:flex;align-items:center;gap:16px;flex-wrap:wrap;padding:12px 0 0;font-size:11px;color:#778598}.input-level label{display:flex;align-items:center;gap:12px;color:#596c81}.input-level input{width:115px;accent-color:var(--accent)}output{font-variant-numeric:tabular-nums;min-width:40px}.waveform{display:flex;align-items:center;gap:2px;height:32px;max-width:240px;flex:1;overflow:hidden;border-bottom:1px solid #e4e9ef}.waveform i{display:block;min-width:2px;flex:1;background:#5670b4}.waveform .clip{background:#c63c39}.clipping,p{color:#b74433}small{font-size:10px}
</style>
