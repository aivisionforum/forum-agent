//! A bounded look-ahead window joins short ASR cuts without losing durable tails.
use forum_contracts::{TranslationRequested, Uuid};
use std::time::{Duration, Instant};
pub struct PhraseWindow { pending: Option<((Uuid, u32, usize, String, u64), Instant)>, max_wait: Duration }
impl Default for PhraseWindow {
    fn default() -> Self { Self { pending: None, max_wait: Duration::from_millis(3500) } }
}
impl PhraseWindow {
    #[cfg(test)]
    pub fn immediate() -> Self { Self { pending: None, max_wait: Duration::ZERO } }
    pub fn defer(&mut self, request: &TranslationRequested, now: Instant) -> bool {
        let first = &request.source_spans[0];
        let key=(first.segment_id,first.segment_revision.get(),first.start_utf8,request.target_language.clone(),request.direction_epoch);
        let started=match &self.pending {Some((old,started)) if old==&key=>*started,_=>{self.pending=Some((key,now));now}};
        let source=request.input_text.trim();
        let complete=source.ends_with(['.', '!', '?', '。', '！', '？'])
            && (source.split_whitespace().count()>=12 || source.chars().filter(|c| matches!(*c as u32,0x4e00..=0x9fff)).count()>=18);
        let combined=request.source_spans.iter().any(|s|s.segment_id!=first.segment_id);
        let wait=!combined && !complete && now.duration_since(started)<self.max_wait;
        if !wait { self.pending=None; }
        wait
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use forum_contracts::{Revision,SourceSpan};
    fn request(text:&str)->TranslationRequested {
        TranslationRequested {translation_id:Uuid::new_v4(),revision:Revision::FIRST,attempt:1,target_language:"zh".into(),direction_epoch:1,
            source_spans:vec![SourceSpan{segment_id:Uuid::new_v4(),segment_revision:Revision::FIRST,start_utf8:0,end_utf8:text.len(),quote:text.into()}],context_spans:vec![],
            input_text:text.into(),normalization_version:"join-space-v1".into(),backend:"test".into(),model_manifest_id:"test".into()}
    }
    #[test]
    fn short_cuts_wait_for_continuation_but_lone_tail_has_a_deadline(){
        let mut window=PhraseWindow::default();let now=Instant::now();let mut first=request("whether to build in-house.");
        assert!(window.defer(&first,now));assert!(window.defer(&first,now+Duration::from_secs(2)));
        first.translation_id=Uuid::new_v4(); // polling proposes new request IDs, never resets the window
        assert!(!window.defer(&first,now+Duration::from_millis(3500)));
        let mut window=PhraseWindow::default();assert!(window.defer(&first,now));
        first.source_spans.extend(request("or buy in.").source_spans);first.input_text.push_str(" or buy in.");
        assert!(!window.defer(&first,now+Duration::from_secs(3)));
    }
    #[test]
    fn complete_sentence_needs_no_extra_wait(){
        assert!(!PhraseWindow::default().defer(&request("We should evaluate the learning outcomes before we replace all of the classroom devices."),Instant::now()));
    }
}
