<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import { pair, uiLocale } from '../lib/i18n';
  import { forumClient as client } from '../lib/forum/factory';
  import { canReviewForWall, type ArtifactRecord, type ArtifactPublishCommand } from '../lib/forum/client';
  import { publishReviewedInsight } from '../lib/forum/publish-insight';
  export let artifact: ArtifactRecord;
  export let sessionId: string;
  const dispatch = createEventDispatcher<{updated: ArtifactRecord; notice: string}>();
  let proposal: ArtifactPublishCommand | null = null;
  let preparing = true;
  let busy = false;
  let disposed = false;
  let error = '';
  let windowError = '';
  let editing = false;
  let draftTexts: string[] = [];
  $: tr = $pair;
  $: claims = artifact.content.sections.flatMap(s => s.claims);
  $: evidenceQuotes = [...new Set(claims.flatMap(c => c.evidence.map(e => e.kind === 'source' ? e.span.quote : e.quote)))];
  $: published = artifact.publication === 'published';
  $: eligible = canReviewForWall(artifact) && claims.length > 0
    && claims.every(c => c.grounding === 'cited' && c.evidence.length > 0);
  $: timestamp = new Date(artifact.created_at_ms).toLocaleTimeString($uiLocale === 'en' ? 'en-US' : 'zh-CN', {hour:'2-digit',minute:'2-digit'});
  async function prepare() {
    preparing = true; proposal = null; error = '';
    try {
      if (eligible) {
        const next = await client.prepareInsightPublication(artifact.artifact_id, artifact.revision);
        if (!disposed) proposal = next;
      }
    } catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) preparing = false; }
  }
  async function publish() {
    if (busy || !proposal || client.mode !== 'desktop') return;
    busy = true; error = ''; windowError = '';
    try {
      // Do not re-prepare here: approval applies to exactly the preview displayed.
      const result = await publishReviewedInsight(client, proposal, sessionId, $uiLocale);
      if (!disposed) {
        windowError = result.windowError;
        artifact = result.artifact;
        dispatch('updated', result.artifact);
      }
    } catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) busy = false; }
  }
  async function openWall() {
    if (busy || client.mode !== 'desktop') return;
    busy = true; windowError = '';
    try { await client.showInsightWall(sessionId, $uiLocale); }
    catch(e) { if (!disposed) windowError = String(e); }
    finally { if (!disposed) busy = false; }
  }
  async function hide() {
    if (busy || client.mode !== 'desktop') return;
    busy = true; error = '';
    try {
      const hidden = await client.hideArtifact({artifact_id:artifact.artifact_id, expected_revision:artifact.revision,
        publication:'hidden', operator_id:'local-operator', reason:'操作员在本场控制台隐藏洞察'});
      if (!disposed) { dispatch('notice', tr('已隐藏，洞察墙会自动更新。', 'Hidden. The wall updates automatically.')); dispatch('updated', hidden); }
    } catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) busy = false; }
  }
  function edit() { draftTexts = claims.map(c => c.text); editing = true; error = ''; }
  async function save() {
    if (busy || draftTexts.some(text => !text.trim()) || client.mode !== 'desktop') return;
    busy = true; error = '';
    const content = structuredClone(artifact.content);
    let index = 0;
    for (const section of content.sections) for (const claim of section.claims) claim.text = draftTexts[index++].trim();
    try {
      const next = await client.editArtifact({artifact_id:artifact.artifact_id, expected_revision:artifact.revision,
        content, operator_id:'local-operator', reason:'操作员在本场控制台纠正洞察正文'});
      if (!disposed) { editing = false; dispatch('notice', tr('修改已保存，核对正文后点击“上墙”。', 'Edits saved. Review the copy, then publish to the wall.')); dispatch('updated', next); }
    } catch (e) { if (!disposed) error = String(e); }
    finally { if (!disposed) busy = false; }
  }
  onMount(() => { void prepare(); return () => { disposed = true; }; });
</script>

<article class="publication-card" class:published>
  <header class="publication-heading"><span class="publication-state">{published ? tr('已上墙', 'On the wall') : tr('待上墙', 'Ready for review')}</span><span>{client.mode === 'preview' ? tr('合成示例', 'Synthetic example') : timestamp}</span></header>
  {#if editing}
    <div class="publication-editor">
      {#each draftTexts as text, i}<label>{tr('要点', 'Point')} {i + 1}<textarea rows="3" bind:value={draftTexts[i]} disabled={busy}></textarea></label>{/each}
      <p>{published ? tr('保存修改会先从墙上移除；核对后再点击“上墙”。', 'Saving removes this copy from the wall. Review it, then publish again.') : tr('保存后可预览匿名正文，再点击“上墙”。', 'Save to preview the anonymous copy, then publish.')}</p>
      <div class="publication-actions"><button class="publish-primary" disabled={busy || draftTexts.some(text => !text.trim())} on:click={save}>{tr('保存修改', 'Save edits')}</button><button disabled={busy} on:click={() => editing = false}>{tr('取消', 'Cancel')}</button></div>
    </div>
  {:else}
    <div class="publication-copy">{#if proposal}<p>{proposal.reviewed_text}</p>{:else}{#each claims as claim}<p>{claim.text}</p>{/each}{/if}</div>
    {#if preparing}<p class="publication-hint" role="status">{tr('正在准备匿名上墙正文…', 'Preparing anonymous wall copy…')}</p>
    {:else if !eligible}<p class="publication-hint">{tr('来源尚未通过核验，暂不能上墙。等待下一轮生成有引用的要点。', 'Source checks are incomplete. Wait for the next round of cited insights.')}</p>
    {:else if !artifact.coverage_complete}<p class="publication-hint">{tr('本轮仅整理了部分原文；以上要点的引用已核验。', 'This round covers part of the transcript; the points above have checked citations.')}</p>{/if}
    <div class="publication-actions">
      {#if published}<button class="published-action" disabled={busy || client.mode !== 'desktop'} on:click={openWall}>✓ {tr('已上墙 · 查看', 'On the wall · View')} ↗</button>
      {:else}<button class="publish-primary" disabled={!proposal || preparing || busy || client.mode !== 'desktop'} on:click={publish}>{busy ? tr('正在上墙…', 'Publishing…') : tr('上墙 ↗', 'Publish to wall ↗')}</button>{/if}
      <button disabled={busy || client.mode !== 'desktop'} on:click={edit}>{tr('编辑', 'Edit')}</button>
      <button disabled={busy || client.mode !== 'desktop'} on:click={hide}>{tr('隐藏', 'Hide')}</button>
    </div>
    <details class="publication-evidence"><summary>{tr('查看原文依据', 'View source evidence')}</summary>{#each evidenceQuotes as quote}<blockquote>{quote}</blockquote>{/each}</details>
  {/if}
  {#if error}<div class="publication-error" role="alert"><p>{tr('操作未完成。内容可能已更新，请刷新后重试。', 'The action did not complete. Content may have changed; refresh and retry.')}</p><details><summary>{tr('查看详情', 'Details')}</summary>{error}</details><button disabled={busy || preparing} on:click={prepare}>{tr('刷新', 'Refresh')}</button></div>{/if}
  {#if windowError}<div class="publication-error" role="status"><p>{tr('内容已上墙，但窗口未能打开。', 'Published successfully, but the wall window could not open.')}</p><button disabled={busy} on:click={openWall}>{tr('重新打开洞察墙', 'Open insight wall again')}</button><details><summary>{tr('查看详情', 'Details')}</summary>{windowError}</details></div>{/if}
</article>
