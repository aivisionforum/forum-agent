<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';
  import { getMeetingSessions, getMeetingTranscript, recoverMeeting, isTauri,
    type SessionSummary, type MeetingPage, type TranslationRecord, type RuntimeState } from './lib/api';
  export let running = false;
  export let english = false;
  const dispatch = createEventDispatcher<{ runtime: RuntimeState }>();
  let sessions: SessionSummary[] = [];
  let selected = '';
  let page: MeetingPage | null = null;
  let loading = false;
  let error = '';
  let generation = 0;
  let destroyed = false;
  const tr = (zh: string, en: string) => english ? en : zh;
  const statusName = (value: string) => ({ created: tr('已创建', 'Created'), preparing: tr('准备中', 'Preparing'),
    ready: tr('已就绪', 'Ready'), recording: tr('采音中', 'Recording'), stopping: tr('正在停止', 'Stopping'),
    draining: tr('保存尾句中', 'Draining'), completed: tr('原文已封存', 'Transcript sealed'),
    interrupted: tr('待恢复', 'Interrupted') }[value] ?? value);
  async function refreshSessions() {
    if (!isTauri() || destroyed) return;
    try {
      const result = await getMeetingSessions();
      if (destroyed) return;
      sessions = result;
      if (!selected && result.length) {
        selected = result[0].session.session_id;
        await loadPage();
      }
    } catch (e) { error = String(e); }
  }
  async function loadPage(next = false) {
    if (!selected || loading) return;
    const expected = ++generation;
    const id = selected;
    loading = true;
    error = '';
    try {
      const result = await getMeetingTranscript(id, next ? page?.cursor ?? null : null, next ? page?.next_after ?? null : null);
      if (!destroyed && expected === generation && id === selected) page = result;
    } catch (e) { if (expected === generation) error = String(e); }
    finally { if (expected === generation) loading = false; }
  }
  function translationsFor(id: string): TranslationRecord[] {
    return page?.translations.filter(t => t.request.source_spans[0]?.segment_id === id) ?? [];
  }
  function combinedElsewhere(id: string): boolean {
    return page?.translations.some(t => t.request.source_spans[0]?.segment_id !== id && t.request.source_spans.some(s => s.segment_id === id)) ?? false;
  }
  async function recover() {
    if (!selected || running || loading) return;
    loading = true; error = '';
    try { dispatch('runtime', await recoverMeeting(selected)); }
    catch (e) { error = String(e); }
    finally { loading = false; }
  }
  onMount(() => {
    void refreshSessions();
    const timer = window.setInterval(() => void refreshSessions(), 2000);
    return () => { destroyed = true; generation++; window.clearInterval(timer); };
  });
  $: selectedSummary = sessions.find(s => s.session.session_id === selected);
</script>

