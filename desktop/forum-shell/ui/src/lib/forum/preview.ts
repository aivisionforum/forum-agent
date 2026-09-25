import type { AnalysisState, ArtifactRecord, MeetingPage, SessionPage } from './client';
import type { AnalysisConfig, AnalysisJob, SessionSummary } from '../../../../../../packages/contracts/forum.generated';
const sessionId = '00000000-0000-4000-8000-000000000001';
const segmentId = '00000000-0000-4000-8000-000000000002';
const artifactId = '00000000-0000-4000-8000-000000000003';
const text = '本周先测试字幕延迟与识别准确率。字幕延迟能否控制在三秒内？预算尚未批准，公开内容需要人工审核。';
const span = {segment_id:segmentId,segment_revision:1,start_utf8:0,end_utf8:new TextEncoder().encode(text).length,quote:text};
const config: AnalysisConfig = {model_profile:'preview',model_manifest_id:'preview-only',prompt_version:'preview',prompt_sha256:'preview',profile_id:'ai-vision-forum',profile_version:'preview',profile_sha256:'preview',effective_config_hash:'preview',projection_policy_hash:'preview',generation:{}};
const session: SessionSummary = {
  session:{session_id:sessionId,event_id:'00000000-0000-4000-8000-000000000010',room_id:'00000000-0000-4000-8000-000000000011',owner_device_id:'00000000-0000-4000-8000-000000000012',title:'界面预览 · 论坛圆桌（合成）'},
  status:{session_id:sessionId,state:'completed',direction_epoch:1,target_languages:[],capture_stopped:true,transcript_sealed:true,incomplete:false,state_seq:1},cursor:1,segment_count:1,translation_pending:0
};
const artifact: ArtifactRecord = {
  artifact_id:artifactId,revision:1,kind:'insight',event_id:session.session.event_id,session_ids:[sessionId],job_id:'00000000-0000-4000-8000-000000000004',snapshot_id:'00000000-0000-4000-8000-000000000005',
  content:{title:'合成草稿：先验证会场，再确认发布',sections:[{heading:'讨论中的关键点',claims:[
    {claim_id:'00000000-0000-4000-8000-000000000006',kind:'action',text:'先测试字幕延迟与识别准确率，再进行会场联调。',evidence:[{kind:'source',session_id:sessionId,span}],grounding:'cited',assignee:null,due:'本周'},
    {claim_id:'00000000-0000-4000-8000-000000000007',kind:'question',text:'字幕延迟能否控制在三秒内？',evidence:[{kind:'source',session_id:sessionId,span}],grounding:'cited',assignee:null,due:null}
  ]}]},coverage:{units:[{target:{kind:'source',segment_id:segmentId,segment_revision:1},start_utf8:0,end_utf8:span.end_utf8,status:'processed',reason:null}]},coverage_complete:true,validation:'valid',review:'draft',publication:'private',config,created_at_ms:0,operator_id:null,reason:null
};
const job: AnalysisJob = {job_id:artifact.job_id,request_id:'00000000-0000-4000-8000-000000000008',kind:'insight',event_id:artifact.event_id,session_ids:[sessionId],state:'succeeded',attempt:1,max_attempts:3,budget_ms:180000,created_at_ms:0,deadline_at_ms:180000,started_at_ms:0,snapshot_id:artifact.snapshot_id,snapshot_sha256:'preview',config,automatic:false,progress:{phase:'合成示例 · 草稿已生成',completed_units:1,total_units:1,wait_reason:null},error:null,result:{artifact_id:artifactId,revision:1}};
export const previewSessionPage: SessionPage = {items:[session],next_after:null};
export const previewMeetingPage: MeetingPage = {session:session.session,status:session.status,cursor:1,next_after:null,translations:[],items:[{segment_id:segmentId,track_id:'00000000-0000-4000-8000-000000000009',audio:{start_sample:0,end_sample:96000,sample_rate:16000,start_ms:0,end_ms:6000},recording_ref:null,transcript:{session_id:sessionId,payload:{track_id:'00000000-0000-4000-8000-000000000009',segment_id:segmentId,revision:1,audio:{start_sample:0,end_sample:96000,sample_rate:16000,start_ms:0,end_ms:6000},text,configured_source_language:'auto',detected_language:'zh',target_languages:[],direction_epoch:1,speaker_id:null,status:'success',reason:null,backend:'preview',model_manifest_id:'preview'},origin:{kind:'asr'},created_seq:1}}]};
export const previewAnalysisState: AnalysisState = {session_id:sessionId,cursor:1,jobs:[job,{...job,job_id:'00000000-0000-4000-8000-000000000020',kind:'minutes',state:'waiting',progress:{phase:'合成示例 · 等待后台资源',completed_units:0,total_units:4,wait_reason:'实时翻译优先；等待本机模型资源。'},result:null}],artifacts:[artifact,{...artifact,artifact_id:'00000000-0000-4000-8000-000000000021',kind:'minutes',content:{...artifact.content,title:'合成纪要 · 会场联调讨论'}}],next_jobs:null,next_artifacts:null};
export const previewEvidence = {session_id:sessionId,text,segment_id:segmentId,segment_revision:1,start_ms:0,current:true};

export const previewSpeakerAssignments: import('./speakers').SpeakerAssignment[] = [segmentId,artifactId].map((id,index) => ({
  session_id:sessionId,segment_id:id,source_revision:1,revision:1,current:true,created_at_ms:0,
  label:{kind:'anonymous',speaker_id:index ? 'b22222bb-0000-4000-8000-000000000002' : 'a11111aa-0000-4000-8000-000000000001'},
  origin:{kind:'automatic',model_manifest_id:'synthetic-preview',model_version:'preview'}
}));
