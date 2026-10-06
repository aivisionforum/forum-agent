<script lang="ts">
  import { t } from '../lib/i18n';
  import { productName } from '../lib/product';
  export let subtitle: string;
  export let nativeControls = true;
  export let pinned = false;
  export let action: (name: 'close' | 'minimize' | 'fullscreen' | 'pin') => void;
  export let drag: (event: MouseEvent) => void = () => {};
</script>

<header class="meeting-titlebar">
  <div class="window-controls" aria-label={$t('窗口控制')}>
    {#if nativeControls}
      <button aria-label={$t('关闭会议窗口')} title={$t('关闭窗口（会议继续运行）')} on:click={() => action('close')}>×</button>
      <button aria-label={$t('最小化窗口')} title={$t('最小化')} on:click={() => action('minimize')}>−</button>
    {/if}
    <button aria-label={$t('切换全屏')} title={$t('进入或退出全屏')} on:click={() => action('fullscreen')}>⛶</button>
  </div>
  <div class="window-drag-title" data-tauri-drag-region role="presentation" on:mousedown={drag} on:dblclick={() => action('fullscreen')}><span data-tauri-drag-region>{productName}</span><span class="window-subtitle" data-tauri-drag-region>{subtitle}</span></div>
  {#if nativeControls}<button class="pin-button" class:pinned aria-pressed={pinned} title={$t('保持在其他窗口上方')} on:click={() => action('pin')}>{pinned ? $t('已置顶') : $t('置顶')}</button>{/if}
</header>

<style>
  .meeting-titlebar{height:44px;flex-shrink:0;display:flex;align-items:center;gap:16px;padding:0 16px;border-bottom:1px solid #25292d;background:#101214;user-select:none;box-sizing:border-box}
  .window-controls{display:flex;align-items:center;gap:4px}
  button{font:inherit;cursor:pointer}button:focus-visible{outline:2px solid #a4b9c5;outline-offset:-2px}
  .window-controls button{width:25px;height:26px;padding:0;border:0;border-radius:5px;background:transparent;color:#aeb4bb;font:20px/1 Arial,sans-serif}
  .window-controls button:hover{background:#303438;color:#eee}
  .window-drag-title{flex:1;min-width:0;height:100%;display:flex;align-items:center;justify-content:center;gap:12px;cursor:grab;color:#c5cad0;font-size:12px;letter-spacing:.01em}
  .window-drag-title:active{cursor:grabbing}.window-subtitle{color:#7e858d;padding-left:12px;border-left:1px solid #353a40}
  .pin-button{background:transparent;color:#9099a3;border:1px solid #343a41;border-radius:5px;padding:3px 10px;font-size:11px}.pin-button.pinned{color:#e6ebee;background:#303a41}
  @media(max-width:600px){.window-drag-title{gap:8px;font-size:11px}.window-subtitle{padding-left:8px}.window-drag-title>span:first-child{display:none}}
</style>
