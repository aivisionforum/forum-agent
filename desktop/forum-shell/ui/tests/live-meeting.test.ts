import test from 'node:test';
import assert from 'node:assert/strict';
import { bilingualCaption, captionSpeaker, meetingTopics, latestInsight, rollingInsights } from '../src/lib/forum/live-meeting.ts';
import { captionShare } from '../src/lib/meeting-layout.ts';
import { previewAnalysisState } from '../src/lib/forum/preview.ts';
import type { SpeakerAssignment } from '../src/lib/forum/speakers';

test('live labels follow durable segment identity and exact revision, never similar text or another session', () => {
  const assignment = {session_id:'a',segment_id:'segment',source_revision:2,current:true,label:{kind:'anonymous',speaker_id:'person'}} as SpeakerAssignment;
  assert.equal(captionSpeaker([assignment],'a','segment',2),assignment);
  assert.equal(captionSpeaker([assignment],'b','segment',2),null);
  assert.equal(captionSpeaker([assignment],'a','segment',1),null);
  assert.equal(captionSpeaker([assignment],'a','another',2),null);
  assert.equal(captionSpeaker([{...assignment,current:false}],'a','segment',2),null);
  assert.equal(captionSpeaker([assignment],null,'segment',2),null);
  assert.equal(captionSpeaker([assignment],'a'),null);
});

test('live insights use the current session, newest available revision and remove withdrawn or stale drafts', () => {
  const state=structuredClone(previewAnalysisState);
  const original=state.artifacts[0];
  const newer={...original,artifact_id:'newer',created_at_ms:100};
  state.artifacts.push(newer,{...newer,revision:2});
  assert.equal(latestInsight(state,state.session_id)?.revision,2);
  assert.equal(latestInsight(state,'another'),null);
  for(const publication of ['hidden','withdrawn'] as const) {
    assert.equal(latestInsight({...state,artifacts:[{...newer,publication}]},state.session_id),null);
  }
  for(const validation of ['invalid','stale'] as const) {
    assert.equal(latestInsight({...state,artifacts:[{...newer,validation}]},state.session_id),null);
  }
  assert.equal(latestInsight({...state,artifacts:[{...newer,review:'rejected'}]},state.session_id),null);
  assert.equal(latestInsight({...state,artifacts:[{...newer,session_ids:['other']}]},state.session_id),null);
});

test('bilingual captions keep Chinese and English in stable lanes across speaker language changes', () => {
  const chinese={sourceText:'预算十二万',translation:'The budget is 120,000.',sourceLanguage:'zh',languageTexts:{en:'The budget is 120,000.'}};
  const english={sourceText:'We have not approved it.',translation:'还没有批准。',sourceLanguage:'en',languageTexts:{zh:'还没有批准。'}};
  assert.equal(bilingualCaption(chinese).sourceText,'预算十二万');
  assert.equal(bilingualCaption(chinese).translation,'The budget is 120,000.');
  assert.equal(bilingualCaption(english).sourceText,'还没有批准。');
  assert.equal(bilingualCaption(english).translation,'We have not approved it.');
  assert.equal(bilingualCaption({...english,languageTexts:{}}).sourceText,'');
  assert.equal(english.sourceText,'We have not approved it.');
});

test('legacy topics group source-backed domains instead of splitting ordinary words', () => {
  const state=structuredClone(previewAnalysisState);
  const artifact=state.artifacts[0];
  const base=artifact.content.sections[0].claims[0];
  artifact.content.sections=[{heading:'旧洞察', claims:[
    {...base,claim_id:'one',text:'课堂上的触摸屏能否提高学生的学习兴趣？'},
    {...base,claim_id:'two',text:'用户使用设备，存在过滤问题。'},
    {...base,claim_id:'three',text:'政府应该支持医院。',grounding:'unsupported',evidence:[]}
  ]}];
  const topics=meetingTopics(state,state.session_id);
  assert.ok(topics.some(t=>t.text==='教育应用' && t.claimIds.includes('one')));
  assert.ok(!topics.some(t=>['用户','使用','设备','存在','过滤','政府','医疗'].includes(t.text)));
  assert.ok(!topics.some(t=>t.claimIds.includes('three')));
  assert.deepEqual(meetingTopics(null,state.session_id),[]);
});

