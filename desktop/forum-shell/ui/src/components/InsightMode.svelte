<script lang="ts">
  import { onMount } from 'svelte';
  import { pair } from '../lib/i18n';
  import { forumClient as client } from '../lib/forum/factory';
  export let sessionId: string | null = null;
  export let mode: 'gated' | 'automatic' = 'gated';
  export let locked = false;
  export let compact = false;
  let loaded = false;
  let busy = false;
  let disposed = false;
  let generation = 0;
  let error = '';
  let notice = '';
  $: tr = $pair;
  async function refresh() {
    if (!sessionId || busy || disposed) return;
    const version = ++generation;
    try {
      const saved = await client.insightSettings(sessionId);
      if (!disposed && version === generation) { mode = saved.mode; loaded = true; error = ''; }
    } catch (e) { if (!disposed && version === generation) error = String(e); }
  }
  async function choose(next: 'gated' | 'automatic') {
    if (busy || locked) return;
    if (!sessionId) { mode = next; return; }
    busy = true; generation++; error = ''; notice = '';
    try {
      const saved = await client.setInsightMode(sessionId, next);
      if (!disposed) { mode = saved.mode; notice = next; }
    } catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) busy = false; }
  }
  onMount(() => {
    void refresh();
    const timer = window.setInterval(() => { if (!document.hidden) void refresh(); }, 2500);
    return () => { disposed = true; generation++; window.clearInterval(timer); };
  });
</script>

<fieldset class="insight-mode" class:compact disabled={locked || busy || (!!sessionId && (!loaded || client.mode !== 'desktop'))}>
  <legend>{sessionId ? tr('上墙方式', 'Publishing mode') : tr('本场洞察上墙模式', 'SESSION · INSIGHT APPROVAL')}</legend>
  <div class="choices">
    <button type="button" class:chosen={mode === 'gated'} title={tr('先在操作台审核，批准后才公开。', 'Review privately before publishing.')} aria-pressed={mode === 'gated'} on:click={() => choose('gated')}><strong>{tr('守门模式', 'Gated approval')}</strong><span>{tr('核对正文后，点击“上墙”公开。', 'Review the copy, then click Publish to wall.')}</span></button>
    <button type="button" class:chosen={mode === 'automatic'} title={tr('新要点直接上墙，可现场编辑或隐藏。', 'Publish new insights automatically; edit or hide them live.')} aria-pressed={mode === 'automatic'} on:click={() => choose('automatic')}><strong>{tr('自动批准', 'Automatic approval')}</strong><span>{tr('新要点生成后直接上墙，操作员现场编辑或隐藏。', 'New insights go straight to the wall; the operator edits or hides them live.')}</span></button>
  </div>
  {#if compact}<p class="mode-explanation">{mode === 'gated' ? tr('核对左侧内容，点击“上墙”即可公开。', 'Review the copy, then click Publish to wall.') : tr('新要点自动上墙；可随时编辑或隐藏。', 'New insights publish automatically. Edit or hide them here.')}</p>{/if}
  <p class="cadence">{tr('约 3 分钟一轮 · 仅用于本场洞察 · 来源核验失败的内容留在操作台', 'About every 3 minutes · Session insights only · Failed source checks stay private')}</p>
  {#if busy}<p role="status">{tr('正在保存本场模式…', 'Saving session mode…')}</p>{/if}
  {#if notice}<p role="status">{notice === 'automatic' ? tr('已启用自动批准；从后续新要点开始生效，已有草稿仍待审核。', 'Automatic approval is active for new insights. Existing drafts still need review.') : tr('已启用守门模式；后续新要点等待审核，已上墙内容可单独隐藏。', 'Gated approval is active for new insights. Published items can be hidden individually.')}</p>{/if}
</fieldset>
{#if error}<p class="mode-error" role="alert">{error}</p>{/if}

<style>
  fieldset{border:1px solid #cfd5dc;padding:14px 18px;margin:12px 0;background:#fff;min-width:0}legend{font-size:12px;font-weight:600;padding:0 6px;color:#334052}.choices{display:grid;grid-template-columns:1fr 1fr;gap:12px}button{display:flex;flex-direction:column;text-align:left;gap:6px;background:#f6f7f9;border:1px solid #d2d7df;border-radius:5px;padding:13px 16px;font:inherit;color:#334052;cursor:pointer}button.chosen{border:2px solid var(--accent,#0003fe);padding:12px 15px;background:#f0f2ff}strong{font-size:15px}span{font-size:12px;line-height:1.7}p{font-size:11px;line-height:1.7;color:#637080;margin:10px 0 0}.mode-error{color:#a52e2e}fieldset:disabled button{cursor:default;opacity:.65}@media(max-width:680px){.choices{grid-template-columns:1fr}}
.compact{padding:10px 14px;margin:14px 0 0}.compact .choices{gap:8px}.compact button{padding:8px 12px}.compact button.chosen{padding:7px 11px}.compact strong{font-size:12px}.compact span{font-size:11px}.compact p{margin-top:6px;font-size:10px}
</style>
