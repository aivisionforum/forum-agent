import test from 'node:test';
import assert from 'node:assert/strict';
import {parsePeerInvite,reconcileSelections,buildPublicJobRequest,NetworkClient,type PublicSessionView} from '../src/lib/forum/network.ts';
import {ForumClient} from '../src/lib/forum/client.ts';
const selection={owner_device_id:'owner',session_id:'session',public_id:'public',revision:1};
const room={...selection,event_id:'event',title:'公开',room_name:'A',local:false,stale:false,last_sync_at_ms:10,cursor:7,artifacts:[{public_id:'public',revision:1,kind:'minutes',title:'公开',text:'文本',evidence:[],publication_seq:7}]} as unknown as PublicSessionView;
test('public analysis selection is removed when source is stale, withdrawn, revised or changes owner',()=>{
 assert.deepEqual(reconcileSelections([selection],[room]),[selection]);
 for(const next of [{...room,stale:true},{...room,artifacts:[]},{...room,owner_device_id:'another-owner'},{...room,artifacts:[{...room.artifacts[0],revision:2}]}]) assert.deepEqual(reconcileSelections([selection],[next]),[]);
});
test('report and closing requests preserve exact local and peer versions and reject stale or foreign activity selections',async()=>{
 const local={...room,local:true};
 const peer={...room,owner_device_id:'peer-owner',session_id:'peer-session',artifacts:[{...room.artifacts[0],public_id:'peer-public',revision:4}]};
 const chosen=[{...selection},{owner_device_id:'peer-owner',session_id:'peer-session',public_id:'peer-public',revision:4}];
 const calls:Array<[string,unknown]>=[];
 const client=new ForumClient({mode:'desktop',async call<T>(command:string,args?:Record<string,unknown>){calls.push([command,args]);return {} as T;}});
 for(const kind of ['event_report','closing_brief'] as const){
  const request=buildPublicJobRequest(kind,'event','session',chosen,[local,peer],`request-${kind}`);
  await client.createJob(request);
  assert.deepEqual(calls.at(-1),['create_analysis_job',{request:{request_id:`request-${kind}`,session_ids:['session','peer-session'],kind,automatic:false,public_selections:chosen}}]);
  assert.notEqual(request.public_selections?.[0],chosen[0]);
  for(const unavailable of [{...peer,stale:true},{...peer,event_id:'other-event'},{...peer,artifacts:[]},{...peer,artifacts:[{...peer.artifacts[0],revision:5}]}]) assert.throws(()=>buildPublicJobRequest(kind,'event','session',chosen,[local,unavailable],'blocked'));
  assert.throws(()=>buildPublicJobRequest(kind,'event','session',chosen.slice(1),[local,peer],'no-local'));
 }
 assert.equal(calls.length,2);
});
test('peer invite parsing rejects expired, malformed and oversized capabilities',()=>{
 const invite={invite_id:'id',endpoint:'https://127.0.0.1:1000',source:{owner_device_id:'owner',event_id:'event',session_id:'session',title:'public',room_name:'A'},invite_token:'a'.repeat(64),certificate_sha256:'b'.repeat(64),ca_der_base64:'certificate',expires_at_ms:Date.now()+60000};
 assert.equal(parsePeerInvite(JSON.stringify(invite)).source.title,'public');
 assert.throws(()=>parsePeerInvite(JSON.stringify({...invite,expires_at_ms:1})));
 assert.throws(()=>parsePeerInvite(JSON.stringify({...invite,endpoint:'http://127.0.0.1:1000'})));
 assert.throws(()=>parsePeerInvite(JSON.stringify({...invite,certificate_sha256:'wrong'})));
 assert.throws(()=>parsePeerInvite('中'.repeat(40000)));
});
test('public search stays scoped and pairing uses a distinct explicit confirmation field',async()=>{
 const calls:Array<[string,unknown]>=[];
 const client=new NetworkClient({mode:'desktop',async call<T>(command:string,args?:Record<string,unknown>){calls.push([command,args]);return [] as T;}});
 await client.search('event','公开',['one-session']);
 assert.deepEqual(calls[0],['search_public_content',{eventId:'event',sessionIds:['one-session'],query:'公开',limit:50}]);
 await client.revoke('grant');assert.deepEqual(calls[1],['revoke_lan_access',{grantId:'grant'}]);
});
