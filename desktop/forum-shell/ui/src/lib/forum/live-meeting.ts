import type { AnalysisState, ArtifactRecord, AnalysisClaim } from './client';
import type { SpeakerAssignment } from './speakers';
import { topicsFromHistory, type MeetingTopic } from './meeting-topics.js';

/** Never associate a label by text: repeated words and revised ASR are common. */
export function captionSpeaker(assignments: SpeakerAssignment[], sessionId: string | null,
  segmentId?: string | null, revision?: number | null): SpeakerAssignment | null {
  if (!sessionId || !segmentId || revision == null) return null;
  return assignments.find(a => a.current && a.session_id === sessionId
    && a.segment_id === segmentId && a.source_revision === revision) ?? null;
}

export function visibleInsights(state: AnalysisState | null, sessionId: string | null): ArtifactRecord[] {
  if (!state || !sessionId || state.session_id !== sessionId) return [];
  const current = new Map<string,ArtifactRecord>();
  for (const a of state.artifacts) if (!current.has(a.artifact_id) || current.get(a.artifact_id)!.revision < a.revision) current.set(a.artifact_id,a);
  return [...current.values()].filter(a => a.kind === 'insight' && a.session_ids.includes(sessionId)
    && !['hidden', 'withdrawn'].includes(a.publication) && a.review !== 'rejected'
    && !['invalid', 'stale'].includes(a.validation))
    .sort((a, b) => b.created_at_ms - a.created_at_ms || b.revision - a.revision);
}
export function latestInsight(state: AnalysisState | null, sessionId: string | null): ArtifactRecord | null {
  return visibleInsights(state,sessionId)[0] ?? null;
}

export type RollingInsight = AnalysisClaim & {key:string; firstSeen:number; lastSeen:number; review:ArtifactRecord['review']; artifactId:string};
/** Retain earlier evidence-backed notes, merging only text-identical claims.
 * Opposing statements remain distinct; a repeated model claim is not consensus.
 * Rebuild from current artifacts so withdrawn/revised evidence cannot linger.
 */
export function rollingInsights(state: AnalysisState | null, sessionId: string | null): RollingInsight[] {
  const notes = new Map<string,RollingInsight>();
  for (const artifact of visibleInsights(state,sessionId).reverse()) {
    for (const claim of artifact.content.sections.flatMap(s => s.claims)) {
      const key = JSON.stringify([claim.kind,claim.text.trim().replace(/\s+/g,' ').toLowerCase(),claim.assignee,claim.due]);
      notes.set(key,{...claim,key,firstSeen:notes.get(key)?.firstSeen ?? artifact.created_at_ms,lastSeen:artifact.created_at_ms,review:artifact.review,artifactId:artifact.artifact_id});
    }
  }
  return [...notes.values()].sort((a,b) => b.lastSeen-a.lastSeen || b.firstSeen-a.firstSeen);
}

/** All current insight artifacts, including topics not selected as individual notes. */
export function meetingTopics(state: AnalysisState | null, sessionId: string | null): MeetingTopic[] {
  return topicsFromHistory(visibleInsights(state, sessionId), rollingInsights(state, sessionId));
}

export function friendlyAnalysisError(message: string): string {
  if (message.includes('snapshot_stale')) return '原文已更新，请重新生成这一轮洞察。';
  if (message.includes('INVALID_MODEL_OUTPUT')) return '这一轮内容未通过引用核验，请重试；上一轮有效洞察仍会保留。';
  if (message.includes('analysis_empty_input') || message.includes('DEPENDENCY_NOT_READY')) return '还没有足够的有效原文，收到更多内容后再生成。';
  if (message.includes('DEADLINE_EXCEEDED')) return '这一轮等待超时，可在语音识别空闲时重试。';
  if (message.includes('MODEL_UNAVAILABLE')) return '洞察模型尚未就绪，请在主页检查会议助理设置。';
  return message;
}

export function bilingualCaption<T extends {sourceText:string;translation:string;sourceLanguage?:string;languageTexts?:Record<string,string>}>(sentence:T): T {
  return {...sentence,
    sourceText:sentence.languageTexts?.zh ?? (sentence.sourceLanguage === 'zh' ? sentence.sourceText : ''),
    translation:sentence.languageTexts?.en ?? (sentence.sourceLanguage === 'en' ? sentence.sourceText : '')};
}
