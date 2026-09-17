import assert from 'node:assert/strict';
import test from 'node:test';
import { SessionFence, evidenceParts } from '../src/lib/forum/session-fence.ts';
import { DesktopTransport, DisplayTransport } from '../src/lib/forum/transport.ts';

test('late selection responses cannot overwrite B or a later selection of A', async () => {
  const fence = new SessionFence();
  fence.select('A'); const firstA = fence.capture();
  fence.select('B'); const b = fence.capture();
  fence.select('A'); const lastA = fence.capture();
  assert.equal(firstA(), false); assert.equal(b(), false); assert.equal(lastA(), true);
  fence.dispose(); assert.equal(lastA(), false);
});

test('evidence uses exact UTF8 slices including emoji and combining text, rejecting stale or split bytes', () => {
  const text = '证据👩🏽‍💻é结束'; const quote = '👩🏽‍💻é';
  const start = new TextEncoder().encode('证据').length;
  const end = start + new TextEncoder().encode(quote).length;
  assert.deepEqual(evidenceParts(text, start, end, quote), { before: '证据', quote, after: '结束' });
  assert.equal(evidenceParts(text, start + 1, end, quote), null);
  assert.equal(evidenceParts(text, start, end, 'other revision'), null);
  assert.equal(evidenceParts(text, 0, 9999, text), null);
});

test('desktop transport preserves typed request payload rather than leaking it into global state', async () => {
  const calls: unknown[] = [];
  const desktop = new DesktopTransport(async <T>(command: string, args?: Record<string, unknown>) => { calls.push([command,args]); return {} as T; });
  await desktop.call('review_artifact', { request: { session_id: 'A', expected_revision: 2 } });
  assert.deepEqual(calls, [['review_artifact', { request: { session_id:'A', expected_revision:2 } }]]);
});

test('display rejects every write and sends credentials only in a bearer header', async () => {
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  const transport = new DisplayTransport('http://127.0.0.1:8211', 'secret', (async (url: URL | RequestInfo, init?: RequestInit) => {
    calls.push({url:String(url), init}); return new Response(JSON.stringify({cursor: 9}), {status:200});
  }) as typeof fetch);
  await assert.rejects(transport.call('publish_artifact', { request: {} }), /仅可读取/);
  assert.equal(calls.length, 0);
  await transport.call('get_public_snapshot', { sessionId: 'e7b8381a-fb48-4942-b87d-ffbbbd9cc170', after: 8 });
  assert.ok(calls[0].url.endsWith('/snapshot?after=8'));
  assert.ok(!calls[0].url.includes('secret'));
  assert.equal((calls[0].init?.headers as Record<string,string>).Authorization, 'Bearer secret');
  assert.equal(calls[0].init?.cache, 'no-store');
});

import { PublicProjection, displayCredentials } from '../src/lib/forum/display-state.ts';
import { canPublish } from '../src/lib/forum/client.ts';
import { previewAnalysisState } from '../src/lib/forum/preview.ts';

test('public snapshots replace withdrawn cards, and any stale/error state removes visible content', () => {
  const state = new PublicProjection();
  const card = {public_id:'published-id',revision:1,kind:'insight',title:'<script>alert(1)</script>',text:'plain text',evidence:[],publication_seq:1};
  state.accept({cursor:1,artifacts:[card]});
  assert.equal(state.snapshot?.artifacts[0].title,'<script>alert(1)</script>');
  state.accept({cursor:2,artifacts:[]});
  assert.equal(state.snapshot?.artifacts.length,0);
  assert.throws(() => state.accept({cursor:1,artifacts:[card]}),/版本/);
  assert.equal(state.snapshot,null);
  state.accept({cursor:3,artifacts:[card]}); state.disconnect();
  assert.equal(state.snapshot,null); assert.equal(state.cursor,3);
  assert.throws(() => state.accept({cursor:4,artifacts:[{...card,source_spans:[{quote:'private'}]}]}),/格式/);
  assert.equal(state.snapshot,null);
});

test('display refresh uses tab-scoped token and strips it from the visible URL', () => {
  const values = new Map<string,string>();
  const storage = {getItem:(key:string) => values.get(key) ?? null,setItem:(key:string,value:string) => {values.set(key,value);}};
  const url = 'http://127.0.0.1:8123/display.html?session=e7b8381a-fb48-4942-b87d-ffbbbd9cc170#token=read-only-secret';
  const first = displayCredentials(new URL(url),storage);
  assert.equal(first.token,'read-only-secret'); assert.ok(!first.cleanUrl.includes('secret'));
  assert.equal(displayCredentials(new URL(first.cleanUrl),storage).token,'read-only-secret');
  assert.throws(() => displayCredentials(new URL('http://127.0.0.1:8123/display.html?session=62dd4406-b46e-45fa-9615-c1ea9c6ee51d'),storage),/缺少凭据/);
});

test('UI publication affordance requires current validation, explicit approval and complete coverage', () => {
  const a = structuredClone(previewAnalysisState.artifacts[0]);
  assert.equal(canPublish(a),false);
  a.review = 'approved'; assert.equal(canPublish(a),true);
  a.coverage_complete = false; assert.equal(canPublish(a),false);
  a.coverage_complete = true; a.validation = 'stale'; assert.equal(canPublish(a),false);
  a.validation = 'valid'; a.kind = 'suggested_questions'; assert.equal(canPublish(a),false);
});

import { readLegacyFile, MAX_LEGACY_IMPORT_BYTES } from '../src/lib/forum/import.ts';

test('legacy import rejects oversized input before reading and preserves original text verbatim', async () => {
  let read = false;
  await assert.rejects(readLegacyFile({name:'large.jsonl',size:MAX_LEGACY_IMPORT_BYTES+1,text:async () => {read=true;return '';}}),/16 MiB/);
  assert.equal(read,false);
  const content = '{"text":"不要发布"}\n{"text":"<script>private</script>"}\n';
  const result = await readLegacyFile({name:'旧会议.jsonl',size:new TextEncoder().encode(content).length,text:async () => content});
  assert.deepEqual(result,{title:'旧会议',content});
  await assert.rejects(readLegacyFile({name:'empty.jsonl',size:2,text:async () => '  '}),/没有可导入/);
});