<section class="meeting-records" aria-label={tr('本机会议记录', 'Local meeting records')}>
  <header>
    <div><h2>{tr('本机会议记录', 'Local meeting records')}</h2>
      <p>{tr('原文持续保存；恢复操作只读取已有录音，不会打开麦克风。', 'Transcripts are saved continuously. Recovery reads existing recordings without opening the microphone.')}</p></div>
    <button on:click={() => { void refreshSessions(); void loadPage(); }} disabled={loading}>{tr('刷新记录', 'Refresh')}</button>
  </header>
  {#if !isTauri()}
    <p>{tr('界面预览。请在桌面应用中查看本机保存的真实会议。', 'UI preview. Open the desktop app to view saved meetings.')}</p>
  {:else if !sessions.length}
    <p>{tr('开始第一场后，原文和译文会保存在这里。', 'Your first session will appear here after starting.')}</p>
  {:else}
    <div class="meeting-picker">
      <select aria-label={tr('选择会议', 'Select session')} bind:value={selected} disabled={loading} on:change={() => { page = null; void loadPage(); }}>
        {#each sessions as session}
          <option value={session.session.session_id}>{session.session.title} · {statusName(session.status.state)}</option>
        {/each}
      </select>
      <button disabled={running || loading || !selectedSummary || (selectedSummary.status.transcript_sealed && !selectedSummary.status.incomplete && selectedSummary.translation_pending === 0)} on:click={recover}>
        {tr('恢复未完成内容', 'Recover unfinished work')}
      </button>
    </div>
    {#if selectedSummary}
      <p class="record-status">{statusName(selectedSummary.status.state)} · {selectedSummary.segment_count} {tr('段原文', 'segments')} ·
        {selectedSummary.translation_pending} {tr('项译文待处理', 'translations pending')}
        {#if selectedSummary.status.incomplete}<strong> · {tr('含缺口或未完成识别', 'Contains gaps or unfinished recognition')}</strong>{/if}</p>
    {/if}
    {#if page}
      <div class="transcript-items" aria-live="polite">
        {#each page.items as item (item.segment_id)}
          <article>
            <small>{Math.floor(item.audio.start_ms / 60000)}:{String(Math.floor(item.audio.start_ms / 1000) % 60).padStart(2, '0')}</small>
            <div class="caption-text">
              <p class="source">{item.transcript?.payload.status === 'success' ? item.transcript.payload.text
                : item.transcript?.payload.status === 'failed' ? tr('识别失败，录音可用时可以恢复。', 'Recognition failed; recover from the recording if available.')
                : item.transcript?.payload.status === 'empty' ? tr('此段未识别到文字。', 'No text recognized in this segment.')
                : tr('音频段已登记，等待原文保存。', 'Audio registered; awaiting transcript.')}</p>
              {#each translationsFor(item.segment_id) as translation}
                <p class:stale={translation.state === 'stale'} class="translation">
                  <span>{translation.request.target_language.toUpperCase()}</span>
                  {translation.result?.text ?? (translation.state === 'failed' ? tr('翻译失败，可恢复重试', 'Translation failed; retry through recovery') : tr('等待翻译', 'Translation pending'))}
                  {#if translation.state === 'stale'} · {tr('来源或语向已修改，此译文已过期', 'Outdated source or direction')}{/if}
                </p>
              {:else}
                {#if combinedElsewhere(item.segment_id)}<p class="translation">{tr('此段与相邻原文合并翻译。', 'Translated together with the neighboring segment.')}</p>
                {:else if item.transcript?.payload.target_languages.length}<p class="translation">{tr('等待翻译', 'Translation pending')}</p>{/if}
              {/each}
            </div>
          </article>
        {:else}<p>{tr('本场尚未保存原文段。', 'No segments saved in this session yet.')}</p>{/each}
      </div>
      <footer><span>{tr('每页最多 100 段；刷新查看最新内容。', 'Up to 100 segments per page; refresh for the latest content.')}</span>
        <button disabled={!page.next_after || loading} on:click={() => loadPage(true)}>{tr('下一页', 'Next page')}</button></footer>
    {/if}
  {/if}
  {#if error}<p class="record-error" role="alert">{error}</p>{/if}
</section>

<style>
  .meeting-records { margin-top: 24px; border-top: 1px solid #aaa5; padding: 22px 0; }
  header, .meeting-picker, footer { display: flex; align-items: center; justify-content: space-between; gap: 14px; }
  h2 { font-size: 19px; margin: 0; }
  p { margin: 8px 0; line-height: 1.6; }
  header p, .record-status, footer { font-size: 12px; opacity: .7; }
  button, select { font: inherit; padding: 9px 12px; border: 1px solid #aaa6; background: transparent; color: inherit; }
  select { flex: 1; min-width: 0; }
  button { cursor: pointer; white-space: nowrap; }
  button:disabled { opacity: .35; cursor: default; }
  .transcript-items { max-height: 430px; overflow-y: auto; border: 1px solid #aaa4; margin: 14px 0; }
  article { display: flex; align-items: baseline; padding: 12px 16px; border-bottom: 1px solid #aaa3; gap: 18px; }
  small { font-variant-numeric: tabular-nums; opacity: .6; }
  .caption-text { min-width: 0; overflow-wrap: anywhere; white-space: pre-wrap; }
  .source { margin-top: 0; }
  .translation { opacity: .75; }
  .translation span { font-size: 10px; border: 1px solid #aaa5; padding: 2px 5px; margin-right: 6px; }
  .stale { opacity: .45; }
  .record-error { color: #b63b31; }
  @media (max-width: 700px) { header, .meeting-picker { align-items: stretch; flex-direction: column; } }
</style>
