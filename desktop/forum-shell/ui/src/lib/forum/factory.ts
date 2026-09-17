import { invoke } from '@tauri-apps/api/core';
import { isTauri } from '../api';
import { previewSessionPage, previewMeetingPage, previewAnalysisState, previewEvidence } from './preview';
import { ForumClient } from './client';
import { DesktopTransport, type ForumTransport } from './transport';

class PreviewTransport implements ForumTransport {
  readonly mode = 'preview' as const;
  async call<T>(command: string, args: Record<string,unknown> = {}): Promise<T> {
    if (command === 'list_meeting_sessions_page') return structuredClone(previewSessionPage) as T;
    if (command === 'get_meeting_transcript') return structuredClone(previewMeetingPage) as T;
    if (command === 'get_analysis_state') return structuredClone(previewAnalysisState) as T;
    if (command === 'get_analysis_artifact') return structuredClone(previewAnalysisState.artifacts.find(a => a.artifact_id === args.artifactId) ?? null) as T;
    if (command === 'get_analysis_evidence') return structuredClone(previewEvidence) as T;
    throw new Error('界面预览不创建或修改真实会议，请在桌面应用中操作。');
  }
}
export const forumClient = new ForumClient(isTauri() ? new DesktopTransport(invoke) : new PreviewTransport());
