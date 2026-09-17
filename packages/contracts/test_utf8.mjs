// Compile utf8.ts and forum.generated.ts to a temporary directory before running.
import test from 'node:test';
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
const {resolveSourceSpan,utf16SelectionToUtf8} = await import(pathToFileURL(process.env.FORUM_CONTRACTS_JS).href);
const span={segment_id:'00000000-0000-4000-8000-000000000001',segment_revision:1,start_utf8:3,end_utf8:7,quote:'📝'};
test('emoji and combining marks preserve exact byte provenance',()=>{
  assert.equal(resolveSourceSpan('中📝e\u0301文',span),'📝');
  assert.deepEqual(utf16SelectionToUtf8('中📝e\u0301文',1,3),{start_utf8:3,end_utf8:7});
  assert.equal(resolveSourceSpan('中📝e\u0301文',{...span,start_utf8:7,end_utf8:10,quote:'e\u0301'}),'e\u0301');
});
test('split code points, split surrogate pairs, wrong quotes and unsafe numbers fail',()=>{
  assert.throws(()=>resolveSourceSpan('中📝文',{...span,start_utf8:4}));
  assert.throws(()=>resolveSourceSpan('中📝文',{...span,quote:'x'}));
  assert.throws(()=>resolveSourceSpan('中📝文',{...span,end_utf8:Number.MAX_SAFE_INTEGER+1}));
  assert.throws(()=>utf16SelectionToUtf8('中📝文',1,2));
});
test('source whitespace is preserved instead of creating coverage gaps',()=>{
  assert.equal(resolveSourceSpan(' x ',{...span,start_utf8:0,end_utf8:1,quote:' '}),' ');
});
