<script lang="ts">
  import { afterUpdate, onDestroy, onMount } from 'svelte';
  import logoUrl from '../../icons/logo-mark.png';
  import { productName } from './lib/product';
  import { getOverlayState, listenOverlay, startWindowDrag, type OverlayState } from './lib/api';
  import { nextChromeTier, type ChromeTier } from './lib/caption-metrics';

  let state: OverlayState | null = null;
  let sentenceFeed: HTMLDivElement;
  let targetFeed: HTMLElement;
  let sourceFeed: HTMLElement;
  let shell: HTMLElement;
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
    state = snapshot;
    syncTranslatingSentence(snapshot);
    scheduleResponsiveLayout();
  }

  function displayHistory() {
    if (!state?.translating?.complete || state.history.length === 0) return state?.history ?? [];
    const last = state.history[state.history.length - 1];
    if (
      last.sourceText === state.translating.sourceText
      && last.translation === state.translating.translation
    ) {
      return state.history.slice(0, -1);
    }
    return state.history;
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
    void startWindowDrag();
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
    if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
    scrollFrame = requestAnimationFrame(() => {
      followNewContent();
      scrollFrame = null;
    });
  }

  onMount(() => {
    reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
    let unlisten: () => void = () => undefined;
    void getOverlayState().then(applySnapshot);
    void listenOverlay(applySnapshot).then((cleanup) => { unlisten = cleanup; });
    resizeObserver = new ResizeObserver(() => {
      scheduleResponsiveLayout();
      scheduleFollowLatest();
    });
    if (shell) resizeObserver.observe(shell);
    return () => unlisten();
  });

  afterUpdate(() => {
    scheduleFollowLatest();
  });

  onDestroy(() => {
    resizeObserver?.disconnect();
    if (layoutFrame !== null) cancelAnimationFrame(layoutFrame);
    if (scrollFrame !== null) cancelAnimationFrame(scrollFrame);
    clearRevealTimer();
  });
</script>

<svelte:head>
  <title>{productName} — 操作员字幕</title>
</svelte:head>

<svelte:window on:mousedown={dragWindow} />

<main
  class="overlay-shell chrome-tier-{chromeTier}"
  class:sentence-pairs={state?.subtitleSplit && !state?.translationOnly && state?.targetLanguage !== 'none'}
  class:translation-only={state?.translationOnly && state?.targetLanguage !== 'none'}
  class:source-only={state?.targetLanguage === 'none'}
  style={`--subtitle-size:${state?.fontSize ?? 24}px;--configured-caption-anchor:${state?.anchorPosition ?? 50}%;`}
  data-tauri-drag-region
  bind:this={shell}
>
  <div class="drag-strip" data-tauri-drag-region aria-hidden="true"></div>
  {#if state?.targetLanguage === 'none'}
    <section class="source-only-feed anchor-feed" aria-label="Source language subtitles" bind:this={sourceFeed}>
      <div class="pane-content anchor-track">
        {#each displayHistory() as sentence}
          {#if sentence.sourceText.trim()}
            <p class="pane-line source-only-line">{sentence.sourceText}</p>
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
        {#each displayHistory() as sentence}
          {#if sentence.translation.trim()}
            <p class="pane-line target-line">{sentence.translation}</p>
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
          {#each displayHistory() as sentence}
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
          {#each displayHistory() as sentence}
            {#if sentence.translation.trim()}
              <p class="pane-line target-line">{sentence.translation}</p>
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
          {#each displayHistory() as sentence}
            {#if sentence.sourceText.trim()}
              <p class="pane-line source-line">{sentence.sourceText}</p>
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
  <footer class="brand-footer" data-tauri-drag-region>
    <span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span>
    <span class="brand-name">{productName}</span>
    <span class="brand-tagline">{state?.active ? "操作员私有 · 未审核" : "字幕布局预览 · 合成数据"}</span>
  </footer>
</main>
