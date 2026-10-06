<script lang="ts">
  import { t, uiLocale } from '../lib/i18n';
  import { createEventDispatcher } from 'svelte';
  import { forumClient as client } from '../lib/forum/factory';
  import { canReviewForWall, kindLabels, validationLabels, reviewLabels, publicationLabels,
    type ArtifactRecord, type ArtifactContent, type AnalysisEvidence, type ArtifactEdit,
    type ArtifactReviewCommand, type ArtifactPublishCommand, type ArtifactVisibilityCommand,
    type ExportFormat } from '../lib/forum/client';
  export let artifact: ArtifactRecord;
  export let busy = false;
  const dispatch = createEventDispatcher<{
    edit: ArtifactEdit; review: ArtifactReviewCommand; publish: ArtifactPublishCommand;
    hide: ArtifactVisibilityCommand; evidence: AnalysisEvidence; export: ExportFormat;
  }>();
  let editing = false;
  let editRevision = 0;
  let content: ArtifactContent = { title: '', sections: [] };
  let reason = '';
  let publishing = false;
  let publishRevision = 0;
  let publicTitle = '';
  let publicText = '';
  let publicConfirmed = false;
  let preparing = false;
  let proposalError = '';
  let proposalGeneration = 0;
  let publicEvidence: Array<{ evidence: AnalysisEvidence; reviewed_text: string; include: boolean }> = [];
  export function completeAction(action: string) { if (action === 'edit') editing = false; if (action === 'publish') publishing = false; }
  const operator = 'local-operator';
  const claimNames: Record<string,string> = {fact:'事实',decision:'决策候选',action:'行动项',risk:'风险',question:'问题',uncertainty:'不确定项'};
  function beginEdit() {
    editRevision = artifact.revision; content = structuredClone(artifact.content); editing = true;
  }
  function saveEdit() {
    dispatch('edit', { artifact_id: artifact.artifact_id, expected_revision: editRevision, content,
      operator_id: operator, reason: reason.trim() || '操作员修订正文，重新审核' });
  }
  function review(approved: boolean) {
    dispatch('review', { artifact_id: artifact.artifact_id, expected_revision: artifact.revision,
      review: approved ? 'approved' : 'rejected', operator_id:operator, reason: reason.trim() || (approved ? '本机操作员已检查正文与引用' : '本机操作员驳回，需要修订') });
  }
  async function beginPublish() {
    publishRevision = artifact.revision; publicTitle = ''; publicText = ''; publicConfirmed = false;
    const id = artifact.artifact_id;
    const generation = ++proposalGeneration;
    proposalError = '';
    const unique = new Map<string,AnalysisEvidence>();
    for (const section of artifact.content.sections) for (const claim of section.claims)
      for (const evidence of claim.evidence) unique.set(JSON.stringify(evidence),evidence);
    publicEvidence = [...unique.values()].map(evidence => ({ evidence, reviewed_text:'', include:false }));
    publishing = true;
    if (artifact.kind === 'insight') {
      preparing = true;
      try {
        const proposal = await client.prepareInsightPublication(id, publishRevision);
        if (generation !== proposalGeneration || !publishing || artifact.artifact_id !== id || artifact.revision !== publishRevision) return;
        publicTitle = proposal.reviewed_title; publicText = proposal.reviewed_text;
        publicEvidence = proposal.evidence.map(item => ({...item,include:true}));
      } catch { if (generation === proposalGeneration) proposalError = '未能准备公开草稿，请核对原文后手动填写。'; }
      finally { if (generation === proposalGeneration) preparing = false; }
    }
  }
  function publish() {
    dispatch('publish', { artifact_id:artifact.artifact_id, expected_revision:publishRevision,
      operator_id:operator, reason:reason.trim() || '操作员审核脱敏公开正文与公开引文',
      policy_hash:artifact.config.projection_policy_hash, reviewed_title:publicTitle.trim(), reviewed_text:publicText.trim(),
      evidence:publicEvidence.filter(e => e.include).map(({evidence,reviewed_text}) => ({evidence,reviewed_text:reviewed_text.trim()})) });
  }
  $: revisionChanged = editing && editRevision !== artifact.revision;
  $: publishChanged = publishing && publishRevision !== artifact.revision;
  $: publishReady = canReviewForWall(artifact) && !preparing && !publishChanged && publicTitle.trim() && publicText.trim() && publicConfirmed
    && publicEvidence.some(e => e.include) && publicEvidence.filter(e => e.include).every(e => e.reviewed_text.trim());
