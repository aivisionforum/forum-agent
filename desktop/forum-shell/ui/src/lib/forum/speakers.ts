import type {SpeakerAssignment,SpeakerAssignmentCommand,SpeakerLabel} from '../../../../../../packages/contracts/forum.generated';
import type {ForumTransport} from './transport';
export type {SpeakerAssignment,SpeakerLabel};
export type SpeakerStatus={enabled:boolean;session_id:string|null;state:string;notice:string|null;active_segment:string|null;completed:number;skipped:number};
export class SpeakerClient {
 constructor(readonly transport:ForumTransport){}
 status():Promise<SpeakerStatus>{return this.transport.call('get_speaker_status');}
 assignments(sessionId:string):Promise<SpeakerAssignment[]>{return this.transport.call('get_speaker_assignments',{sessionId});}
 enable(sessionId:string,modelPath:string|null):Promise<SpeakerStatus>{return this.transport.call('enable_session_speakers',{sessionId,modelPath});}
 disable():Promise<SpeakerStatus>{return this.transport.call('disable_session_speakers');}
 correct(command:SpeakerAssignmentCommand):Promise<SpeakerAssignment>{return this.transport.call('correct_speaker_assignment',{command});}
}
export function speakerText(label:SpeakerLabel, locale:'zh'|'en'='zh'):string {return label.kind==='unknown'?(locale==='en'?'Unknown speaker':'说话人未知'):label.kind==='overlap'?(locale==='en'?'Overlapping speech':'重叠发言'):`${locale==='en'?'Anonymous':'匿名'} ${label.speaker_id.slice(0,6)}`;}
export function currentSpeaker(assignments:SpeakerAssignment[],sessionId:string,segmentId:string,sourceRevision:number|null):SpeakerAssignment|null{return assignments.find(a=>a.current&&a.session_id===sessionId&&a.segment_id===segmentId&&a.source_revision===sourceRevision)??null;}
