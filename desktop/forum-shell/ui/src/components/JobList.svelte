<script lang="ts">
  import { t, uiLocale } from '../lib/i18n';
  import { createEventDispatcher } from 'svelte';
  import { jobLabels, kindLabels, type AnalysisJob } from '../lib/forum/client';
  export let jobs: AnalysisJob[] = [];
  export let busy = false;
  const dispatch = createEventDispatcher<{ cancel: AnalysisJob; retry: AnalysisJob; result: AnalysisJob }>();
  const retryable = (state: string) => ['failed', 'interrupted', 'succeeded_partial', 'cancelled'].includes(state);
  const cancellable = (state: string) => ['queued', 'waiting', 'running'].includes(state);
  function percent(job: AnalysisJob) { return job.progress.total_units ? Math.min(100, Math.floor(job.progress.completed_units / job.progress.total_units * 100)) : 0; }
</script>
<div class="job-list">
  {#each jobs as job (job.job_id)}
    <article class="job-card">
      <div class="job-heading"><strong>{$t(kindLabels[job.kind])}</strong><span class:error={job.state === 'failed'} class:active={['queued','waiting','running'].includes(job.state)} class="badge">{$t(jobLabels[job.state] ?? job.state)}</span></div>
      <div class="job-meta">{$t("第")} {job.attempt} {$t("次执行 ·")} {new Date(job.created_at_ms).toLocaleString($uiLocale === 'en' ? 'en-US' : 'zh-CN')} · {Math.round(job.budget_ms / 1000)} {$t("秒总预算（含排队）")}</div>
      {#if job.progress.total_units > 0}<progress max="100" value={percent(job)} aria-label={$t("任务完成进度")}></progress>{/if}
      <p>{job.progress.phase || $t("等待调度")}{#if job.progress.total_units > 0} · {job.progress.completed_units}/{job.progress.total_units} {$t("段")}{/if}</p>
      {#if job.progress.wait_reason}<p class="wait-reason">{job.progress.wait_reason}</p>{/if}
      {#if job.state === 'succeeded_partial'}<p class="warning">{$t("仅生成部分内容，缺失范围会保留在草稿中；不能作为完整纪要发布。")}</p>{/if}
      {#if job.error}<p class="error-text">{job.error}</p>{/if}
      <div class="actions">
        {#if cancellable(job.state)}<button disabled={busy} on:click={() => dispatch('cancel', job)}>{$t("取消任务")}</button>{/if}
        {#if job.state === 'cancel_requested'}<span>{$t("正在等待工作进程停止…")}</span>{/if}
        {#if retryable(job.state)}<button disabled={busy} on:click={() => dispatch('retry', job)}>{$t("重试未完成部分")}</button>{/if}
        {#if job.result}<button on:click={() => dispatch('result', job)}>{$t("查看草稿 →")}</button>{/if}
      </div>
    </article>
  {:else}<div class="empty-state"><h3>{$t("还没有分析任务")}</h3><p>{$t("选择会议后，可以生成即时洞察或完整纪要。任务状态会持续保存。")}</p></div>{/each}
</div>
<style>
  .job-list { display:grid; gap:12px; } .job-card { border:1px solid #d8dbdf; padding:18px; background:white; }
  .job-heading { display:flex; align-items:center; justify-content:space-between; gap:10px; } .job-meta { font-size:11px; color:var(--muted); margin:10px 0; }
  .badge { font-size:11px; padding:5px 8px; background:#f0f1f3; } .badge.active { background:var(--accent-soft); color:var(--accent-text); } .badge.error,.error-text { color:#a52e2e; }
  p { margin:8px 0; font-size:13px; line-height:1.6; white-space:pre-wrap; overflow-wrap:anywhere; }
  progress { width:100%; height:5px; accent-color:var(--accent); } .wait-reason,.warning { color:#7d5619; background:#fff6e5; padding:8px 10px; }
  .actions { display:flex; gap:8px; margin-top:14px; font-size:12px; align-items:center; } button { background:white; border:1px solid #bbc0c7; padding:7px 10px; } button:disabled { opacity:.4; }
  .empty-state { border:1px dashed #cbd0d7; text-align:center; padding:40px 20px; color:var(--muted); } h3 { color:var(--ink); font-size:16px; }
</style>
