<script lang="ts">
  import AudienceTitlebar from './components/AudienceTitlebar.svelte';
  import { t, uiLocale, initializeLocale, setLocale } from './lib/i18n';
  import { afterUpdate, onDestroy, onMount } from 'svelte';
  import { bilingualCaption } from './lib/forum/live-meeting';
  import { productName, brandLogoUrl as logoUrl } from './lib/product';
  import { getOverlayState, listenOverlay, startWindowDrag, isTauri, meetingWindowAction, type OverlayState } from './lib/api';
  import { nextChromeTier, type ChromeTier } from './lib/caption-metrics';

  initializeLocale();
  let state: OverlayState | null = null;
  let pinned = false;
  let windowError = '';
  let followLatest = true;
  let lastFeed = '';
  let disposed = false;
  let snapshotVersion = 0;
  $: history = state ? displayHistory() : [];
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
    if (snapshot.sessionId !== state?.sessionId) { followLatest = true; }
    state = snapshot;
    if (isTauri() && snapshot.appLanguage) setLocale(snapshot.appLanguage);
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

    if ((state?.subtitleSplit && !state?.subtitleSideBySide)) {
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
    reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    let unlisten: () => void = () => undefined;
    void refreshOverlay();
    void listenOverlay(applySnapshot).then((cleanup) => { if (disposed) cleanup(); else unlisten = cleanup; }).catch(e => windowError = String(e));
    const syncTimer = window.setInterval(() => { if (isTauri() && !document.hidden) void refreshOverlay(); },2500);
    resizeObserver = new ResizeObserver(() => {
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
  <title>{productName} {$t("— 公开字幕")}</title>
</svelte:head>

<svelte:window on:keydown={(e) => { if (e.key === 'Escape') windowError = ''; }} />

<main
  class="overlay-shell chrome-tier-{chromeTier}"
  class:sentence-pairs={(state?.subtitleSplit && !state?.subtitleSideBySide) && !state?.translationOnly && state?.targetLanguage !== 'none'}
  class:translation-only={state?.translationOnly && state?.targetLanguage !== 'none'}
  class:source-only={state?.targetLanguage === 'none'}
  style={`--subtitle-size:${state?.fontSize ?? 24}px;--configured-caption-anchor:${state?.anchorPosition ?? 50}%;`}
  bind:this={shell}
>
  <AudienceTitlebar subtitle={$t("公开字幕 · 屏幕 1")} {pinned} action={windowAction} drag={dragWindow}/>
  {#if windowError}<div class="window-error" role="alert">{$t("窗口暂不可用，请操作员检查")}</div>{/if}
  <div class="meeting-content" bind:this={meetingContent}>
  <section id="meeting-captions" class="captions-panel" aria-label={$t("实时双语字幕")}>
    <header class="captions-heading audience-heading"><div><span class="eyebrow">LIVE TRANSCRIPT</span><h1>{$t("实时字幕")}</h1></div><button class:paused={!followLatest} on:click={resumeFollow}>{followLatest ? $t("跟随最新 ↓") : $t("回到最新 ↓")}</button></header>
    <!-- Scrollable captions are keyboard-focusable so readers can pause following with Page Up. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex a11y_no_noninteractive_element_interactions -->
    <div class="captions-content" on:wheel|passive={pauseFollow} on:keydown={pauseByKey} role="region" aria-label={$t("字幕内容")} tabindex="0">
    {#if state && !state.history.length && !state.translating && !state.pendingSourceText}<div class="caption-empty"><h2>{$t("等待现场的第一句话")}</h2><p>{$t("开始会议后，原文与译文会持续显示在这里。")}</p></div>{/if}
  {#if state?.targetLanguage === 'none'}
    <section class="source-only-feed anchor-feed" aria-label={$t("原文字幕")} bind:this={sourceFeed}>
      <div class="pane-content anchor-track">
        {#each history as sentence}
          {#if sentence.sourceText.trim()}
            <div class="caption-block"><p class="pane-line source-only-line">{sentence.sourceText}</p></div>
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
    <section class="translation-only-feed anchor-feed" aria-label={$t("译文字幕")} bind:this={targetFeed}>
      <div class="pane-content anchor-track">
        {#each history as sentence}
          {#if sentence.translation.trim()}
            <div class="caption-block"><p class="pane-line target-line">{sentence.translation}</p></div>
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
          <p class="translation-loading" aria-label={$t("等待译文")} aria-live="polite">
            <span aria-hidden="true"></span>
            <span aria-hidden="true"></span>
            <span aria-hidden="true"></span>
          </p>
        {/if}
      </div>
      <div class="anchor-tail" aria-hidden="true"></div>
    </section>
  {:else if (state?.subtitleSplit && !state?.subtitleSideBySide)}
    <div class="subtitle-feed anchor-feed" bind:this={sentenceFeed}>
      <div class="subtitle-track anchor-track">
        {#if state.history.length === 0 && !state.translating && !state.pendingSourceText}
          <section class="empty-transcript" aria-label={$t("空字幕窗口")} data-tauri-drag-region>
            <div class="empty-zone target-zone" data-tauri-drag-region></div>
            <div class="empty-zone source-zone" data-tauri-drag-region></div>
          </section>
        {:else}
          {#each history as sentence}
            <article class="subtitle-entry">

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
              <p class="translation" aria-label={$t("等待译文")}></p>
              <p class="source">{state.pendingSourceText}</p>
            </article>
          {/if}
        {/if}
      </div>
      <div class="anchor-tail" aria-hidden="true"></div>
    </div>
  {:else}
    <div class="stacked-feed" class:side-by-side={state?.subtitleSideBySide} aria-label={state?.subtitleSideBySide ? ($uiLocale === 'en' ? 'Translation on the left, original on the right' : '译文在左，原文在右') : $t("译文在上，原文在下")}>
      <section class="language-pane target-pane anchor-feed" aria-label={$t("目标语言字幕")} bind:this={targetFeed}>
        <div class="pane-content anchor-track">
          {#each history as sentence}
            {#if sentence.translation.trim()}
              <div class="caption-block"><p class="pane-line target-line">{sentence.translation}</p></div>
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
      <section class="language-pane source-pane anchor-feed" aria-label={$t("原文字幕")} bind:this={sourceFeed}>
        <div class="pane-content anchor-track">
          {#each history as sentence}
            {#if sentence.sourceText.trim()}
              <div class="caption-block"><p class="pane-line source-line">{sentence.sourceText}</p></div>
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
  </div>
  <footer class="brand-footer audience-footer">
    <span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span>
    <span class="brand-name">{productName}</span>
    <span class="brand-tagline">{$t("公开字幕 · 屏幕 1")}</span>
  </footer>
</main>
