import test from 'node:test';
import assert from 'node:assert/strict';
import {currentSpeaker,speakerText,type SpeakerAssignment} from '../src/lib/forum/speakers.ts';
test('speaker labels never cross sessions, stale source revisions or unsupported overlaps',()=>{
 const value={session_id:'one',segment_id:'segment',source_revision:2,revision:1,label:{kind:'anonymous',speaker_id:'abcd12-rest'},origin:{kind:'human',operator_id:'operator',reason:'checked audio'},created_at_ms:0,current:true} as SpeakerAssignment;
 assert.equal(currentSpeaker([value],'one','segment',2),value);
 assert.equal(currentSpeaker([value],'two','segment',2),null);
 assert.equal(currentSpeaker([value],'one','segment',3),null);
 assert.equal(currentSpeaker([{...value,current:false}],'one','segment',2),null);
 assert.equal(speakerText(value.label),'匿名 abcd12');
 assert.equal(speakerText({kind:'overlap'}),'重叠发言');
 assert.equal(speakerText({kind:'unknown'}),'说话人未知');
});
