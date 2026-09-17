import type { ForumTransport } from './transport';
import type { JobRequest } from './client';
import type { PeerSession, PeerSessionState, PublicSessionView, PublicSearchHit } from '../../../../../../packages/contracts/forum.generated';
export type { PeerSession, PeerSessionState, PublicSessionView, PublicSearchHit };
export type CertificateInfo = {directory:string;address:string;ca_certificate_path:string;certificate_sha256:string;ca_sha256:string;expires_days:number};
export type LanState = {enabled:boolean;endpoint:string|null;certificate:CertificateInfo|null;peers:PeerSessionState[];grants:Array<{grant_id:string;source:PeerSession;kind:string;expires_at_ms:number}>;notice:string|null};
export type ParticipantInfo = {grant_id:string;url:string;expires_at_ms:number};
export type PeerInvite = {invite_id:string;endpoint:string;source:PeerSession;invite_token:string;certificate_sha256:string;ca_der_base64:string;expires_at_ms:number};
export type PublishedSelection = {owner_device_id:string;session_id:string;public_id:string;revision:number};
export type PublicAnalysisKind = 'event_report' | 'closing_brief';
export class NetworkClient {
 constructor(readonly transport:ForumTransport){}
 state():Promise<LanState>{return this.transport.call('get_lan_state');}
 certificate(address:string):Promise<CertificateInfo>{return this.transport.call('create_lan_identity',{address});}
 start(address:string,port:number):Promise<LanState>{return this.transport.call('start_lan',{address,port});}
 stop():Promise<LanState>{return this.transport.call('stop_lan');}
 participant(request:{session_id:string;public_title:string;room_name:string;ttl_ms:number}):Promise<ParticipantInfo>{return this.transport.call('create_participant_access',{request});}
 invite(request:{session_id:string;public_title:string;room_name:string}):Promise<PeerInvite>{return this.transport.call('create_peer_invite',{request});}
 pair(invite:PeerInvite,confirmed_fingerprint:string):Promise<PeerSessionState>{return this.transport.call('pair_forum_peer',{request:{invite,confirmed_fingerprint}});}
 revoke(grantId:string):Promise<void>{return this.transport.call('revoke_lan_access',{grantId});}
 disconnect(ownerDeviceId:string,sessionId:string):Promise<void>{return this.transport.call('disconnect_forum_peer',{ownerDeviceId,sessionId});}
 sessions(eventId:string,sessionIds:string[]=[]):Promise<PublicSessionView[]>{return this.transport.call('get_public_sessions',{eventId,sessionIds});}
 search(eventId:string,query:string,sessionIds:string[]=[]):Promise<PublicSearchHit[]>{return this.transport.call('search_public_content',{eventId,sessionIds,query,limit:50});}
}
export function parsePeerInvite(text:string):PeerInvite {
 if (new TextEncoder().encode(text).byteLength>100000) throw new Error('配对邀请过大。');
 const value=JSON.parse(text) as PeerInvite;
 if (!value || !value.source || typeof value.endpoint!=='string' || !value.endpoint.startsWith('https://') || typeof value.certificate_sha256!=='string' || !/^[a-f0-9]{64}$/.test(value.certificate_sha256) || typeof value.invite_token!=='string' || !/^[a-f0-9]{64}$/.test(value.invite_token) || !Number.isSafeInteger(value.expires_at_ms) || value.expires_at_ms<=Date.now()) throw new Error('配对邀请无效或已过期，请向来源会场重新获取。');
 return value;
}
/** A new source revision is never silently reselected for a public-source analysis. */
export function reconcileSelections(selected:PublishedSelection[],sessions:PublicSessionView[]):PublishedSelection[]{
 return selected.filter(s=>sessions.some(room=>!room.stale&&room.owner_device_id===s.owner_device_id&&room.session_id===s.session_id&&room.artifacts.some(a=>a.public_id===s.public_id&&a.revision===s.revision)));
}
/** Capture only exact public identifiers; the core revalidates against current publications. */
export function buildPublicJobRequest(kind:PublicAnalysisKind,eventId:string,currentSessionId:string,selected:PublishedSelection[],sessions:PublicSessionView[],requestId:string):JobRequest {
 const scoped=sessions.filter(room=>room.event_id===eventId);
 if (!selected.length||reconcileSelections(selected,scoped).length!==selected.length) throw new Error('所选公开版本已变更、撤回、离线或不属于此活动，请重新确认。');
 if (!scoped.some(room=>room.local&&room.session_id===currentSessionId&&selected.some(s=>s.owner_device_id===room.owner_device_id&&s.session_id===room.session_id))) throw new Error('请选择当前本机场次的公开版本，以便在此场次审核结果。');
 const chosen=[...new Map(selected.map(s=>[`${s.owner_device_id}:${s.session_id}:${s.public_id}:${s.revision}`,{owner_device_id:s.owner_device_id,session_id:s.session_id,public_id:s.public_id,revision:s.revision}])).values()];
 return {request_id:requestId,session_ids:[...new Set(chosen.map(s=>s.session_id))],kind,automatic:false,public_selections:chosen};
}