</script>
<article class="artifact-detail">
  <header><div><p class="eyebrow">{$t(kindLabels[artifact.kind])} · {$t("第 {0} 版", artifact.revision)}</p><h2>{artifact.content.title}</h2></div>
    <span class="privacy">{$t(publicationLabels[artifact.publication])}</span></header>
  <div class="states" aria-label={$t("独立审核状态")}>
    <span class:warning={artifact.validation !== 'valid'}>{$t(validationLabels[artifact.validation])}</span>
    <span>{artifact.review === 'approved' && artifact.operator_id === 'session-auto-approval' ? $t("本场自动批准") : $t(reviewLabels[artifact.review])}</span>
    <span class:warning={!artifact.coverage_complete}>{artifact.coverage_complete ? $t("输入范围完整") : $t("仅部分覆盖")}</span>
  </div>
  <p class="disclaimer">{$t("模型生成内容需由操作员核查；“决策候选”不代表会议已正式确认。")}</p>
  {#if artifact.validation === 'stale'}<p class="notice error">{$t("引用来源已改变，请重新生成或修订后审核。旧批准不能用于当前版本。")}</p>{/if}
  {#if !artifact.coverage_complete}<p class="notice">{$t("这份草稿未覆盖全部输入。请查看任务失败范围并重试，不能按完整纪要发布。")}</p>{/if}
  {#if editing}
    <div class="edit-panel">
      {#if revisionChanged}<p class="notice error">{$t("当前已有第")} {artifact.revision} {$t("版。你的编辑保留在下方；请先复制需要的内容并重新载入当前版，旧版本不能覆盖新版本。")}</p>{/if}
      <label>{$t("标题")}<input bind:value={content.title} maxlength="400" /></label>
      {#each content.sections as section}
        <label>{$t("章节标题")}<input bind:value={section.heading} /></label>
        {#each section.claims as claim}
          <label>{$t(claimNames[claim.kind] ?? claim.kind)}<textarea rows="3" bind:value={claim.text}></textarea></label>
          {#if claim.kind === 'action'}<div class="two-fields"><label>{$t("责任人")}<input bind:value={claim.assignee} placeholder={$t("未明确")} /></label><label>{$t("期限")}<input bind:value={claim.due} placeholder={$t("未明确")} /></label></div>{/if}
        {/each}
      {/each}
      <p class="small">{$t("保存会创建新版本，并清除原版本的批准。引文位置保留，修改后的结论仍需核查。")}</p>
      <div class="actions"><button class="primary" disabled={busy || revisionChanged || !content.title.trim()} on:click={saveEdit}>{$t("保存为新版本")}</button><button on:click={() => editing = false}>{$t("放弃编辑")}</button></div>
    </div>
  {:else}
    {#each artifact.content.sections as section}
      <section class="content-section"><h3>{section.heading}</h3>
        {#each section.claims as claim}
          <div class="claim"><span class="claim-kind">{$t(claimNames[claim.kind] ?? claim.kind)}</span><p>{claim.text}</p>
            {#if claim.assignee || claim.due}<p class="assignment">{claim.assignee ? $t("责任人：{0}", claim.assignee) : $t("责任人未明确")}{claim.due ? $t(" · 期限：{0}", claim.due) : ''}</p>{/if}
            {#if claim.grounding === 'unsupported'}<span class="unsupported">{$t("缺少可验证引用，需人工检查")}</span>{/if}
            <div class="references">{#each claim.evidence as evidence, i}<button on:click={() => dispatch('evidence',evidence)}>{$t("↗ 引用")} {i + 1}{evidence.kind === 'artifact' ? $t(" · 已发布资料") : $t(" · 原文")}</button>{/each}</div>
          </div>
        {/each}
      </section>
    {/each}
  {/if}
  <details class="coverage"><summary>{$t("输入覆盖与来源版本 ·")} {artifact.coverage.units.length} {$t("项")}</summary>
    <div class="coverage-items">{#each artifact.coverage.units as unit}<p class:bad={unit.status === 'failed'}>{unit.status === 'processed' ? $t("已处理") : unit.status === 'ignored_empty' ? $t("无文本") : $t("未完成")} · {unit.target.kind === 'source' ? unit.target.segment_id : unit.target.artifact_id} · {unit.start_utf8}–{unit.end_utf8} bytes{unit.reason ? ` · ${unit.reason}` : ''}</p>{/each}</div>
  </details>
  <label class="reason">{$t("审核或修订说明")}<input bind:value={reason} placeholder={$t("可填写更正依据、批准或隐藏原因")} /></label>
  <div class="actions review-actions">
    <button disabled={busy || editing} on:click={beginEdit}>{$t("编辑正文")}</button>
    {#if artifact.kind !== 'insight'}<button disabled={busy || editing || artifact.validation === 'stale' || artifact.validation === 'invalid'} on:click={() => review(true)}>{$t("批准当前版")}</button>{/if}
    <button disabled={busy || editing} on:click={() => review(false)}>{$t("驳回")}</button>
    <button class="primary" disabled={busy || editing || !canReviewForWall(artifact)} on:click={beginPublish}>{$t("审核并上墙")}</button>
    {#if artifact.publication === 'published'}<button disabled={busy} on:click={() => dispatch('hide',{artifact_id:artifact.artifact_id,expected_revision:artifact.revision,publication:'hidden',operator_id:operator,reason:reason.trim() || '操作员隐藏公开内容'})}>{$t("从大屏隐藏")}</button>{/if}
  </div>
  {#if artifact.kind === 'suggested_questions'}<p class="small">{$t("主持人问题仅在操作台显示。")}</p>{/if}
  <div class="exports"><span>{$t("导出当前版本")}</span>{#each ['markdown','html','json'] as format}<button disabled={busy} on:click={() => dispatch('export',format as ExportFormat)}>{format === 'markdown' ? 'Markdown' : format.toUpperCase()}</button>{/each}<small>{$t("草稿导出保留审核状态；导出文件无法远程撤回。")}</small></div>
  {#if publishing}
    <section class="public-editor" aria-label={$t("审核独立公开版本")}>
      <h3>{$t("准备大屏公开内容")}</h3><p>{$t("请核对下方公开草稿，必要时修改。发言人仅用“发言人A/B”等匿名标签；确认后，只有这些公开字段上墙，内部原文和状态不会上墙。")}</p>
      {#if preparing}<p role="status">{$t("正在准备匿名公开草稿…")}</p>{/if}
      {#if proposalError}<p role="status">{$t(proposalError)}</p>{/if}
      {#if publishChanged}<p class="notice error">{$t("版本已变化，请关闭此编辑区，重新审核当前版。")}</p>{/if}
      <label>{$t("公开标题")}<input disabled={preparing} bind:value={publicTitle} placeholder={$t("人工确认后的公开标题")} /></label>
      <label>{$t("公开正文")}<textarea disabled={preparing} rows="6" bind:value={publicText} placeholder={$t("填写已核对并脱敏的公开正文")}></textarea></label>
      {#each publicEvidence as item, i}<div class="public-evidence"><label class="checkbox"><input type="checkbox" bind:checked={item.include} />{$t("公开引用")} {i + 1}</label><button on:click={() => dispatch('evidence',item.evidence)}>{$t("查看内部出处")}</button>{#if item.include}<textarea rows="2" bind:value={item.reviewed_text} placeholder={$t("填写人工审核、脱敏后的引用文本")}></textarea>{/if}</div>{/each}
      <label class="checkbox"><input type="checkbox" disabled={preparing} bind:checked={publicConfirmed} />{$t("我已核对当前版本及公开内容，确认不含需要隐藏的姓名、账号或敏感细节。")}</label>
      <div class="actions"><button class="primary" disabled={busy || !publishReady} on:click={publish}>{$t("批准并上墙")}</button><button on:click={() => {publishing = false; proposalGeneration++; preparing = false;}}>{$t("关闭")}</button></div>
    </section>
  {/if}
</article>
<style>
  .artifact-detail { background:#fff; padding:24px; border:1px solid #d8dbdf; } header { display:flex; justify-content:space-between; gap:16px; } h2 { font-size:23px; line-height:1.4; margin:5px 0 16px; white-space:pre-wrap; overflow-wrap:anywhere; } .eyebrow { font-size:11px; letter-spacing:.08em; color:var(--muted); margin:0; } .privacy { font-size:11px; white-space:nowrap; padding-top:6px; }
  .states { display:flex; gap:6px; flex-wrap:wrap; } .states span { padding:5px 8px; font-size:11px; background:#edf3ef; } .states span.warning { background:#fff0d5; color:#7a5518; }
  .disclaimer,.small { color:var(--muted); font-size:12px; line-height:1.7; } .notice { background:#fff5df; padding:12px; font-size:13px; line-height:1.6; } .notice.error { background:#ffeded; color:#9b2828; }
  h3 { margin:22px 0 12px; font-size:16px; } .claim { padding:12px 0; border-top:1px solid #eceef0; } .claim-kind { color:var(--accent-text); font-size:11px; } .claim p { margin:6px 0; white-space:pre-wrap; overflow-wrap:anywhere; line-height:1.8; font-size:14px; } .claim .assignment { font-size:12px; color:var(--muted); } .unsupported { color:#96621d; font-size:11px; }
  .references { display:flex; gap:7px; flex-wrap:wrap; margin-top:6px; } button { padding:8px 10px; font-size:12px; border:1px solid #bbc0c7; background:#fff; } button:disabled { opacity:.4; cursor:default; } .references button { font-size:11px; color:var(--accent-text); border-color:#d5d8e2; }
  .actions { display:flex; gap:8px; flex-wrap:wrap; margin-top:14px; } .primary { background:var(--accent); color:var(--accent-on-solid); border-color:var(--accent); }
  label { display:flex; flex-direction:column; gap:6px; font-size:12px; margin:12px 0; } input:not([type=checkbox]),textarea { width:100%; padding:10px; border:1px solid #c8ccd2; border-radius:0; background:#fff; color:var(--ink); font:inherit; font-size:13px; line-height:1.7; } textarea { resize:vertical; } .two-fields { display:grid; grid-template-columns:1fr 1fr; gap:12px; } .reason { border-top:1px solid #d8dbdf; padding-top:14px; }
  .coverage { font-size:12px; border-top:1px solid #e4e6e9; padding:14px 0; margin-top:20px; } summary { cursor:pointer; } .coverage-items { max-height:180px; overflow:auto; overflow-wrap:anywhere; } .coverage-items p { font-size:11px; color:var(--muted); } .coverage-items p.bad { color:#a43c31; }
  .exports { display:flex; gap:7px; align-items:center; flex-wrap:wrap; margin-top:20px; font-size:11px; } .exports small { width:100%; color:var(--muted); line-height:1.7; }
  .public-editor { margin-top:24px; border:1px solid var(--accent); background:#fafaff; padding:18px; } .public-editor>h3 { margin-top:0; } .public-editor>p { font-size:12px; line-height:1.7; } .checkbox { flex-direction:row; align-items:flex-start; line-height:1.7; } .checkbox input { margin:4px 5px 0 0; width:14px; } .public-evidence { border-top:1px solid #dadddf; padding:8px 0; }
  @media(max-width:650px) { .artifact-detail { padding:16px; } h2 {font-size:19px;} }
</style>
