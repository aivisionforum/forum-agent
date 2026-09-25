<script lang="ts">
  import { afterUpdate, onDestroy, onMount } from 'svelte';
  import LiveInsights from './components/LiveInsights.svelte';
  import { captionSpeaker, bilingualCaption } from './lib/forum/live-meeting';
  import { speakerText, type SpeakerAssignment } from './lib/forum/speakers';
  import logoUrl from '../../icons/logo-mark.png';
  import { productName } from './lib/product';
  import { getOverlayState, listenOverlay, startWindowDrag, isTauri, meetingWindowAction, type OverlayState } from './lib/api';
  import { nextChromeTier, type ChromeTier } from './lib/caption-metrics';
  import { captionShare, DEFAULT_CAPTION_SHARE, MEETING_SPLIT_KEY } from './lib/meeting-layout';

  let state: OverlayState | null = null;
  let assignments: SpeakerAssignment[] = [];
  let pinned = false;
  let windowError = '';
  let followLatest = true;
  let lastFeed = '';
  let disposed = false;
  let snapshotVersion = 0;
  $: sessionId = state?.sessionId ?? (!isTauri() && new URLSearchParams(location.search).get('idle') !== '1' ? '00000000-0000-4000-8000-000000000001' : null);
  $: statusText = ({idle:sessionId ? '本场已停止' : '等待开始',warming:'模型准备中',starting:'正在启动',running:'实时转写中',listening:'正在收音',degraded:'部分功能待恢复',stopping:'正在停止',draining:'正在保存尾句',error:'运行异常',failed:'运行失败'}[state?.status ?? 'idle'] ?? state?.status);
  $: history = state ? displayHistory() : [];
  $: captionLabels = new Map((state?.history ?? []).map(sentence => {
    const assigned = captionSpeaker(assignments, sessionId, sentence.segmentId, sentence.sourceRevision);
    return [sentence.segmentId ?? sentence.sourceText, assigned ? speakerText(assigned.label) : ''];
  }));
  async function windowAction(action: 'close' | 'minimize' | 'fullscreen' | 'pin') {
    windowError = '';
    try { await meetingWindowAction(action, !pinned); if (action === 'pin' && isTauri()) pinned = !pinned; }
    catch(e) { windowError = String(e); }
  }
  function pauseFollow(event: WheelEvent) { if (event.deltaY < 0) followLatest = false; }
  function pauseByKey(event: KeyboardEvent) { if (['ArrowUp','PageUp','Home'].includes(event.key)) followLatest = false; }
  function resumeFollow() { followLatest = true; scheduleFollowLatest(); }

  let sentenceFeed: HTMLDivElement;
  let targetFeed: HTMLElement;
  let sourceFeed: HTMLElement;
  let shell: HTMLElement;
  let meetingContent: HTMLDivElement;
  let splitShare = DEFAULT_CAPTION_SHARE;
  let splitWidth = 1180;
  let resizing = false;
  $: effectiveShare = captionShare(splitShare, splitWidth);
  function saveSplit() { try { localStorage.setItem(MEETING_SPLIT_KEY, String(splitShare)); } catch {} }
  function moveSplit(event: PointerEvent) {
    if (!resizing) return;
    const rect = meetingContent.getBoundingClientRect();
    splitShare = captionShare((event.clientX - rect.left - 4) / Math.max(1, rect.width - 8), rect.width);
    scheduleFollowLatest();
  }
  function beginSplit(event: PointerEvent) {
    if (event.button !== 0) return;
    event.preventDefault();
    resizing = true;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
    moveSplit(event);
  }
  function endSplit() { if (resizing) { resizing = false; saveSplit(); } }
  function resetSplit() { splitShare = DEFAULT_CAPTION_SHARE; saveSplit(); scheduleFollowLatest(); }
  function keySplit(event: KeyboardEvent) {
    if (!['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) return;
    event.preventDefault();
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? 1 : effectiveShare + (event.key === 'ArrowLeft' ? -.025 : .025);
    splitShare = captionShare(next, splitWidth); saveSplit(); scheduleFollowLatest();
  }
  let resizeObserver: ResizeObserver | null = null;
  let layoutFrame: number | null = null;
  let scrollFrame: number | null = null;
  let chromeTier: ChromeTier = 0;
  let revealTimer: number | null = null;
  let activeCommitId: number | null = null;
  let receivedTranslation = '';
  let visibleTranslation = '';
  let translationComplete = false;
  let reduceMotion = false;

  function applySnapshot(snapshot: OverlayState): void {
    if (disposed) return;
    snapshotVersion++;
    if (snapshot.sessionId !== state?.sessionId) { assignments = []; followLatest = true; }
    state = snapshot;
    syncTranslatingSentence(snapshot);
    scheduleResponsiveLayout();
  }

  async function refreshOverlay() {
    const version = snapshotVersion;
    try { const next = await getOverlayState(); if (!disposed && version === snapshotVersion) applySnapshot(next); }
    catch(e) { if (!disposed) windowError = String(e); }
  }

  function displayHistory() {
    if (!state) return [];
    const clean = state.history.map(s => state?.targetLanguage === 'bilingual' ? bilingualCaption(s) : s).filter(s => s.sourceText.trim() || s.translation.trim());
    if (!state.translating?.complete || clean.length === 0) return clean;
    const last = clean[clean.length - 1];
    if (
      last.sourceText === state.translating.sourceText
      && last.translation === state.translating.translation
    ) {
      return clean.slice(0, -1);
    }
    return clean;
  }

  function sharedPrefix(left: string, right: string): string {
    const leftUnits = Array.from(left);
    const rightUnits = Array.from(right);
    const length = Math.min(leftUnits.length, rightUnits.length);
    let index = 0;
    while (index < length && leftUnits[index] === rightUnits[index]) index += 1;
    return leftUnits.slice(0, index).join('');
  }

  function clearRevealTimer(): void {
    if (revealTimer !== null) {
      window.clearTimeout(revealTimer);
      revealTimer = null;
    }
  }

  function scheduleReveal(): void {
    if (reduceMotion) {
      visibleTranslation = receivedTranslation;
      return;
    }
    if (revealTimer !== null || visibleTranslation === receivedTranslation) return;
    revealTimer = window.setTimeout(revealNext, 16);
  }

  function revealNext(): void {
    revealTimer = null;
    const target = Array.from(receivedTranslation);
    const visible = Array.from(visibleTranslation);
    const backlog = target.length - visible.length;
    if (backlog <= 0) return;

    const step = translationComplete
      ? Math.max(1, Math.ceil(backlog / 10))
      : backlog > 18
        ? 2
        : 1;
    visibleTranslation = target.slice(0, visible.length + step).join('');

    if (visibleTranslation !== receivedTranslation) {
      const delay = translationComplete ? 16 : backlog > 18 ? 18 : backlog > 8 ? 26 : 36;
      revealTimer = window.setTimeout(revealNext, delay);
    }
  }

  function syncTranslatingSentence(snapshot: OverlayState): void {
    const translating = snapshot.translating;
    if (!translating) {
      clearRevealTimer();
      activeCommitId = null;
      receivedTranslation = '';
      visibleTranslation = '';
      translationComplete = false;
      return;
    }

    if (activeCommitId !== translating.commitId) {
      clearRevealTimer();
      activeCommitId = translating.commitId;
      visibleTranslation = '';
    } else if (!translating.translation.startsWith(visibleTranslation)) {
      visibleTranslation = sharedPrefix(visibleTranslation, translating.translation);
    }

    receivedTranslation = translating.translation;
    translationComplete = translating.complete;
    scheduleReveal();
  }

  function dragWindow(event: MouseEvent): void {
    if (event.button !== 0 || (event.target as HTMLElement).closest('button, select, input')) return;
    void startWindowDrag().catch(e => windowError = String(e));
  }

  function measureResponsiveLayout(): void {
    layoutFrame = null;
    if (!state || !shell) return;
    chromeTier = nextChromeTier(chromeTier, shell.clientHeight, state.fontSize);
  }

  function scheduleResponsiveLayout(): void {
    if (layoutFrame !== null) return;
    layoutFrame = requestAnimationFrame(measureResponsiveLayout);
  }

  function followFeed(feed: HTMLElement | undefined, hasContent: boolean): void {
    if (!feed) return;
    feed.scrollTop = hasContent ? feed.scrollHeight : 0;
  }

  function followNewContent(): void {
    if (state?.targetLanguage === 'none') {
      followFeed(
        sourceFeed,
        Boolean(
          state.history.some((sentence) => sentence.sourceText.trim())
          || state.translating
          || state.pendingSourceText.trim()
        )
      );
      return;
    }

    if (state?.translationOnly) {
      followFeed(
        targetFeed,
        Boolean(
          state
          && (
            state.history.some((sentence) => sentence.translation.trim())
            || state.translating
            || state.pendingSourceText.trim()
          )
        )
      );
      return;
    }

    if (state?.subtitleSplit) {
      followFeed(
        sentenceFeed,
        Boolean(state && (state.history.length > 0 || state.translating || state.pendingSourceText))
      );
      return;
    }

    followFeed(
      targetFeed,
      Boolean(
        state
        && (
          state.history.some((sentence) => sentence.translation.trim())
          || state.translating?.translation.trim()
        )
      )
    );
    followFeed(
      sourceFeed,
      Boolean(
        state
        && (
          state.history.some((sentence) => sentence.sourceText.trim())
          || state.translating
          || state.pendingSourceText.trim()
        )
      )
    );
  }

  function scheduleFollowLatest(): void {
    if (!followLatest) return;
    if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
    scrollFrame = requestAnimationFrame(() => {
      followNewContent();
      scrollFrame = null;
    });
  }

  onMount(() => {
    try { const saved = localStorage.getItem(MEETING_SPLIT_KEY); if (saved !== null && Number.isFinite(Number(saved))) splitShare = Number(saved); } catch {}
    reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    let unlisten: () => void = () => undefined;
    void refreshOverlay();
    void listenOverlay(applySnapshot).then((cleanup) => { if (disposed) cleanup(); else unlisten = cleanup; }).catch(e => windowError = String(e));
    const syncTimer = window.setInterval(() => { if (isTauri() && !document.hidden) void refreshOverlay(); },2500);
    resizeObserver = new ResizeObserver(() => {
      splitWidth = meetingContent?.clientWidth ?? splitWidth;
      scheduleResponsiveLayout();
      scheduleFollowLatest();
    });
    if (shell) resizeObserver.observe(shell);
    if (meetingContent) resizeObserver.observe(meetingContent);
    return () => { disposed = true; unlisten(); window.clearInterval(syncTimer); };
  });

  afterUpdate(() => {
    const content = JSON.stringify([state?.history, state?.pendingSourceText, visibleTranslation]);
    if (lastFeed !== content) { lastFeed = content; scheduleFollowLatest(); }
  });

  onDestroy(() => {
    resizeObserver?.disconnect();
    if (layoutFrame !== null) cancelAnimationFrame(layoutFrame);
    if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
    clearRevealTimer();
  });
</script>

<svelte:head>
  <title>{productName} — 实时会议</title>
</svelte:head>

<svelte:window on:keydown={(e) => { if (e.key === 'Escape') windowError = ''; }} />

<main
  class="overlay-shell chrome-tier-{chromeTier}"
  class:sentence-pairs={state?.subtitleSplit && !state?.translationOnly && state?.targetLanguage !== 'none'}
  class:translation-only={state?.translationOnly && state?.targetLanguage !== 'none'}
  class:source-only={state?.targetLanguage === 'none'}
  style={`--subtitle-size:${state?.fontSize ?? 24}px;--configured-caption-anchor:${state?.anchorPosition ?? 50}%;`}
  bind:this={shell}
>
  <header class="meeting-titlebar">
    <div class="window-controls" aria-label="窗口控制">
      <button aria-label="关闭会议窗口" title="关闭窗口（会议继续运行）" on:click={() => windowAction('close')}>×</button>
      <button aria-label="最小化窗口" title="最小化" on:click={() => windowAction('minimize')}>−</button>
      <button aria-label="切换全屏" title="进入或退出全屏" on:click={() => windowAction('fullscreen')}>⛶</button>
    </div>
    <div class="window-drag-title" data-tauri-drag-region role="presentation" on:mousedown={dragWindow} on:dblclick={() => windowAction('fullscreen')}><span data-tauri-drag-region>{productName}</span><span class="window-subtitle" data-tauri-drag-region>实时会议</span></div>
    <button class="pin-button" class:pinned aria-pressed={pinned} title="保持在其他窗口上方" on:click={() => windowAction('pin')}>{pinned ? '已置顶' : '置顶'}</button>
  </header>
  {#if windowError}<div class="window-error" role="alert">{windowError}</div>{/if}
  <div class="meeting-content" class:resizing bind:this={meetingContent} style={`--caption-share:${effectiveShare}fr;--insight-share:${1-effectiveShare}fr`}>
  <section id="meeting-captions" class="captions-panel" aria-label="实时双语字幕">
    <header class="captions-heading"><div><span class="eyebrow">LIVE TRANSCRIPT</span><h1>实时字幕</h1></div><button class:paused={!followLatest} on:click={resumeFollow}>{followLatest ? '跟随最新 ↓' : '回到最新 ↓'}</button></header>
    <!-- Scrollable captions are keyboard-focusable so readers can pause following with Page Up. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
    <div class="captions-content" on:wheel|passive={pauseFollow} on:keydown={pauseByKey} role="region" aria-label="字幕内容" tabindex="0">
    {#if state && !state.history.length && !state.translating && !state.pendingSourceText}<div class="caption-empty"><h2>等待现场的第一句话</h2><p>开始会议后，原文与译文会持续显示在这里。</p></div>{/if}
  {#if state?.targetLanguage === 'none'}
    <section class="source-only-feed anchor-feed" aria-label="Source language subtitles" bind:this={sourceFeed}>
      <div class="pane-content anchor-track">
        {#each history as sentence}
          {#if sentence.sourceText.trim()}
            <div class="caption-block">{#if captionLabels.get(sentence.segmentId ?? sentence.sourceText)}<span class="caption-speaker">{captionLabels.get(sentence.segmentId ?? sentence.sourceText)}</span>{/if}<p class="pane-line source-only-line">{sentence.sourceText}</p></div>
          {/if}
        {/each}
        {#if state.translating}
          <p class="pane-line source-only-line translating-source">{state.translating.sourceText}</p>
        {/if}
        {#if state.pendingSourceText.trim()}
          <p class="pane-line source-only-line pending-line">{state.pendingSourceText}</p>
        {/if}
      </div>
      <div class="anchor-tail" aria-hidden="true"></div>
    </section>
  {:else if state?.translationOnly}
    <section class="translation-only-feed anchor-feed" aria-label="Translation subtitles" bind:this={targetFeed}>
      <div class="pane-content anchor-track">
        {#each history as sentence}
          {#if sentence.translation.trim()}
            <div class="caption-block">{#if captionLabels.get(sentence.segmentId ?? sentence.sourceText)}<span class="caption-speaker">{captionLabels.get(sentence.segmentId ?? sentence.sourceText)}</span>{/if}<p class="pane-line target-line">{sentence.translation}</p></div>
          {/if}
        {/each}
        {#if state.translating && receivedTranslation.trim()}
          <p class="pane-line target-line translating-line" aria-live="polite">
            <span>{visibleTranslation}</span>
            {#if !state.translating.complete || visibleTranslation !== receivedTranslation}
              <span class="translation-caret" aria-hidden="true"></span>
            {/if}
          </p>
        {:else if state.translating || state.pendingSourceText.trim()}
          <p class="translation-loading" aria-label="Translation pending" aria-live="polite">
            <span aria-hidden="true"></span>
            <span aria-hidden="true"></span>
            <span aria-hidden="true"></span>
          </p>
        {/if}
      </div>
      <div class="anchor-tail" aria-hidden="true"></div>
    </section>
  {:else if state?.subtitleSplit}
    <div class="subtitle-feed anchor-feed" bind:this={sentenceFeed}>
      <div class="subtitle-track anchor-track">
        {#if state.history.length === 0 && !state.translating && !state.pendingSourceText}
          <section class="empty-transcript" aria-label="Empty subtitle window" data-tauri-drag-region>
            <div class="empty-zone target-zone" data-tauri-drag-region></div>
            <div class="empty-zone source-zone" data-tauri-drag-region></div>
          </section>
        {:else}
          {#each history as sentence}
            <article class="subtitle-entry">
              {#if captionLabels.get(sentence.segmentId ?? sentence.sourceText)}<span class="caption-speaker">{captionLabels.get(sentence.segmentId ?? sentence.sourceText)}</span>{/if}
              <p class="translation">{sentence.translation}</p>
              <p class="source">{sentence.sourceText}</p>
            </article>
          {/each}

          {#if state.translating}
            <article class="subtitle-entry translating">
              <p class="translation" aria-live="polite">
                <span>{visibleTranslation}</span>
                {#if !state.translating.complete || visibleTranslation !== receivedTranslation}
                  <span class="translation-caret" aria-hidden="true"></span>
                {/if}
              </p>
              <p class="source">{state.translating.sourceText}</p>
            </article>
          {/if}

          {#if state.pendingSourceText}
            <article class="subtitle-entry pending">
              <p class="translation" aria-label="Translation pending"></p>
              <p class="source">{state.pendingSourceText}</p>
            </article>
          {/if}
        {/if}
      </div>
      <div class="anchor-tail" aria-hidden="true"></div>
    </div>
  {:else}
    <div class="stacked-feed" aria-label="Target language above source language">
      <section class="language-pane target-pane anchor-feed" aria-label="Target language subtitles" bind:this={targetFeed}>
        <div class="pane-content anchor-track">
          {#each history as sentence}
            {#if sentence.translation.trim()}
              <div class="caption-block">{#if captionLabels.get(sentence.segmentId ?? sentence.sourceText)}<span class="caption-speaker">{captionLabels.get(sentence.segmentId ?? sentence.sourceText)}</span>{/if}<p class="pane-line target-line">{sentence.translation}</p></div>
            {/if}
          {/each}
          {#if state?.translating}
            <p class="pane-line target-line translating-line" aria-live="polite">
              <span>{visibleTranslation}</span>
              {#if !state.translating.complete || visibleTranslation !== receivedTranslation}
                <span class="translation-caret" aria-hidden="true"></span>
              {/if}
            </p>
          {/if}
        </div>
        <div class="anchor-tail" aria-hidden="true"></div>
      </section>
      <section class="language-pane source-pane anchor-feed" aria-label="Source language subtitles" bind:this={sourceFeed}>
        <div class="pane-content anchor-track">
          {#each history as sentence}
            {#if sentence.sourceText.trim()}
              <div class="caption-block">{#if captionLabels.get(sentence.segmentId ?? sentence.sourceText)}<span class="caption-speaker">{captionLabels.get(sentence.segmentId ?? sentence.sourceText)}</span>{/if}<p class="pane-line source-line">{sentence.sourceText}</p></div>
            {/if}
          {/each}
          {#if state?.translating}
            <p class="pane-line source-line translating-source">{state.translating.sourceText}</p>
          {/if}
          {#if state?.pendingSourceText.trim()}
            <p class="pane-line source-line pending-line">{state.pendingSourceText}</p>
          {/if}
        </div>
        <div class="anchor-tail" aria-hidden="true"></div>
      </section>
    </div>
  {/if}
    </div>
  </section>
  <!-- A movable window splitter is a focusable ARIA separator with arrow-key controls. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
  <div class="meeting-divider" role="separator" aria-controls="meeting-captions" aria-label="调整字幕与洞察宽度" aria-orientation="vertical" aria-valuemin={Math.round(captionShare(0,splitWidth)*100)} aria-valuemax={Math.round(captionShare(1,splitWidth)*100)} aria-valuenow={Math.round(effectiveShare*100)} aria-valuetext={`字幕 ${Math.round(effectiveShare*100)}%，洞察 ${Math.round((1-effectiveShare)*100)}%`} tabindex="0" title="拖动调整宽度 · 双击恢复 · 方向键微调" on:pointerdown={beginSplit} on:pointermove={moveSplit} on:pointerup={endSplit} on:pointercancel={endSplit} on:lostpointercapture={endSplit} on:keydown={keySplit} on:dblclick={resetSplit}></div>
  {#key sessionId}<LiveInsights {sessionId} running={state?.active ?? false} on:speakers={e => assignments = e.detail}/>{/key}
  </div>
  <footer class="brand-footer">
    <span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span>
    <span class="brand-name">{productName}</span>
    <span class="runtime-status"><span class="status-dot" class:working={state?.active}></span>{isTauri() ? statusText : '界面预览 · 合成数据'}</span>
    <span class="runtime-detail" title={state?.runtimeMessage}>{isTauri() ? state?.runtimeMessage ?? '' : '不采集音频'}</span>
    <span class="brand-tagline">本机私有 · 洞察需审核</span>
  </footer>
</main>