test('whole-meeting topics retain early domains across pages and ignore a final small-talk insight', () => {
  const state=structuredClone(previewAnalysisState);
  const template=state.artifacts[0];
  const base=template.content.sections[0].claims[0];
  const evidence=base.evidence;
  const early={...structuredClone(template),created_at_ms:1,content:{title:'开场',sections:[
    {heading:'主题',claims:[{...base,claim_id:'early',text:'讨论公共教育政策。'}],topics:[
      {label:'教育',evidence},{label:'政府',evidence}]}]}};
  const later=Array.from({length:35},(_,i)=>({...structuredClone(template),artifact_id:`later-${i}`,created_at_ms:i+2,
    content:{title:'更新',sections:[{heading:'更新',claims:[{...base,claim_id:`small-talk-${i}`,text:'The speaker says thank you.',evidence:[],grounding:'unsupported' as const}]}]}}));
  state.artifacts=[...later,early];
  assert.deepEqual(meetingTopics(state,state.session_id).map(t=>t.text),['教育','政府']);
  assert.ok(meetingTopics(state,state.session_id).every(t=>t.claimIds.includes('early')));
  assert.deepEqual(meetingTopics(state,'other-session'),[]);
  for (const excluded of [{publication:'hidden'}, {publication:'withdrawn'}, {review:'rejected'}, {validation:'stale'}] as const) {
    state.artifacts=[...later,early,{...early,...excluded,revision:early.revision+1}];
    assert.deepEqual(meetingTopics(state,state.session_id),[]);
  }
});

test('model topics can describe source omitted from claims, merge aliases and deduplicate overlap', () => {
  const state=structuredClone(previewAnalysisState);
  const artifact=state.artifacts[0];
  const base=artifact.content.sections[0].claims[0];
  artifact.created_at_ms=1;
  artifact.content.sections=[{heading:'本轮',claims:[],topics:[
    {label:'教育教学',evidence:base.evidence}, {label:'航天探索',evidence:base.evidence},
    {label:'speaker',evidence:base.evidence}, {label:'存在',evidence:base.evidence},
    {label:'政府',evidence:[]}
  ]}];
  const newer=structuredClone(artifact); newer.artifact_id='repeat'; newer.created_at_ms=2;
  newer.content.sections[0].topics=[{label:'教育应用',evidence:base.evidence}];
  state.artifacts=[newer,artifact];
  assert.deepEqual(meetingTopics(state,state.session_id),[
    {text:'教育应用',count:1,claimIds:[]},{text:'航天探索',count:1,claimIds:[]}
  ]);
});

test('rolling notes retain earlier ideas, deduplicate exact repeats and keep disagreements separate', () => {
  const state=structuredClone(previewAnalysisState);
  const old=state.artifacts[0]; old.created_at_ms=100;
  const base=old.content.sections[0].claims[0];
  const newer={...structuredClone(old),artifact_id:'next',created_at_ms:200};
  newer.content.sections=[{heading:'更新',claims:[{...base,claim_id:'repeat'},
    {...base,claim_id:'opposing',text:'暂不测试字幕延迟。'}]}];
  state.artifacts=[old,newer];
  const notes=rollingInsights(state,state.session_id);
  assert.equal(notes.length,3);
  assert.equal(notes.find(c=>c.text===base.text)?.firstSeen,100);
  assert.equal(notes.find(c=>c.text===base.text)?.lastSeen,200);
  assert.ok(notes.some(c=>c.text==='暂不测试字幕延迟。'));
  state.artifacts.push({...newer,revision:2,publication:'hidden'});
  assert.equal(rollingInsights(state,state.session_id).length,2);
  assert.ok(rollingInsights(state,state.session_id).every(c=>c.lastSeen===100));
  state.artifacts=[{...old,validation:'stale'},newer,{...newer,revision:3,review:'rejected'}];
  assert.deepEqual(rollingInsights(state,state.session_id),[]);
  assert.deepEqual(rollingInsights({...state,session_id:'other'},old.session_ids[0]),[]);
});

test('splitter leaves both panels usable at window sizes and rejects invalid stored values', () => {
  for (const width of [850,1180,2500,5160]) {
    assert.ok(captionShare(0,width)*(width-8)>=279.9);
    assert.ok((1-captionShare(1,width))*(width-8)>=299.9);
    assert.equal(captionShare(NaN,width),captionShare(.62,width));
  }
  assert.ok(captionShare(.2,2500)<.3);
});


test('topic phrases keep concrete subjects and reject generic AI, Data and process labels', () => {
  const state=structuredClone(previewAnalysisState);const artifact=state.artifacts[0];
  const evidence=artifact.content.sections[0].claims[0].evidence;
  artifact.content.sections=[{heading:'主题',claims:[],topics:[
    ...['AI','Data','人工智能','数据','how to','process','change','traditional','speaker'].map(label=>({label,evidence})),
    {label:'知识产权变现',evidence},{label:'Build vs buy',evidence}
  ]}];
  assert.deepEqual(meetingTopics(state,state.session_id).map(t=>t.text),['知识产权变现','Build vs buy']);
});
