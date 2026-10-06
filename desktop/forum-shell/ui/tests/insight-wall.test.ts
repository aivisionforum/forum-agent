import assert from 'node:assert/strict';
import test from 'node:test';
import { paginateText, wallArtifacts, wallMessage } from '../src/lib/forum/insight-wall.ts';
import { PublicProjection } from '../src/lib/forum/display-state.ts';
import type { PublicArtifact, PublicSnapshot } from '../src/lib/forum/client.ts';
const artifact = (id: string, seq: number): PublicArtifact => ({public_id:id,publication_seq:seq,revision:1,kind:'insight',title:'讨论要点',text:`发言人A：要点 ${id}`,evidence:[]});
const snapshot: PublicSnapshot = {cursor:3,artifacts:[artifact('old',1),{...artifact('minutes',2),kind:'minutes'},artifact('new',3)]};

test('live wall shows newest insights, recap keeps earlier ones and honors withdrawals', () => {
  assert.deepEqual(wallArtifacts(snapshot,'live').map(a=>a.public_id),['new']);
  assert.deepEqual(wallArtifacts(snapshot,'recap').map(a=>a.public_id),['old','new']);
  assert.deepEqual(wallArtifacts({...snapshot,artifacts:[artifact('new',3)]},'recap').map(a=>a.public_id),['new']);
  assert.deepEqual(wallArtifacts(null,'recap'),[]);
});

test('pagination fits measured space, including a single oversized mixed-language card without text loss', () => {
  const text = '发言人A：中英讨论。\n\nLong English paragraph '.repeat(35)+'📝'.repeat(40);
  const pages = paginateText(text,s=>Array.from(s).length<=90);
  assert.ok(pages.length>10);
  assert.equal(pages.join(''),text);
  assert.ok(pages.every(s=>Array.from(s).length<=90 && s.length>0));
  assert.equal(paginateText('',()=>true).length,0);
  const emoji = '👩🏽‍💻';
  assert.deepEqual(paginateText(emoji.repeat(3),s=>s.length<=emoji.length),[emoji,emoji,emoji]);
  assert.throws(()=>paginateText('内容',()=>false),/空间不足/);
});

test('countdown uses server time and never claims completed work when deadline has passed', () => {
  const s: PublicSnapshot = {...snapshot,wall:{phase:'listening',server_time_ms:1000,next_update_at_ms:181000}};
  assert.equal(wallMessage(s,1000).label,'3:00');
  assert.equal(wallMessage(s,181500).label,'等待新一轮整理');
  assert.equal(wallMessage({...s,wall:{...s.wall!,phase:'working'}},1000).label,'WORKING');
  assert.equal(wallMessage({...s,wall:{...s.wall!,phase:'finished',next_update_at_ms:null}},1000).label,'本场已结束');
});

test('public status cannot carry draft content, paths, errors or unknown protocol states', () => {
  const projection = new PublicProjection();
  const wall = {phase:'working',next_update_at_ms:null,server_time_ms:1000};
  projection.accept({...snapshot,wall});
  for (const bad of [{...wall,error:'/private/path'},{...wall,phase:'draft'},{...wall,server_time_ms:NaN}]) {
    assert.throws(()=>projection.accept({...snapshot,wall:bad}),/格式/);
    assert.equal(projection.snapshot,null);
  }
  assert.throws(()=>projection.accept({...snapshot,jobs:[{text:'private'}]}),/格式/);
});
