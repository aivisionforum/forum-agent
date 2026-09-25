<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import { isTauri, showMeetingWorkspace } from '../lib/api';
  import { forumClient as client } from '../lib/forum/factory';
  import { jobLabels, reviewLabels, type AnalysisState } from '../lib/forum/client';
  import { previewSpeakerAssignments } from '../lib/forum/preview';
  import { latestInsight, rollingInsights, meetingTopics, friendlyAnalysisError } from '../lib/forum/live-meeting';
  import { SpeakerClient, type SpeakerAssignment, type SpeakerStatus } from '../lib/forum/speakers';
  export let sessionId: string | null = null;
  export let running = false;
  const dispatch = createEventDispatcher<{ speakers: SpeakerAssignment[] }>();
  const speakers = new SpeakerClient(client.transport);
  let analysis: AnalysisState | null = null;
  let status: SpeakerStatus | null = null;
  let reading = false;
  let busy = false;
  let destroyed = false;
  let analysisError = '';
  let speakerError = '';
  let actionError = '';
  let updatedAt: number | null = null;
  let selectedTopic = '';
  let liveCursor = 0;
  let observedSession: string | null = null;
  $: if (observedSession !== sessionId) {
    observedSession = sessionId; analysis = null; liveCursor = 0; selectedTopic = '';
    updatedAt = null; showAllNotes = false; showAllQuestions = false;
  }
  let showAllNotes = false;
  let showAllQuestions = false;
  $: artifact = latestInsight(analysis, sessionId);
  $: ledger = rollingInsights(analysis,sessionId);
  $: topics = meetingTopics(analysis, sessionId);
  $: if (selectedTopic && !topics.some(t => t.text === selectedTopic)) selectedTopic = '';
  $: selectedIds = topics.find(t => t.text === selectedTopic)?.claimIds;
  $: claims = ledger.filter(c => !selectedIds || selectedIds.includes(c.claim_id));
  $: questions = claims.filter(c => ['question','uncertainty'].includes(c.kind));
  $: notes = claims.filter(c => !['question','uncertainty'].includes(c.kind));
  const claimLabels: Record<string,string> = {fact:'重点',decision:'决定',action:'行动',risk:'风险',question:'问题',uncertainty:'待确认'};
  $: job = analysis?.jobs.filter(j => j.kind === 'insight' && j.session_ids.includes(sessionId ?? ''))
    .sort((a,b) => b.created_at_ms - a.created_at_ms)[0];
  $: queued = job && ['queued','waiting','running','cancel_requested'].includes(job.state);
  $: generating = job?.state === 'running';
  $: paused = generating && job?.progress.phase === 'paused';
  $: failed = job && ['failed','interrupted','cancelled'].includes(job.state);
  $: speakerState = !isTauri() && sessionId ? '示例标签（合成）' : status?.session_id && status.session_id !== sessionId ? '另一场会议正在分析'
    : ({disabled:'标签未启用',preparing:'检查声纹模型',waiting:'标签等待计算资源',running:'正在分析说话人',unavailable:'声纹模型未就绪',stopping:'标签正在停止',error:'标签分析失败',uncontained:'等待分析进程退出',stopped:'标签分析已停止'}[status?.state ?? 'disabled'] ?? status?.state);
  const time = (ms: number) => new Date(ms).toLocaleTimeString('zh-CN',{hour:'2-digit',minute:'2-digit'});
  async function refresh() {
    if (reading || destroyed || !sessionId) return;
    reading = true;
    const results = await Promise.allSettled([
      client.liveAnalysis(sessionId,liveCursor),
      ...(isTauri() ? [Promise.all([speakers.status(), speakers.assignments(sessionId)])] : [])
    ]);
    if (destroyed) return;
    const a = results[0];
    if (a.status === 'fulfilled') {
      const value = a.value as AnalysisState;
      if (value.session_id === sessionId) {
        analysis = {...value,artifacts:value.live_artifacts ?? (value.live_cursor && liveCursor === value.live_cursor ? analysis?.artifacts ?? value.artifacts : value.artifacts)};
        liveCursor = value.live_cursor ?? 0;
        updatedAt = Date.now(); analysisError = '';
      }
    } else analysisError = String(a.reason);
    const s = results[1];
    if (s?.status === 'fulfilled') {
      const [next, assignments] = s.value as [SpeakerStatus, SpeakerAssignment[]];
      status = next; speakerError = ''; dispatch('speakers', assignments);
    } else if (s?.status === 'rejected') { speakerError = String(s.reason); dispatch('speakers', []); }
    reading = false;
  }
  async function act(work: () => Promise<unknown>) {
    if (busy || !isTauri()) return;
    busy = true; actionError = '';
    try { await work(); } catch(e) { if (!destroyed) actionError = String(e); }
    finally { if (!destroyed) { busy = false; void refresh(); } }
  }
  function generate() {
    if (sessionId) void act(async () => {
      const created = await client.createJob({request_id:crypto.randomUUID(),session_ids:[sessionId!],kind:'insight',automatic:false});
      if (!destroyed && analysis) analysis = {...analysis, jobs:[created,...analysis.jobs.filter(j => j.job_id !== created.job_id)]};
    });
  }
  onMount(() => {
    if (!isTauri() && sessionId) dispatch('speakers', previewSpeakerAssignments);
    void refresh();
    const timer = window.setInterval(() => { if (!document.hidden) void refresh(); },2500);
    return () => { destroyed = true; window.clearInterval(timer); };
  });
