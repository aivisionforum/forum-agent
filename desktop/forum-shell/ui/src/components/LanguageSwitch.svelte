<script lang="ts">
  import type { Locale } from '../lib/i18n/locale';
  export let value: Locale;
  export let onchange: (locale: Locale) => void;
  export let disabled = false;
</script>
<div class="language-switch" role="group" aria-label="界面语言 / Interface language">
  <button type="button" lang="zh-CN" aria-label="中文界面" aria-pressed={value === 'zh'} {disabled} on:click={() => onchange('zh')}>中文</button>
  <button type="button" lang="en" aria-label="English interface" aria-pressed={value === 'en'} {disabled} on:click={() => onchange('en')}>EN</button>
</div>
<style>
  .language-switch{display:inline-flex;flex-shrink:0;border:1px solid currentColor;border-radius:6px;overflow:hidden;color:inherit;}
  button{font:inherit;font-size:12px;line-height:1.4;min-height:32px;padding:6px 10px;border:0;background:transparent;color:inherit;cursor:pointer;white-space:nowrap;}
  button[aria-pressed="true"]{background:var(--accent,#536aa0);color:#fff;}
  button:focus-visible{outline:2px solid currentColor;outline-offset:-3px;}button:disabled{opacity:.5;cursor:wait;}
</style>
