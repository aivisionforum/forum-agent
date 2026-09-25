<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import JobList from './components/JobList.svelte';
  import ForumNetwork from './components/ForumNetwork.svelte';
  import SpeakerPanel from './components/SpeakerPanel.svelte';
  import {currentSpeaker,speakerText,type SpeakerAssignment} from './lib/forum/speakers';
  import ArtifactDetail from './components/ArtifactDetail.svelte';
  import { forumClient as client } from './lib/forum/factory';
  import { readLegacyFile } from './lib/forum/import';
  import { SessionFence, evidenceParts, errorText } from './lib/forum/session-fence';
  import { kindLabels, reviewLabels, publicationLabels, type AnalysisKind, type SessionSummary,
    type AnalysisState, type MeetingPage, type AnalysisJob, type ArtifactRecord, type AnalysisEvidence,
    type EvidenceDetail, type ArtifactEdit, type ArtifactReviewCommand, type ArtifactPublishCommand,
    type ArtifactVisibilityCommand, type ExportFormat } from './lib/forum/client';
  import { recoverMeeting, type RuntimeState } from './lib/api';

  export let requestedSession: string | null = null;
  export let requestVersion = 0;
  let handledRequest = 0;
  $: if (requestVersion !== handledRequest) {
    handledRequest = requestVersion;
    if (requestedSession) selectSession(requestedSession);
    tab = 'insights';
  }
  export let running = false;
  export let active = true;
  const dispatch = createEventDispatcher<{ runtime: RuntimeState }>();
  let sessions: SessionSummary[] = [];
  let sessionsAfter: string | null = null;
  let listBusy = false;
  let selected = '';
  let tab: 'transcript' | 'jobs' | 'insights' | 'documents' = 'transcript';
  let transcript: MeetingPage | null = null;
  let transcriptBusy = false;
  let analysis: AnalysisState | null = null;
  let analysisBusy = false;
  let analysisGeneration = 0;
  let afterJobs: string | null = null;
  let afterArtifacts: string | null = null;
  let transcriptPaged = false;
  let actionBusy = false;
  let destroyed = false;
  let error = '';
  let notice = '';
  let createKind: AnalysisKind = 'insight';
  let reportSessions: string[] = [];
  let selectedArtifactId = '';
  let heldArtifact: ArtifactRecord | null = null;
  let artifactGeneration = 0;
  let evidence: AnalysisEvidence | null = null;
  let evidenceDetail: EvidenceDetail | null = null;
  let evidenceBusy = false;
  let evidenceError = '';
  let evidenceGeneration = 0;
  let displayUrl = '';
  let displayExpires = '';
  let savedExportPath = '';
  let importDraft: {title:string;content:string} | null = null;
  let importBusy = false;
  let importError = '';
  let importGeneration = 0;
  let speakerAssignments:SpeakerAssignment[]=[];
  let artifactView: ArtifactDetail | undefined;
  const fence = new SessionFence();
  const writable = client.mode === 'desktop';
  const sessionStatus: Record<string,string> = { created:'已创建',preparing:'准备中',ready:'已就绪',recording:'进行中',stopping:'停止中',draining:'保存尾句',completed:'原文已封存',interrupted:'待恢复' };
  $: summary = sessions.find(s => s.session.session_id === selected);
  $: artifacts = (analysis?.artifacts ?? []).filter(a => tab === 'insights'
    ? ['insight','suggested_questions','redaction_review'].includes(a.kind) : ['minutes','event_report','closing_brief'].includes(a.kind));
  $: {
    const matching = artifacts.find(a => a.artifact_id === selectedArtifactId);
    if (matching && (!heldArtifact || heldArtifact.artifact_id !== matching.artifact_id || matching.revision >= heldArtifact.revision)) heldArtifact = matching;
    else if ((!heldArtifact || !(tab === 'insights' ? ['insight','suggested_questions','redaction_review'] : ['minutes','event_report','closing_brief']).includes(heldArtifact.kind)) && ['insights','documents'].includes(tab)) {
      heldArtifact = artifacts[0] ?? null; selectedArtifactId = heldArtifact?.artifact_id ?? '';
    }
  }
  $: selectedArtifact = heldArtifact;
  $: evidenceSpan = evidence?.kind === 'source' ? evidence.span : evidence?.kind === 'artifact' ? evidence : null;
  $: highlighted = evidenceDetail && evidenceSpan && evidenceDetail.current
    ? evidenceParts(evidenceDetail.text,evidenceSpan.start_utf8,evidenceSpan.end_utf8,evidenceSpan.quote) : null;

  async function loadSessions(next = false) {
    if (listBusy || destroyed) return;
    listBusy = true;
    try {
      const result = await client.sessions(next ? sessionsAfter : null);
      if (destroyed) return;
      sessions = next ? [...new Map([...sessions,...result.items].map(s => [s.session.session_id,s])).values()] : result.items;
      sessionsAfter = result.next_after;
      if (!selected && sessions.length) { selectSession(sessions[0].session.session_id); }
    } catch (e) { if (!destroyed) error = errorText(e); }
    finally { if (!destroyed) listBusy = false; }
  }
  function selectSession(id: string) {
    selected = id; speakerAssignments=[]; fence.select(id); transcript = null; analysis = null; error = ''; notice = '';
    transcriptBusy = false; analysisBusy = false; actionBusy = false; transcriptPaged = false;
    analysisGeneration++; afterJobs = null; afterArtifacts = null; selectedArtifactId = ''; heldArtifact = null; artifactGeneration++; displayUrl = ''; displayExpires = ''; savedExportPath = '';
    closeEvidence(); reportSessions = [id];
    void loadTranscript(); void loadAnalysis();
  }
  async function loadTranscript(next = false) {
    if (!selected || transcriptBusy || destroyed) return;
    const current = fence.capture(); const sessionId = selected;
    const cursor = next ? transcript?.cursor ?? null : null;
    const after = next ? transcript?.next_after ?? null : null;
    transcriptBusy = true;
    try {
      const result = await client.transcript(sessionId,cursor,after);
      if (current() && result.session.session_id === sessionId) { transcript = result; transcriptPaged = next; }
    } catch (e) { if (current()) error = errorText(e); }
    finally { if (current()) transcriptBusy = false; }
  }
  async function loadAnalysis(next: 'jobs' | 'artifacts' | null = null) {
    if (!selected || analysisBusy || destroyed) return;
    const current = fence.capture(); const sessionId = selected;
    const jobAfter = next === 'jobs' ? analysis?.next_jobs ?? null : afterJobs;
    const artifactAfter = next === 'artifacts' ? analysis?.next_artifacts ?? null : afterArtifacts;
    analysisBusy = true;
    const generation = ++analysisGeneration;
    try {
      const result = await client.analysis(sessionId,jobAfter,artifactAfter);
      if (current() && generation === analysisGeneration && result.session_id === sessionId && (jobAfter !== null || artifactAfter !== null || result.cursor >= (analysis?.cursor ?? 0))) { analysis = result; afterJobs = jobAfter; afterArtifacts = artifactAfter; }
    } catch (e) { if (current()) error = errorText(e); }
    finally { if (current() && generation === analysisGeneration) analysisBusy = false; }
  }
  async function mutate<T>(work: () => Promise<T>, success: (value:T) => void) {
    if (!writable || actionBusy) return;
    const current = fence.capture(); actionBusy = true; error = ''; notice = '';
    try { const result = await work(); if (current()) { analysisGeneration++; analysisBusy = false; success(result); void loadAnalysis(); } }
    catch (e) { if (current()) error = errorText(e); }
    finally { if (current()) actionBusy = false; }
  }
  function createJob() {
    const sessionIds = createKind === 'event_report' ? [...reportSessions] : [selected];
    if (!sessionIds.length) return;
    const request = { request_id:crypto.randomUUID(),session_ids:sessionIds,kind:createKind,automatic:false };
    void mutate(() => client.createJob(request), () => { tab = 'jobs'; afterJobs = null; notice = '任务已入队，完成后将生成待审核草稿。'; });
  }
  function jobAction(job: AnalysisJob, action: 'cancel' | 'retry') {
    const request = { job_id:job.job_id,expected_attempt:job.attempt };
    void mutate(() => action === 'cancel' ? client.cancelJob(request) : client.retryJob(request), result => {
      if (analysis) analysis = {...analysis,jobs:analysis.jobs.map(j => j.job_id === result.job_id ? result : j)};
      notice = action === 'cancel' ? '已提交取消，等待工作进程停止。' : '已请求重试，状态将持续更新。';
    });
  }
  function acceptArtifact(result: ArtifactRecord, action: string) {
    if (heldArtifact?.artifact_id === result.artifact_id) heldArtifact = result;
    if (analysis) analysis = {...analysis,artifacts:analysis.artifacts.map(a => a.artifact_id === result.artifact_id ? result : a)};
    if (selectedArtifactId === result.artifact_id || selectedArtifact?.artifact_id === result.artifact_id) artifactView?.completeAction(action);
    notice = action === 'edit' ? '已保存新版本，请重新审核。' : action === 'publish' ? '公开版本已发布。' : action === 'hide' ? '已从公开投影隐藏。' : '审核状态已保存。';
  }
  function edit(request: ArtifactEdit) { void mutate(() => client.editArtifact(request), r => acceptArtifact(r,'edit')); }
  function review(request: ArtifactReviewCommand) { void mutate(() => client.reviewArtifact(request), r => acceptArtifact(r,'review')); }
  function publish(request: ArtifactPublishCommand) { void mutate(() => client.publishArtifact(request), r => acceptArtifact(r,'publish')); }
  function hide(request: ArtifactVisibilityCommand) { void mutate(() => client.hideArtifact(request), r => acceptArtifact(r,'hide')); }
  async function showJobResult(job: AnalysisJob) {
    if (!job.result) return;
    const current = fence.capture(); const generation = ++artifactGeneration;
    error = '';
    try {
      const record = await client.artifact(job.result.artifact_id,job.result.revision);
      if (!current() || generation !== artifactGeneration) return;
      if (!record || !record.session_ids.includes(selected)) throw new Error('此任务结果不可用或不属于当前会议。');
      heldArtifact = record; selectedArtifactId = record.artifact_id;
      tab = ['insight','suggested_questions','redaction_review'].includes(job.kind) ? 'insights' : 'documents';
      afterArtifacts = null; void loadAnalysis();
    } catch(e) { if (current() && generation === artifactGeneration) error = errorText(e); }
  }
  async function showEvidence(value: AnalysisEvidence) {
    evidence = value; evidenceDetail = null; evidenceError = ''; evidenceBusy = true;
    const generation = ++evidenceGeneration; const current = fence.capture();
    try { const result = await client.evidence(value); if (current() && generation === evidenceGeneration) evidenceDetail = result; }
    catch (e) { if (current() && generation === evidenceGeneration) evidenceError = errorText(e); }
    finally { if (current() && generation === evidenceGeneration) evidenceBusy = false; }
  }
  function closeEvidence() { evidenceGeneration++; evidence = null; evidenceDetail = null; evidenceBusy = false; evidenceError = ''; }
  function download(format: ExportFormat) {
    if (!selectedArtifact) return;
    const a = selectedArtifact;
    void mutate(() => client.exportArtifact(a.artifact_id,a.revision,format), result => {
      const url = URL.createObjectURL(new Blob([result.content],{type:result.mime_type}));
      const anchor = document.createElement('a'); anchor.href = url; anchor.download = result.filename; anchor.click();
      window.setTimeout(() => URL.revokeObjectURL(url),1000); savedExportPath = result.saved_path ?? ''; notice = result.saved_path ? '已导出并保存到本机。' : `已准备导出 ${result.filename}`;
    });
  }
  async function chooseImport(event: Event) {
    const input = event.currentTarget as HTMLInputElement; const file = input.files?.[0]; input.value = '';
    if (!file || !writable || importBusy) return;
    const generation = ++importGeneration; importError = '';
    try { const result = await readLegacyFile(file); if (!destroyed && generation === importGeneration) importDraft = result; }
    catch(e) { if (!destroyed && generation === importGeneration) importError = errorText(e); }
  }
  async function importLegacy() {
    if (!writable || importBusy || !importDraft) return;
    importBusy = true; importError = ''; const current = fence.capture();
    try {
      const result = await client.importLegacy({title:importDraft.title.trim(),content:importDraft.content});
      if (destroyed) return;
      importDraft = null; sessions = [result,...sessions.filter(s => s.session.session_id !== result.session.session_id)];
      if (current()) selectSession(result.session.session_id);
      notice = '旧文字记录已导入为独立会议；未导入任何录音。';
    } catch(e) { if (!destroyed) importError = errorText(e); }
    finally { if (!destroyed) importBusy = false; }
  }
  function openDisplay() { void mutate(() => client.displayInfo(selected), info => { displayUrl = info.url; displayExpires = info.expiresAt; }); }
  function recover() { void mutate(() => recoverMeeting(selected), result => dispatch('runtime',result)); }
  function translationsFor(id:string) { return transcript?.translations.filter(t => t.request.source_spans[0]?.segment_id === id) ?? []; }
  function combinedElsewhere(id:string) { return transcript?.translations.some(t => t.request.source_spans[0]?.segment_id !== id && t.request.source_spans.some(s => s.segment_id === id)) ?? false; }
  const time = (ms:number) => `${Math.floor(ms/60000)}:${String(Math.floor(ms/1000)%60).padStart(2,'0')}`;
  onMount(() => {
    void loadSessions();
    const timer = window.setInterval(() => {
      if (!active) return;
      if (!selected) { void loadSessions(); return; }
      if (tab === 'transcript' && !transcriptPaged) void loadTranscript();
      if (tab !== 'transcript' && !afterJobs && !afterArtifacts) void loadAnalysis();
    },2500);
    return () => { destroyed = true; fence.dispose(); closeEvidence(); window.clearInterval(timer); };
  });
