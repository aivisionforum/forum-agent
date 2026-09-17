<script lang="ts">
  import { onMount } from 'svelte';
  import { ForumClient, kindLabels, type PublicSnapshot } from './lib/forum/client';
  import { DisplayTransport } from './lib/forum/transport';
  import { displayCredentials, PublicProjection } from './lib/forum/display-state';
  import { errorText } from './lib/forum/session-fence';
  let snapshot: PublicSnapshot | null = null;
  let connected = false;
  let error = '';
  let updated = '';
  onMount(() => {
    let disposed = false; let busy = false;
    const projection = new PublicProjection();
    let client: ForumClient;
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
          snapshot = next; connected = true; error = ''; updated = new Date().toLocaleTimeString();
        } catch(e) {
          if (!disposed) { projection.disconnect(); snapshot = null; connected = false; error = errorText(e); }
        } finally { busy = false; }
      };
      const offline = () => { projection.disconnect(); snapshot = null; connected = false; error = '网络已断开，公开内容暂时隐藏。'; };
      const online = () => void refresh();
      window.addEventListener('offline',offline); window.addEventListener('online',online);
      void refresh();
      const timer = window.setInterval(() => void refresh(),2000);
      return () => { disposed = true; window.clearInterval(timer); window.removeEventListener('offline',offline); window.removeEventListener('online',online); };
    } catch(e) { error = errorText(e); return () => { disposed = true; }; }
  });
</script>
<svelte:head><title>AI Vision Forum · 已审核内容</title><meta name="referrer" content="no-referrer" /></svelte:head>
<main class="display-shell">
  <header><div><p>AI VISION FORUM</p><h1>讨论中的洞察</h1></div><div class:connected class="connection"><span></span>{connected ? '本机同步 · 已审核发布' : '连接未就绪'}<small>{updated && connected ? `更新于 ${updated}` : ''}</small></div></header>
  {#if error}<section class="disconnected" role="alert"><p class="symbol">↻</p><h2>公开内容暂不可用</h2><p>{error}</p><p class="hint">连接恢复后会重新读取当前公开版本。</p></section>
  {:else if !connected}<section class="disconnected"><h2>正在读取已审核内容…</h2></section>
  {:else if snapshot?.artifacts.length}<div class="public-grid">{#each snapshot.artifacts as artifact (artifact.public_id)}<article><p class="kind">{kindLabels[artifact.kind] ?? '会议成果'} · V{artifact.revision}</p><h2>{artifact.title}</h2><p class="body">{artifact.text}</p>{#if artifact.evidence.length}<div class="public-evidence"><span>公开引用</span>{#each artifact.evidence as evidence}<blockquote>{evidence.text}</blockquote>{/each}</div>{/if}</article>{/each}</div>
  {:else}<section class="disconnected"><p class="symbol">✦</p><h2>等待新的洞察</h2><p>操作员审核并发布后，内容会出现在这里。</p></section>{/if}
  <footer><span>LOCAL FIRST · HUMAN REVIEWED</span><span>公开只读视图 · 自动同步撤回与隐藏</span></footer>
</main>
<style>
  :global(html),:global(body),:global(#app) { margin:0;min-height:100%;height:100%;background:#10151d;color:#f4f5f8; } :global(body) {font-family:Inter,'PingFang SC',sans-serif;}
  .display-shell {height:100%;overflow:auto;padding:42px 6vw;display:flex;flex-direction:column;box-sizing:border-box;background:radial-gradient(ellipse at top right,#202941,#10151d 65%);}
  header {display:flex;justify-content:space-between;gap:30px;align-items:center;border-bottom:1px solid #ffffff25;padding-bottom:30px;} header p {font-size:12px;letter-spacing:.2em;color:#a6b0cc;margin:0 0 14px;} h1 {font-size:clamp(26px,3vw,44px);font-weight:550;letter-spacing:-.03em;margin:0;}.connection {font-size:12px;color:#9ba3b5;line-height:2;}.connection>span {display:inline-block;width:7px;height:7px;border-radius:50%;background:#af784f;margin-right:8px;}.connection.connected>span{background:#7ab697;}.connection small{display:block;font-size:10px;margin-left:15px;color:#7e899e;}
  .public-grid {display:grid;grid-template-columns:repeat(auto-fit,minmax(min(440px,100%),1fr));gap:24px;margin:32px 0;align-items:start;}.public-grid article{background:#ffffff08;border:1px solid #ffffff1f;padding:30px;}.kind{font-size:11px;letter-spacing:.1em;color:#9facd5;margin:0 0 18px;}h2{font-size:clamp(21px,2.2vw,30px);font-weight:500;line-height:1.6;margin:0 0 16px;overflow-wrap:anywhere;white-space:pre-wrap;}.body{font-size:clamp(17px,1.7vw,24px);line-height:1.9;color:#d6dce8;white-space:pre-wrap;overflow-wrap:anywhere;margin:0;}.public-evidence{margin-top:26px;border-top:1px solid #ffffff20;padding-top:16px;}.public-evidence>span{font-size:10px;color:#8791a6;}.public-evidence blockquote{margin:12px 0;padding-left:14px;border-left:2px solid #607198;color:#a3aec4;font-size:14px;line-height:1.9;white-space:pre-wrap;overflow-wrap:anywhere;}
  .disconnected {flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;text-align:center;padding:75px 0;min-height:300px;}.disconnected p{font-size:15px;color:#8f9bb3;line-height:1.8;}.disconnected .symbol{font-size:42px;color:#7d8bae;}.disconnected .hint{font-size:12px;}footer{display:flex;justify-content:space-between;gap:16px;font-size:10px;letter-spacing:.06em;color:#62708c;padding-top:30px;margin-top:auto;}
  @media(max-width:650px){.display-shell{padding:28px 20px;}header{align-items:flex-start;flex-direction:column;gap:20px;}.public-grid article{padding:22px;}footer{flex-direction:column;}}
</style>
