<script lang="ts">
  import { t, uiLocale, initializeLocale, setLocale, type Locale } from './lib/i18n';
  import { productName, brandLogoUrl as logoUrl } from './lib/product';
  import AudienceTitlebar from './components/AudienceTitlebar.svelte';
  import './audience-screen.css';
  import LanguageSwitch from './components/LanguageSwitch.svelte';
  import { onMount, tick } from 'svelte';
  import { ForumClient, type PublicSnapshot } from './lib/forum/client';
  import { DisplayTransport } from './lib/forum/transport';
  import { displayCredentials, PublicProjection } from './lib/forum/display-state';
  import { wallArtifacts, wallMessage, paginateText, type WallView } from './lib/forum/insight-wall';
  initializeLocale('wall');
  const nativeWindow = new URL(window.location.href).searchParams.get('window') === 'desktop';
  let pinned = false;
  let windowError = '';
  function nativeAction(action: string) { window.location.assign(`forum-wall-window://${action}`); }
  async function windowAction(action: 'close' | 'minimize' | 'fullscreen' | 'pin') {
    windowError = '';
    if (nativeWindow) { nativeAction(action); return; }
    if (action === 'fullscreen') {
      try {
        if (document.fullscreenElement) await document.exitFullscreen();
        else await document.documentElement.requestFullscreen();
      } catch { windowError = '窗口暂不可用，请操作员检查'; }
    }
  }
  function dragWindow(event: MouseEvent) {
    if (nativeWindow && event.button === 0 && event.detail < 2) nativeAction('drag');
  }
  onMount(() => {
    const update = (event: Event) => {
      const detail = (event as CustomEvent<{pinned:boolean;error:string}>).detail;
      pinned = detail.pinned; windowError = detail.error ? '窗口暂不可用，请操作员检查' : '';
    };
    window.addEventListener('audience-window-state', update);
    if (nativeWindow) nativeAction('state');
    return () => window.removeEventListener('audience-window-state', update);
  });
  async function chooseLanguage(locale: Locale) {
    setLocale(locale, 'wall');
    const url = new URL(window.location.href); url.searchParams.set('lang', locale);
    window.history.replaceState(null, '', url);
    await layout();
  }
  let snapshot: PublicSnapshot | null = null;
  let connected = false;
  let error = '';
  let layoutError = '';
  let view: WallView = new URL(window.location.href).searchParams.get('view') === 'recap' ? 'recap' : 'live';
  let pages: string[] = [];
  let page = 0;
  let now = Date.now();
  let clockOffset = 0;
  let stage: HTMLDivElement;
  let measure: HTMLDivElement;
  let contentKey = '';
  let layoutGeneration = 0;
  let rotateAt = Date.now() + 15000;
  $: status = wallMessage(snapshot,now + clockOffset);

  async function layout() {
    const generation = ++layoutGeneration;
    await tick();
    if (!stage || !measure || !connected || generation !== layoutGeneration) return;
    const height = stage.clientHeight - 4;
    measure.style.width = `${stage.clientWidth}px`;
    try {
      pages = wallArtifacts(snapshot,view).flatMap(a => paginateText(
        a.title === '讨论要点' ? a.text : `${a.title}\n\n${a.text}`,
        text => { measure.textContent = text; return measure.scrollHeight <= height; }
      ));
      page = Math.min(page,Math.max(0,pages.length - 1));
      layoutError = '';
    } catch (e) { pages = []; layoutError = e instanceof Error ? e.message : '请调整屏幕大小。'; }
    finally { measure.textContent = ''; }
  }
  function chooseView(next: WallView) {
    view = next; page = 0; rotateAt = Date.now() + 15000;
    const url = new URL(window.location.href); url.searchParams.set('view',view);
    window.history.replaceState(null,'',url); void layout();
  }
  function turnPage(delta: number) { page = (page + delta + pages.length) % pages.length; rotateAt = Date.now() + 15000; }
  onMount(() => {
    let disposed = false; let busy = false;
    const projection = new PublicProjection();
    const clear = () => { projection.disconnect(); snapshot = null; pages = []; connected = false; layoutGeneration++; };
    let client: ForumClient;
    let resize: ResizeObserver | undefined;
    let timer: number | undefined;
    let ticker: number | undefined;
    let online: () => void = () => {};
    const offline = () => { clear(); error = '连接中断，正在等待恢复'; };
    try {
      const credentials = displayCredentials(new URL(window.location.href),window.sessionStorage);
      window.history.replaceState(null,'',credentials.cleanUrl);
      client = new ForumClient(new DisplayTransport(window.location.origin,credentials.token));
      const refresh = async () => {
        if (busy || disposed) return;
        busy = true;
        try {
          const next = projection.accept(await client.publicSnapshot(credentials.sessionId,projection.cursor));
          if (disposed) return;
          const key = JSON.stringify(next.artifacts);
          const changed = !connected || contentKey !== key;
          snapshot = next; connected = true; error = '';
          now = Date.now();
          clockOffset = next.wall ? next.wall.server_time_ms - now : 0;
          if (changed) { contentKey = key; pages = []; page = 0; rotateAt = Date.now() + 15000; await layout(); }
        } catch {
          if (!disposed) { clear(); error = '连接暂不可用，请操作员检查大屏连接'; }
        } finally { busy = false; }
      };
      online = () => void refresh();
      window.addEventListener('offline',offline); window.addEventListener('online',online);
      resize = new ResizeObserver(() => { rotateAt = Date.now() + 15000; void layout(); });
      resize.observe(stage);
      void document.fonts.ready.then(() => { if (!disposed) void layout(); });
      void refresh();
      timer = window.setInterval(() => void refresh(),2000);
      ticker = window.setInterval(() => {
        now = Date.now();
        if (now >= rotateAt && pages.length > 1) turnPage(1);
      },250);
    } catch { error = '请操作员从本场控制台重新打开洞察墙'; }
    return () => {
      disposed = true; layoutGeneration++; resize?.disconnect();
      window.clearInterval(timer); window.clearInterval(ticker);
      window.removeEventListener('offline',offline); window.removeEventListener('online',online);
    };
  });