</script>

<section class="forum-workspace" aria-label="会议工作台">
  <header class="workspace-heading"><div><p class="eyebrow">FORUM WORKSPACE</p><h2>会议工作台</h2><p>从实时记录，到有出处、可审核的会议成果。</p></div><span class="private-tag">{writable ? '本机私有操作台' : '界面预览 · 合成数据 · 操作禁用'}</span></header>
  {#if summary}{#key selected}<ForumNetwork sessionId={selected} eventId={summary.session.event_id} {running} {active} on:created={() => {tab='jobs';afterJobs=null;void loadAnalysis();}}/>{/key}{/if}
  <div class="workspace-grid">
    <aside class="session-library">
      <div class="library-heading"><h3>会议库</h3><button aria-label="刷新会议库" disabled={listBusy} on:click={() => loadSessions()}>↻</button></div>
      <div class="session-list">{#each sessions as item (item.session.session_id)}<button class:selected={selected === item.session.session_id} on:click={() => selectSession(item.session.session_id)}><strong>{item.session.title}</strong><span>{sessionStatus[item.status.state] ?? item.status.state} · {item.segment_count} 段</span>{#if item.status.incomplete || item.translation_pending}<small>{item.translation_pending} 项译文待处理{item.status.incomplete ? ' · 含未完成内容' : ''}</small>{/if}</button>{:else}<p class="library-empty">{listBusy ? '正在读取…' : '开始第一场后，记录会出现在这里。'}</p>{/each}</div>
      {#if sessionsAfter}<button class="load-more" disabled={listBusy} on:click={() => loadSessions(true)}>载入更早的会议</button>{/if}
      <p class="library-note">每次最多 30 场 · 保存在本机</p><label class="legacy-file"><span>＋ 导入旧 JSONL</span><input type="file" accept=".jsonl,.ndjson,.json,.txt" disabled={!writable || importBusy} on:change={chooseImport} /></label><p class="library-note">只读原文件 · 最多 16 MiB<br />仅文字，不包含原有录音</p>
    </aside>
    <div class="workspace-body">
      {#if importError}<p class="message error" role="alert">{importError}</p>{/if}
      {#if importDraft}<section class="import-panel"><h3>导入旧文字记录</h3><label>新会议名称<input bind:value={importDraft.title} maxlength="180" /></label><p>内容将作为独立历史会议保存，原文件保持不变。不会推断为有录音，也不会自动批准或发布任何内容。</p><pre>{importDraft.content.slice(0,1600)}{importDraft.content.length > 1600 ? '\n…（预览已截断，实际导入完整文件）' : ''}</pre><div><button disabled={importBusy || !importDraft.title.trim()} on:click={importLegacy}>{importBusy ? '正在导入…' : '导入为新会议'}</button><button disabled={importBusy} on:click={() => { importGeneration++; importDraft=null; }}>取消</button></div></section>{/if}
      {#if selected}
        <div class="selected-heading"><div><h3>{summary?.session.title ?? '所选会议'}</h3><span>{summary ? sessionStatus[summary.status.state] ?? summary.status.state : ''}</span></div><button disabled={!writable || actionBusy} on:click={openDisplay}>打开只读大屏 ↗</button></div>
        {#if displayUrl}<div class="display-link"><a href={displayUrl} target="_blank" rel="noopener noreferrer">点击打开本场大屏 ↗</a><span>只显示人工发布内容 · 有效至 {new Date(displayExpires).toLocaleTimeString()}</span></div>{/if}
        <nav class="workspace-tabs" aria-label="会议内容"><button class:active={tab === 'transcript'} on:click={() => { tab = 'transcript'; void loadTranscript(); }}>原文与译文</button><button class:active={tab === 'jobs'} on:click={() => { tab = 'jobs'; void loadAnalysis(); }}>分析任务</button><button class:active={tab === 'insights'} on:click={() => { tab = 'insights'; void loadAnalysis(); }}>洞察与审核</button><button class:active={tab === 'documents'} on:click={() => { tab = 'documents'; void loadAnalysis(); }}>纪要与报告</button></nav>
        <div class="create-bar"><label><span>生成会议成果</span><select bind:value={createKind}>{#each Object.entries(kindLabels).filter(([kind])=>kind!=='closing_brief') as [kind,label]}<option value={kind}>{label}</option>{/each}</select></label><button class="primary" disabled={!writable || actionBusy || (createKind === 'event_report' && reportSessions.length === 0)} on:click={createJob}>＋ 创建任务</button><p>约每 30 秒更新洞察并保留已有重点，停止后自动排队生成纪要；实时翻译优先。</p></div>
        {#if createKind === 'event_report'}<fieldset class="report-picker"><legend>明确选择报告来源（仅使用已发布资料）</legend>{#each sessions.filter(s => s.session.event_id === summary?.session.event_id) as item}<label><input type="checkbox" bind:group={reportSessions} value={item.session.session_id} />{item.session.title}</label>{/each}<small>没有已发布纪要或洞察时，不会自动回退为草稿。</small></fieldset>{/if}
        {#if analysis?.notice}<p class="message error" role="status">{analysis.notice}</p>{/if}
        {#if error}<div class="message error" role="alert">{error}<button on:click={() => error = ''} aria-label="关闭错误提示">×</button></div>{/if}
        {#if notice}<p class="message" role="status">{notice}</p>{/if}
        {#if savedExportPath}<p class="saved-export">已保存：<code>{savedExportPath}</code></p>{/if}
        {#if tab === 'transcript'}
          <div class="content-toolbar"><p>原文持续保存；恢复只读取已有录音，不打开音频设备。</p><button disabled={!writable || running || actionBusy || (summary?.status.transcript_sealed && !summary?.status.incomplete && summary?.translation_pending === 0)} on:click={recover}>恢复未完成内容</button></div>
          {#key selected}<SpeakerPanel sessionId={selected} {transcript} {active} on:assignments={e=>speakerAssignments=e.detail}/>{/key}
          <div class="transcript-list" aria-busy={transcriptBusy}>
            {#each transcript?.items ?? [] as item (item.segment_id)}{@const assigned=currentSpeaker(speakerAssignments,selected,item.segment_id,item.transcript?.payload.revision??null)}<article><span class="timestamp">{time(item.audio.start_ms)}</span><div>{#if assigned}<span class="speaker-label">{speakerText(assigned.label)}</span>{/if}<p class="source">{item.transcript?.payload.status === 'success' ? item.transcript.payload.text : item.transcript?.payload.status === 'failed' ? '识别失败，可从录音恢复。' : item.transcript?.payload.status === 'empty' ? '此段未识别到文字。' : '音频已登记，等待原文保存。'}</p>
              {#each translationsFor(item.segment_id) as translation}<p class:stale={translation.state === 'stale'} class="translation"><span>{translation.request.target_language.toUpperCase()}</span>{translation.result?.text ?? (translation.state === 'failed' ? '翻译失败，可恢复重试' : '等待翻译')}{translation.state === 'stale' ? ' · 来源版本已变更' : ''}</p>{:else}{#if combinedElsewhere(item.segment_id)}<p class="translation">此段与相邻原文合并翻译。</p>{:else if item.transcript?.payload.target_languages.length}<p class="translation">等待翻译</p>{/if}{/each}
            </div></article>{:else}<div class="empty-state"><h3>{transcriptBusy ? '读取原文…' : '本场尚无原文'}</h3><p>真实原文和译文保存后显示在这里。</p></div>{/each}
          </div><div class="pagination"><span>每页最多 100 段 · {transcriptPaged ? '历史分页，自动刷新暂停' : '首屏自动刷新'}</span><button disabled={transcriptBusy} on:click={() => loadTranscript()}>返回首屏</button><button disabled={transcriptBusy || !transcript?.next_after} on:click={() => loadTranscript(true)}>下一页 →</button></div>
        {:else if tab === 'jobs'}
          <JobList jobs={analysis?.jobs ?? []} busy={actionBusy || !writable} on:cancel={e => jobAction(e.detail,'cancel')} on:retry={e => jobAction(e.detail,'retry')} on:result={e => showJobResult(e.detail)} />
          <div class="pagination"><span>{analysisBusy ? '正在更新任务…' : afterJobs ? '历史任务页' : '任务状态自动更新'}</span><button disabled={analysisBusy} on:click={() => {afterJobs=null;void loadAnalysis();}}>最新任务</button><button disabled={analysisBusy || !analysis?.next_jobs} on:click={() => loadAnalysis('jobs')}>下一页 →</button></div>
        {:else}
          {#if artifacts.length}<div class="artifact-picker"><label>选择版本成果<select bind:value={selectedArtifactId} on:change={() => artifactGeneration++}>{#if heldArtifact && !artifacts.some(a => a.artifact_id === heldArtifact?.artifact_id)}<option value={heldArtifact.artifact_id}>{heldArtifact.content.title} · v{heldArtifact.revision} · 当前查看</option>{/if}{#each artifacts as artifact}<option value={artifact.artifact_id}>{artifact.content.title} · v{artifact.revision} · {reviewLabels[artifact.review]} · {publicationLabels[artifact.publication]}</option>{/each}</select></label></div>{/if}
          {#if selectedArtifact}{#key selectedArtifact.artifact_id}<ArtifactDetail bind:this={artifactView} artifact={selectedArtifact} busy={actionBusy || !writable} on:edit={e => edit(e.detail)} on:review={e => review(e.detail)} on:publish={e => publish(e.detail)} on:hide={e => hide(e.detail)} on:evidence={e => showEvidence(e.detail)} on:export={e => download(e.detail)} />{/key}
          {:else}<div class="empty-state"><h3>{tab === 'insights' ? '让讨论形成可核查的洞察' : '把整场讨论整理成完整纪要'}</h3><p>从上方创建任务。模型完成后先生成私有草稿，审核后才能公开。</p></div>{/if}
          <div class="pagination"><span>{analysisBusy ? '正在更新…' : afterArtifacts ? '历史成果页' : '显示当前成果'}</span><button disabled={analysisBusy} on:click={() => {afterArtifacts=null;void loadAnalysis();}}>最新成果</button><button disabled={analysisBusy || !analysis?.next_artifacts} on:click={() => loadAnalysis('artifacts')}>下一页 →</button></div>
        {/if}
      {:else}<div class="empty-state intro"><span class="intro-number">01 / FORUM</span><h3>每场讨论，都留下清楚的依据</h3><p>先在实时控制中开始一场会议。原文、双语译文、分析任务和审核版本会统一保存在这里。</p><div><span>持续记录</span><span>引用核查</span><span>人工发布</span></div></div>{/if}
    </div>
  </div>
</section>
{#if evidence}<div class="evidence-backdrop"><dialog open class="evidence-modal" aria-modal="true" aria-label="引用定位"><header><h2>引用定位</h2><button on:click={closeEvidence} aria-label="关闭引用">×</button></header><p class="evidence-meta">{evidence.kind === 'source' ? `原文 · v${evidence.span.segment_revision}` : `已发布资料 · v${evidence.revision}`}{evidenceDetail?.start_ms !== undefined ? ` · ${time(evidenceDetail.start_ms)}` : ''}</p>{#if evidenceBusy}<p>正在读取引用…</p>{:else if evidenceError}<p class="message error">{evidenceError}</p>{:else if evidenceDetail}<p class="evidence-meta">{evidenceDetail.current ? '来源版本当前有效' : '来源已变更或撤回，不能按当前有效引用发布'}</p>{#if highlighted}<p class="evidence-text">{highlighted.before}<mark>{highlighted.quote}</mark>{highlighted.after}</p>{:else}<p class="evidence-text">{evidenceDetail.text}</p><p class="message error">无法在当前来源中精确定位此引文。请检查版本和引用范围。</p>{/if}{/if}</dialog></div>{/if}
