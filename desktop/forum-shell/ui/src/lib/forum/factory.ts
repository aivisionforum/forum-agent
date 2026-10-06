import { invoke } from '@tauri-apps/api/core';
import { isTauri } from '../api';
import { previewSessionPage, previewMeetingPage, previewAnalysisState, previewEvidence } from './preview';
import { ForumClient } from './client';
import { previewLanState,previewPublicRooms } from './network-preview';
import { DesktopTransport, type ForumTransport } from './transport';

class PreviewTransport implements ForumTransport {
  readonly mode = 'preview' as const;
  async call<T>(command: string, args: Record<string,unknown> = {}): Promise<T> {
    if (command === 'get_lan_state') return structuredClone(previewLanState) as T;
    if (command === 'get_public_sessions') return previewPublicRooms(previewSessionPage.items[0].session) as T;
    if (command === 'list_meeting_sessions_page') return structuredClone(previewSessionPage) as T;
    if (command === 'get_meeting_transcript') return structuredClone(previewMeetingPage) as T;
    if (command === 'get_analysis_state') return structuredClone(previewAnalysisState) as T;
    if (command === 'prepare_insight_publication') {
      const a = previewAnalysisState.artifacts.find(a => a.artifact_id === args.artifactId)!;
      return {artifact_id:a.artifact_id,expected_revision:a.revision,operator_id:'preview',reason:'Synthetic preview',
        policy_hash:a.config.projection_policy_hash,reviewed_title:'讨论要点',
        reviewed_text:a.content.sections.flatMap(s => s.claims).map(c => `发言人A：${c.text}`).join('\n\n'),
        evidence:a.content.sections.flatMap(s => s.claims).flatMap(c => c.evidence.map(evidence => ({evidence,reviewed_text:'依据发言人A的本场发言整理。'})))} as T;
    }
    if (command === 'get_insight_settings') return {mode:'gated'} as T;
    if (command === 'get_analysis_artifact') return structuredClone(previewAnalysisState.artifacts.find(a => a.artifact_id === args.artifactId) ?? null) as T;
    if (command === 'get_analysis_evidence') return structuredClone(previewEvidence) as T;
    throw new Error('界面预览不创建或修改真实会议，请在桌面应用中操作。');
  }
}
export const forumClient = new ForumClient(isTauri() ? new DesktopTransport(invoke) : new PreviewTransport());