</script>
<svelte:head><title>{productName} · {$uiLocale === 'en' ? 'Insight wall' : '洞察墙'}</title><meta name="referrer" content="no-referrer" /></svelte:head>
<main class="display-shell">
  <AudienceTitlebar subtitle={$uiLocale === 'en' ? 'Public insights · Screen 2' : '公开洞察墙 · 屏幕 2'} nativeControls={nativeWindow} {pinned} action={windowAction} drag={dragWindow}/>
  {#if windowError}<div class="window-error" role="alert">{$t(windowError)}</div>{/if}
  <header class="audience-heading"><div><span class="eyebrow">LIVE INSIGHTS</span><h1>{view === 'recap' ? $t("本场至今") : $t("讨论中的洞察")}</h1></div>
    <div class="activity" role="status"><strong class:working={status.label === 'WORKING'}>{connected ? $t(status.label) : $t("正在连接")}</strong><span>{connected ? $t(status.detail) : $t("正在同步本场洞察")}</span></div>
  </header>
  <nav aria-label={$t("洞察墙页面")}><div class="view-tabs"><button class:selected={view === 'live'} aria-pressed={view === 'live'} on:click={() => chooseView('live')}>{$t("最新洞察")}</button><button class:selected={view === 'recap'} aria-pressed={view === 'recap'} on:click={() => chooseView('recap')}>{$t("本场至今")}</button></div><LanguageSwitch value={$uiLocale} onchange={chooseLanguage}/></nav>
  <div class="stage" bind:this={stage}>
    {#if error || layoutError}<section class="empty" role="alert"><span class="symbol">↻</span><h2>{$t(error || layoutError)}</h2><p>{$t("连接或画面恢复后自动继续。")}</p></section>
    {:else if connected && pages.length}<article class="wall-copy">{pages[page]}</article>
    {:else}<section class="empty"><span class="symbol">✦</span><h2>{connected && snapshot?.wall?.phase === 'finished' ? $t("本场暂无公开洞察") : connected && snapshot?.wall?.phase === 'working' ? $t("正在整理讨论中的要点") : $t("正在听取讨论")}</h2><p>{view === 'recap' ? $t("本场已公开的要点会累积在这里，供主持人回顾。") : $t("新的讨论要点就绪后，会自动出现在这里。")}</p></section>{/if}
  </div>
  <div class="wall-copy measure" bind:this={measure} aria-hidden="true"></div>
  <footer class="audience-footer">
    <span class="brand-logo-tile"><span class="brand-logo" style={`--brand-mark:url("${logoUrl}")`} aria-hidden="true"></span></span><span class="brand-name">{productName}</span>
    <div class="paging">{#if connected && pages.length > 1}<button aria-label={$t("上一页")} on:click={() => turnPage(-1)}>←</button><span>{page + 1} / {pages.length} {$t("· 每 15 秒翻页")}</span><button aria-label={$t("下一页")} on:click={() => turnPage(1)}>→</button>{:else}<span>{connected ? $t("持续同步") : $t("等待连接")}</span>{/if}</div>
    <span class="brand-tagline">{$uiLocale === 'en' ? 'Public insights · Screen 2' : '公开洞察墙 · 屏幕 2'}</span>
  </footer>
</main>
<style>
  :global(html),:global(body),:global(#app){margin:0;height:100%;background:var(--audience-background);color:var(--audience-foreground);overflow:hidden}
  :global(body){font-family:var(--audience-font);font-synthesis:none;text-rendering:geometricPrecision}
  .display-shell{height:100dvh;width:100%;box-sizing:border-box;display:flex;flex-direction:column;background:var(--audience-background);overflow:hidden;--accent:#363a3e}
  .audience-heading{flex-shrink:0}
  .activity{display:flex;align-items:center;gap:14px;text-align:right;color:#929ea8}
  .activity strong{font-size:18px;font-weight:500;color:#c5cad0;font-variant-numeric:tabular-nums;white-space:nowrap}
  .activity span{font-size:11px;line-height:1.6;max-width:250px}.working{animation:pulse 2s ease-in-out infinite}@keyframes pulse{50%{opacity:.5}}
  nav{display:flex;align-items:center;justify-content:space-between;gap:16px;padding:0 28px;height:50px;flex-shrink:0;border-bottom:1px solid #1d2126;color:#929aa3}
  .view-tabs{height:100%;display:flex;align-items:stretch;gap:24px}
  button{font:inherit;background:transparent;color:#929ea8;border:0;cursor:pointer}
  button:focus-visible{outline:2px solid #c5cad0;outline-offset:-2px}
  .view-tabs button{font-size:11px;border-bottom:2px solid transparent;padding:0 2px}
  .view-tabs button:hover{color:#eee}.view-tabs button.selected{color:#f6f7f8;border-bottom-color:#f6f7f8}
  nav :global(.language-switch){border-color:#343a41;border-radius:5px}
  nav :global(.language-switch button){min-height:26px;padding:3px 9px;font-size:11px}
  .stage{flex:1;min-height:0;position:relative;overflow:hidden;margin:24px 34px}
  .wall-copy{box-sizing:border-box;white-space:pre-wrap;overflow-wrap:anywhere;font-size:clamp(24px,2.6vw,42px);line-height:1.34;font-weight:620;letter-spacing:-.022em;margin:0;color:#f7f8fa}
  .measure{position:fixed;left:-20000px;top:0;visibility:hidden;pointer-events:none;height:auto}
  .empty{height:100%;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:20px;box-sizing:border-box}
  .symbol{font-size:32px;color:#737f8b}h2{font-size:20px;color:#a5aeb7;font-weight:400;line-height:1.5;margin:18px 0 10px}.empty p{font-size:13px;color:#69737f;line-height:1.8;margin:0}
  .paging{display:flex;align-items:center;gap:10px;font-size:10px;color:#8997a2;margin-left:auto}.paging button{padding:3px 8px;font-size:12px}
  .audience-footer .brand-tagline{margin-left:12px}
  .window-error{position:absolute;top:44px;left:0;right:0;background:#292d31;color:#e4e7eb;font-size:12px;z-index:10;padding:10px}
  @media(max-width:650px){nav{padding:0 22px}.activity{gap:6px;flex-direction:column;align-items:flex-end}.activity strong{font-size:15px}.activity span{max-width:180px;font-size:10px}.stage{margin:20px 26px}.audience-footer .brand-tagline{display:none}.view-tabs{gap:16px}}
  @media(max-height:500px){.audience-heading{min-height:52px;padding-block:9px}.audience-heading .eyebrow{display:none}.audience-heading h1{margin:0}nav{height:40px}.stage{margin-block:12px}}
  @media(prefers-reduced-motion:reduce){.working{animation:none}}
</style>
