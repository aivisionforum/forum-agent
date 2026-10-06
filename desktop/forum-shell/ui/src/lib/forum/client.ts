import type {
  AnalysisKind, AnalysisJob, ArtifactRecord, ArtifactEdit, ArtifactReviewCommand,
  ArtifactPublishCommand, ArtifactVisibilityCommand, AnalysisEvidence, PublicSnapshot,
  SessionSummary, SnapshotItem, SnapshotPage, TranslationRecord, PageKey
} from '../../../../../../packages/contracts/forum.generated';
import type { ForumTransport } from './transport';
export type {
  AnalysisKind, AnalysisJob, ArtifactRecord, ArtifactContent, AnalysisClaim, AnalysisEvidence,
  PublicSnapshot, PublicArtifact, ArtifactEdit, ArtifactReviewCommand, ArtifactPublishCommand,
  ArtifactVisibilityCommand, SessionSummary, SnapshotItem, TranslationRecord
} from '../../../../../../packages/contracts/forum.generated';

export type SessionPage = { items: SessionSummary[]; next_after: string | null };
export type MeetingPage = SnapshotPage & { translations: TranslationRecord[] };
export type AnalysisState = {
  session_id: string; cursor: number; notice?: string | null; jobs: AnalysisJob[]; artifacts: ArtifactRecord[];
  next_jobs: string | null; next_artifacts: string | null;
  insight_settings?: {mode: 'gated' | 'automatic'};
  live_cursor?: number; live_artifacts?: ArtifactRecord[] | null;
};
export type JobRequest = { request_id: string; session_ids: string[]; kind: AnalysisKind; automatic: boolean; public_selections?: import('./network').PublishedSelection[] };
export type JobAction = { job_id: string; expected_attempt: number };
export type EvidenceDetail = { session_id?: string | null; text: string; segment_id?: string; segment_revision?: number; start_ms?: number; quote?: string; current: boolean };
export type DisplayInfo = { url: string; expiresAt: string };
export type ExportFormat = 'markdown' | 'html' | 'json';
export type ExportResult = { filename: string; mime_type: string; content: string; saved_path?: string };

/** All forum views share this API. The display transport exposes only publicSnapshot. */
export class ForumClient {
  constructor(readonly transport: ForumTransport) {}
  get mode() { return this.transport.mode; }
  sessions(after: string | null = null): Promise<SessionPage> {
    return this.transport.call('list_meeting_sessions_page', { after, limit: 30 });
  }
  transcript(sessionId: string, cursor: number | null = null, after: PageKey | null = null): Promise<MeetingPage> {
    return this.transport.call('get_meeting_transcript', { sessionId, cursor, after });
  }
  analysis(sessionId: string, afterJobs: string | null = null, afterArtifacts: string | null = null): Promise<AnalysisState> {
    return this.transport.call('get_analysis_state', { sessionId, afterJobs, afterArtifacts });
  }
  liveAnalysis(sessionId: string, liveCursor: number): Promise<AnalysisState> {
    return this.transport.call('get_analysis_state', {sessionId, liveCursor});
  }
  importLegacy(request: { title: string; content: string }): Promise<SessionSummary> { return this.transport.call('import_legacy_transcript', { request }); }
  createJob(request: JobRequest): Promise<AnalysisJob> { return this.transport.call('create_analysis_job', { request }); }
  cancelJob(request: JobAction): Promise<AnalysisJob> { return this.transport.call('cancel_analysis_job', { request }); }
  retryJob(request: JobAction): Promise<AnalysisJob> { return this.transport.call('retry_analysis_job', { request }); }
  editArtifact(request: ArtifactEdit): Promise<ArtifactRecord> { return this.transport.call('revise_artifact', { request }); }
  reviewArtifact(request: ArtifactReviewCommand): Promise<ArtifactRecord> { return this.transport.call('review_artifact', { request }); }
  publishArtifact(request: ArtifactPublishCommand): Promise<ArtifactRecord> { return this.transport.call('publish_artifact', { request }); }
  approveAndPublishArtifact(request: ArtifactPublishCommand): Promise<ArtifactRecord> { return this.transport.call('approve_and_publish_artifact', {request}); }
  prepareInsightPublication(artifactId: string, revision: number): Promise<ArtifactPublishCommand> {
    return this.transport.call('prepare_insight_publication', {artifactId, revision});
  }
  hideArtifact(request: ArtifactVisibilityCommand): Promise<ArtifactRecord> { return this.transport.call('hide_artifact', { request }); }
  artifact(artifactId: string, revision: number | null = null): Promise<ArtifactRecord | null> { return this.transport.call('get_analysis_artifact', { artifactId, revision }); }
  evidence(evidence: AnalysisEvidence): Promise<EvidenceDetail> { return this.transport.call('get_analysis_evidence', { evidence }); }
  exportArtifact(artifactId: string, revision: number, format: ExportFormat): Promise<ExportResult> {
    return this.transport.call('export_artifact', { request: { artifact_id: artifactId, expected_revision: revision, format } });
  }
  setInsightMode(sessionId: string, mode: 'gated' | 'automatic'): Promise<{mode: 'gated' | 'automatic'}> {
    return this.transport.call('set_insight_settings', {request: {session_id:sessionId,mode,operator_id:'local-operator',reason:mode === 'automatic' ? '操作员为本场选择自动批准并负责现场纠错' : '操作员为本场选择守门审核'}});
  }
  insightSettings(sessionId: string): Promise<{mode: 'gated' | 'automatic'}> {
    return this.transport.call('get_insight_settings', {sessionId});
  }
  showInsightWall(sessionId: string, language: 'zh' | 'en'): Promise<DisplayInfo> {
    return this.transport.call('show_insight_wall', {sessionId, language});
  }
  displayInfo(sessionId: string): Promise<DisplayInfo> { return this.transport.call('get_display_info', { sessionId }); }
  publicSnapshot(sessionId: string, after: number | null): Promise<PublicSnapshot> {
    return this.transport.call('get_public_snapshot', { sessionId, after });
  }
}

export const kindLabels: Record<AnalysisKind, string> = {
  insight: '即时洞察', minutes: '完整纪要', event_report: '活动报告',
  suggested_questions: '主持人问题', redaction_review: '脱敏检查', closing_brief: '闭幕简报'
};
export const jobLabels: Record<string, string> = {
  queued: '已入队', waiting: '等待资源', running: '生成中', cancel_requested: '正在取消',
  cancelled: '已取消', succeeded: '已完成', succeeded_partial: '部分完成', failed: '失败', interrupted: '已中断'
};
export const validationLabels: Record<string, string> = { valid: '引用有效', needs_review: '需要核查', invalid: '验证未通过', stale: '来源已变更' };
export const reviewLabels: Record<string, string> = { draft: '待审核', approved: '已批准', rejected: '已驳回' };
export const publicationLabels: Record<string, string> = { private: '仅操作台', published: '已公开', hidden: '已隐藏', withdrawn: '已撤回' };
export function canPublish(artifact: ArtifactRecord): boolean {
  return artifact.review === 'approved' && canReviewForWall(artifact);
}
export function canReviewForWall(artifact: ArtifactRecord): boolean {
  const claims = artifact.content.sections.flatMap(s => s.claims);
  const coverageReady = artifact.coverage_complete || (artifact.kind === 'insight'
    && artifact.session_ids.length === 1 && claims.length > 0
    && claims.every(c => c.grounding === 'cited' && c.evidence.length > 0));
  return artifact.validation === 'valid' && coverageReady
    && !['suggested_questions','redaction_review'].includes(artifact.kind);
}
