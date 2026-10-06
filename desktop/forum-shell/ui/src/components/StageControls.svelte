<script lang="ts">
  import { onMount } from 'svelte';
  import { pair, uiLocale } from '../lib/i18n';
  import { getOverlayState, showMeetingWindow, isTauri } from '../lib/api';
  import { forumClient as client } from '../lib/forum/factory';
  import { previewSessionPage } from '../lib/forum/preview';
  export let running = false;
  export let starting = false;
  export let sessionId: string | null = null;
  let error = '';
  let busy = false;
  let disposed = false;
  let reading = false;
  let generation = 0;
  function invalidateSession() { generation++; if (!running) sessionId = null; }
  $: if (starting) invalidateSession();
  $: tr = $pair;
  async function refresh() {
    if (reading || disposed || starting) return;
    const version = generation;
    reading = true;
    try { const state = await getOverlayState(); if (!disposed && version === generation && !starting) sessionId = state.sessionId ?? (!isTauri() ? previewSessionPage.items[0].session.session_id : null); }
    catch (e) { if (!disposed) error = String(e); }
    finally { reading = false; }
  }
  async function open(screen: 'captions' | 'insights') {
    if (busy) return;
    busy = true; error = '';
    try {
      if (screen === 'captions') await showMeetingWindow();
      else if (sessionId) await client.showInsightWall(sessionId, $uiLocale);
    } catch (e) { error = String(e); }
    finally { busy = false; }
  }
  onMount(() => { void refresh(); const timer = window.setInterval(refresh, 1500); return () => { disposed = true; window.clearInterval(timer); }; });
</script>

<section class="stage-controls" aria-label={tr('观众屏幕', 'Audience screens')}>
  <button disabled={busy || !isTauri()} on:click={() => open('captions')}><span class="screen-icon">01</span><span><strong>{tr('打开字幕屏', 'Open captions')} ↗</strong><small>{tr('原文与译文', 'Original and translated speech')}</small></span></button>
  <button disabled={busy || !isTauri() || !sessionId || starting || !running} on:click={() => open('insights')}><span class="screen-icon">02</span><span><strong>{tr('打开洞察墙', 'Open insight wall')} ↗</strong><small>{running ? tr('查看已上墙的内容', 'View published insights') : tr('开始会议后可打开', 'Available when the meeting starts')}</small></span></button>
  {#if error}<p role="alert">{error}</p>{/if}
</section>
<style>
.stage-controls{display:flex;gap:12px;margin:18px 0 0}.stage-controls button{display:flex;flex:1;align-items:center;text-align:left;gap:12px;border:1px solid #dce2ea;background:#fff;padding:12px 16px;border-radius:8px;color:#283849}.screen-icon{font-size:16px;color:#8492a4;border:1px solid #d7dee8;border-radius:4px;padding:4px}.stage-controls strong{display:block;font-size:13px;font-weight:600}.stage-controls small{display:block;margin-top:4px;color:#7e8999;font-size:11px}button:disabled{opacity:.5;cursor:default}p{color:#a34432;font-size:12px}@media(max-width:640px){.stage-controls{flex-wrap:wrap}.stage-controls button{min-width:160px}}
</style>