</script>

<aside class="live-insights" aria-label="实时洞察与运行状态">
  <header class="insight-heading"><div><span class="eyebrow">DISCUSSION NOTES</span><h2>实时洞察</h2></div><span class="draft-badge">操作员私有</span></header>
  <div class="insight-body">
    <div class="insight-cadence"><span class:working={queued} class="status-dot"></span><span>{!isTauri() && sessionId ? '示例洞察 · 待审核' : !sessionId ? '开始会议后自动整理' : paused ? '字幕优先 · 稍后继续' : queued ? jobLabels[job!.state] : running ? '约每 12 秒更新 · 重点持续保留' : '会议已停止 · 保留本场结果'}</span></div>
    {#if analysisError}<p class="live-error" role="alert">洞察更新中断，以下为上次结果。{analysisError}</p>{/if}
    {#if artifact}
      <div class="insight-meta"><span>累计 {ledger.length} 条 · 最近更新</span><time>{isTauri() ? time(artifact.created_at_ms) : '合成示例'}</time></div>
      {#if !artifact.coverage_complete}<p class="live-notice">最近一轮仅覆盖部分原文，已有重点继续保留。</p>{/if}
      {#if topics.length}
        <div class="topics-title"><h3>全场主题</h3><span>从会议开始累计 · 点击主题查看内容</span></div>
        <div class="topic-list" aria-label="全场讨论主题词组">
          {#each topics as topic (topic.text)}
            <button class="topic-phrase" class:chosen={selectedTopic === topic.text} title={`${topic.text} · ${topic.count} 处原文`} aria-pressed={selectedTopic === topic.text} on:click={() => selectedTopic = selectedTopic === topic.text ? '' : topic.text}>{topic.text}</button>
          {/each}
        </div>
      {/if}
      {#if selectedTopic}<button class="clear-topic" on:click={() => selectedTopic = ''}>全部洞察 · 清除“{selectedTopic}”筛选 ×</button>{/if}
      {#if questions.length}
        <section class="question-bubbles"><h3>待追问</h3>{#each (showAllQuestions ? questions : questions.slice(0,4)) as claim (claim.key)}<article class="question-bubble"><span class="question-mark">?</span><p>{claim.text}</p><small><time>{time(claim.lastSeen)}</time> · {claim.firstSeen === artifact.created_at_ms ? '新增' : claim.lastSeen === artifact.created_at_ms ? '延续' : '已保留'} · {reviewLabels[claim.review]} · {claim.grounding === 'cited' ? `${claim.evidence.length} 处引用` : '模型建议 · 尚无引用'}</small></article>{/each}{#if questions.length > 4}<button class="expand-notes" on:click={() => showAllQuestions = !showAllQuestions}>{showAllQuestions ? '收起较早的问题' : `展开其余 ${questions.length-4} 个问题`}</button>{/if}</section>
      {/if}
      <section class="insight-notes"><h3>{selectedTopic ? '相关重点' : '持续重点'}</h3>{#each (showAllNotes ? notes : notes.slice(0,8)) as claim (claim.key)}<article class="note-card"><span class="claim-kind">{claimLabels[claim.kind] ?? '重点'}</span><p>{claim.text}</p><small><time>{time(claim.lastSeen)}</time> · {claim.firstSeen === artifact.created_at_ms ? '新增' : claim.lastSeen === artifact.created_at_ms ? '延续' : '已保留'} · {reviewLabels[claim.review]} · {claim.grounding === 'cited' ? `${claim.evidence.length} 处引用` : '待核验'}{claim.assignee ? ` · ${claim.assignee}` : ''}{claim.due ? ` · ${claim.due}` : ''}</small></article>{/each}{#if notes.length > 8}<button class="expand-notes" on:click={() => showAllNotes = !showAllNotes}>{showAllNotes ? '收起较早的重点' : `展开其余 ${notes.length-8} 条重点`}</button>{/if}{#if !claims.length}<p class="no-claims">{selectedTopic ? '该主题已在原文中出现，相关重点仍在整理。' : '继续听取讨论，新的重点会补充到这里。'}</p>{/if}</section>
    {:else}
      <div class="insight-empty"><span aria-hidden="true">≋</span><h3>{paused ? '已保留生成进度' : generating ? '正在生成这一轮洞察' : queued ? '已收到生成请求' : failed ? '这一轮尚未完成' : '让讨论留下重点'}</h3><p>{paused ? '正在等待字幕处理，空闲后会从当前进度继续。' : generating ? (job!.progress.total_units ? `正在整理第 ${Math.min(job!.progress.completed_units + 1,job!.progress.total_units)} / ${job!.progress.total_units} 组原文，结果会自动显示。` : '正在加载本地模型，结果会自动显示。') : queued ? '任务已排队，运行条件恢复后会自动继续。' : failed ? '原因见下方。点击重新生成，会使用本场最新原文。' : sessionId ? '约每 12 秒整理新增发言，重点会持续积累。也可以立即生成。' : '启动实时会议后，这里会整理要点、共识、分歧和待回答的问题。'}</p></div>
    {/if}
    {#if job?.progress.wait_reason}<p class="live-notice">{job.progress.wait_reason}</p>{/if}
    {#if job?.error}<details class="analysis-notice"><summary>{friendlyAnalysisError(job.error)}</summary><p>{job.error}</p></details>{/if}
    {#if analysis?.notice}<details class="analysis-notice"><summary>后台状态</summary><p>{friendlyAnalysisError(analysis.notice)}</p><small>{analysis.notice}</small></details>{/if}
  </div>
  <div class="insight-actions"><button disabled={!sessionId || !isTauri() || busy || !!queued} on:click={generate}>{busy ? '正在提交…' : paused ? '等待继续…' : generating ? '正在生成…' : queued ? '等待继续…' : failed ? '重新生成' : '立即生成'}</button><button disabled={!isTauri()} on:click={() => act(() => showMeetingWorkspace(sessionId))}>查看与审核 ↗</button></div>
  <div class="speaker-status-inline"><span>匿名标签 · {speakerState}</span><button on:click={() => act(() => showMeetingWorkspace(null,'settings'))}>主页设置 ↗</button></div>
  {#if speakerError}<p class="live-error">标签状态暂不可用</p>{/if}
  {#if actionError}<p class="live-error" role="alert">{actionError}</p>{/if}
  <footer class="insight-sync">{!isTauri() ? '界面预览 · 合成数据 · 操作禁用' : updatedAt ? `状态更新于 ${time(updatedAt)} · 实时翻译优先` : '等待会议数据'}</footer>
</aside>
