<script lang="ts">
  import { pair, t } from '../lib/i18n';
  import { onMount } from 'svelte';
  import { isTauri } from '../lib/api';
  import { forumClient as client } from '../lib/forum/factory';
  import { type AnalysisState, type ArtifactRecord } from '../lib/forum/client';
  import { latestInsight, visibleInsights, meetingTopics, friendlyAnalysisError } from '../lib/forum/live-meeting';
  import InsightPublication from './InsightPublication.svelte';
  export let sessionId: string | null = null;
  export let running = false;
  let analysis: AnalysisState | null = null;
  let reading = false;
  let busy = false;
  let destroyed = false;
  let generation = 0;
  let analysisError = '';
  let actionError = '';
  let notice = '';
  let liveCursor = 0;
  let showHistory = false;
  let observedSession: string | null = null;
  $: tr = $pair;
  $: if (observedSession !== sessionId) {
    generation++; observedSession = sessionId; analysis = null; liveCursor = 0;
    showHistory = false; notice = ''; actionError = ''; analysisError = '';
  }
  $: artifact = latestInsight(analysis, sessionId);
  $: rounds = visibleInsights(analysis, sessionId).filter(a => a.content.sections.some(s => s.claims.length));
  $: topics = meetingTopics(analysis, sessionId);
  $: onWall = rounds.filter(a => a.publication === 'published').length;
  $: job = analysis?.jobs.filter(j => j.kind === 'insight' && j.session_ids.includes(sessionId ?? ''))
    .sort((a,b) => b.created_at_ms - a.created_at_ms)[0];
  $: queued = job && ['queued','waiting','running','cancel_requested'].includes(job.state);
  $: generating = job?.state === 'running';
  $: paused = job?.state === 'waiting' || (generating && job?.progress.phase === 'paused');
  $: failed = job && ['failed','interrupted','cancelled'].includes(job.state);
  async function refresh() {
    if (reading || destroyed || !sessionId) return;
    reading = true;
    const version = generation;
    const session = sessionId;
    try {
      const value = await client.liveAnalysis(session, liveCursor);
      if (!destroyed && version === generation && value.session_id === sessionId) {
        analysis = {...value,artifacts:value.live_artifacts ?? (value.live_cursor && liveCursor === value.live_cursor ? analysis?.artifacts ?? value.artifacts : value.artifacts)};
        liveCursor = value.live_cursor ?? 0; analysisError = '';
      }
    } catch(e) { if (!destroyed && version === generation) analysisError = String(e); }
    finally { reading = false; }
  }
  function updated(next: ArtifactRecord) {
    generation++; liveCursor = 0; // An in-flight poll must not restore the old draft.
    if (analysis) analysis = {...analysis, artifacts:analysis.artifacts.map(a => a.artifact_id === next.artifact_id ? next : a)};
    void refresh();
  }
  async function generate() {
    if (!sessionId || busy || !isTauri()) return;
    const session = sessionId;
    busy = true; actionError = '';
    try {
      const created = await client.createJob({request_id:crypto.randomUUID(),session_ids:[session],kind:'insight',automatic:false});
      if (!destroyed && sessionId === session && analysis) analysis = {...analysis, jobs:[created,...analysis.jobs.filter(j => j.job_id !== created.job_id)]};
    } catch(e) { if (!destroyed) actionError = String(e); }
    finally { if (!destroyed) { busy = false; void refresh(); } }
  }
  onMount(() => {
    void refresh();
    const timer = window.setInterval(() => { if (!document.hidden) void refresh(); },2500);
    return () => { destroyed = true; generation++; window.clearInterval(timer); };
  });
</script>

<section class="live-insights" aria-label={tr('洞察上墙', 'Publish insights')}>
  <header class="insight-heading"><div><h2>{tr('洞察上墙', 'Publish insights')}</h2><p>{tr('核对下方正文，点击“上墙”即批准、发布并打开洞察墙。', 'Review the copy below. Publish to approve it and open the insight wall.')}</p></div><span class="wall-count">{onWall} {tr('组已上墙', 'on the wall')}</span></header>
  <div class="insight-toolbar"><span class="insight-cadence" role="status"><span class:working={queued} class="status-dot"></span>{paused ? tr('字幕优先，稍后继续', 'Captions first; resuming shortly') : queued ? tr('正在整理新要点…', 'Preparing new insights…') : running ? tr('约每 3 分钟更新', 'Updates about every 3 minutes') : tr('本场结果已保留', 'Session insights saved')}</span><button disabled={!sessionId || !isTauri() || busy || !!queued} on:click={generate}>{busy ? tr('正在提交…', 'Submitting…') : queued ? tr('正在生成…', 'Generating…') : failed ? tr('重新生成', 'Retry generation') : tr('立即生成', 'Generate now')}</button></div>
  {#if notice}<p class="insight-feedback" role="status">{notice}</p>{/if}
  <div class="insight-body">
    {#if analysisError}<p class="live-error" role="alert">{tr('更新中断，显示上次结果。', 'Updates interrupted; showing the last result.')} {analysisError}</p>{/if}
    {#if rounds.length}
      {#each (showHistory ? rounds : rounds.slice(0,3)) as round (`${round.artifact_id}:${round.revision}`)}
        <InsightPublication artifact={round} sessionId={sessionId!} on:updated={event => updated(event.detail)} on:notice={event => notice = event.detail}/>
      {/each}
      {#if rounds.length > 3}<button class="expand-notes" on:click={() => showHistory = !showHistory}>{showHistory ? tr('收起较早的洞察', 'Show fewer insights') : tr(`查看较早的 ${rounds.length - 3} 组洞察`, `Show ${rounds.length - 3} earlier rounds`)}</button>{/if}
    {:else}
      <div class="insight-empty"><span aria-hidden="true">✦</span><h3>{generating ? tr('正在整理第一组要点', 'Preparing the first insights') : artifact ? tr('暂时没有通过核验的要点', 'No checked insights yet') : tr('等待本场的第一组要点', 'Waiting for the first insights')}</h3><p>{artifact ? tr('原文仍在保存；下一轮将继续整理。也可以点击“立即生成”。', 'The transcript is being saved. The next round will try again, or use Generate now.') : tr('开始讨论后约 3 分钟生成。要点会显示在这里，再点击“上墙”。', 'Insights appear here about 3 minutes into the discussion. Then click Publish to wall.')}</p></div>
    {/if}
    {#if topics.length}<details class="insight-topics"><summary>{tr('本场主题', 'Session themes')} · {topics.length}</summary><div>{#each topics as topic}<span>{topic.text}</span>{/each}</div></details>{/if}
    {#if job?.error || analysis?.notice || job?.progress.wait_reason}<details class="analysis-notice"><summary>{job?.error ? $t(friendlyAnalysisError(job.error)) : tr('后台运行状态', 'Background status')}</summary><p>{job?.error || analysis?.notice || job?.progress.wait_reason}</p></details>{/if}
    {#if actionError}<p class="live-error" role="alert">{actionError}</p>{/if}
  </div>
  <footer class="insight-sync">{!isTauri() ? tr('界面预览 · 合成数据 · 操作禁用', 'UI preview · Synthetic data · Actions disabled') : tr('仅正文上墙；草稿、来源和操作按钮只在此窗口显示。', 'Only the copy goes on the wall. Drafts, sources and controls stay here.')}</footer>
</section>
