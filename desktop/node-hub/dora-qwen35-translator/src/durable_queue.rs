//! Translation scheduling over committed core coverage, never transient ASR text.
use crate::attributed_buffer::{AttributedBuffer, JoinMode};
use crate::recovery_budget::RecoveryBudget;
use anyhow::{ensure, Result};
use forum_contracts::{
    EventType, Revision, SourceSpan, TranslationFailed, TranslationFinal, TranslationRequested,
    TranslationStatus, Uuid,
};
use forum_runtime::{DurableProducer, DurableReceipt, RuntimeClient, RuntimeConfig};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::Instant,
};

#[cfg(target_os = "macos")]
pub const MODEL_BACKEND: &str = "hy-mt2-mlx";
#[cfg(not(target_os = "macos"))]
pub const MODEL_BACKEND: &str = "qwen3.5-mlx";
pub const PASSTHROUGH_BACKEND: &str = "identity-passthrough";

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TranslationWork {
    pub track_id: Uuid,
    pub audio: forum_contracts::AudioRange,
    pub source_span: SourceSpan,
    pub configured_source_language: String,
    pub detected_language: Option<String>,
    pub target_language: String,
    pub direction_epoch: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RequestRecord {
    pub session_id: Uuid,
    pub request: TranslationRequested,
    pub state: String,
    pub created_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
struct PageKey {
    created_seq: u64,
    translation_id: Uuid,
}
#[derive(Deserialize)]
struct RequestPage {
    items: Vec<RequestRecord>,
    next_after: Option<PageKey>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttemptKey {
    pub translation_id: Uuid,
    pub revision: u32,
    pub attempt: u32,
    pub direction_epoch: u64,
}
impl From<&TranslationRequested> for AttemptKey {
    fn from(r: &TranslationRequested) -> Self {
        Self {
            translation_id: r.translation_id,
            revision: r.revision.get(),
            attempt: r.attempt,
            direction_epoch: r.direction_epoch,
        }
    }
}

fn canonical_language(value: &str) -> Option<&'static str> {
    match value.trim().to_lowercase().as_str() {
        "zh" | "zh-cn" | "chinese" => Some("zh"),
        "en" | "en-us" | "english" => Some("en"),
        "ja" | "japanese" => Some("ja"),
        "ko" | "korean" => Some("ko"),
        "fr" | "french" => Some("fr"),
        "de" | "german" => Some("de"),
        "es" | "spanish" => Some("es"),
        "ru" | "russian" => Some("ru"),
        _ => None,
    }
}
fn has_han(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c as u32,0x3400..=0x4dbf|0x4e00..=0x9fff|0x20000..=0x323af))
}
pub fn is_passthrough(work: &TranslationWork) -> bool {
    // Detection remains separate from configuration. A mixed/unknown detected
    // value must never silently fall back to the configured forced Qwen hint.
    let source = match &work.detected_language {
        Some(value) => canonical_language(value),
        None => canonical_language(&work.configured_source_language),
    };
    let target = canonical_language(&work.target_language);
    if source.is_none() || source != target {
        return false;
    }
    // Script checks only prevent unsafe passthrough; they are not audio language
    // detection. A dominant zh/en label cannot hide the other script in a span.
    match target {
        Some("zh") => !work
            .source_span
            .quote
            .chars()
            .any(|c| c.is_ascii_alphabetic()),
        Some("en") => !has_han(&work.source_span.quote),
        _ => false,
    }
}

/// Sources stay in the core's chronological order. Different target/epoch/route
/// groups never share a request. Returned IDs are proposals until core commits.
pub fn plan_requests(
    work: Vec<TranslationWork>,
    model_manifest: &str,
    max_graphemes: usize,
) -> Result<Vec<TranslationRequested>> {
    struct Group {
        target: String, epoch: u64, passthrough: bool, track: Uuid,
        last_start: u64, last_end: u64, buffer: AttributedBuffer,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut seen = HashMap::new();
    for item in work {
        ensure!(
            canonical_language(&item.target_language).is_some(),
            "unsupported translation target"
        );
        ensure!(item.direction_epoch > 0, "invalid direction epoch");
        let key = (
            item.source_span.segment_id,
            item.source_span.segment_revision.get(),
            item.source_span.start_utf8,
            item.source_span.end_utf8,
            item.target_language.clone(),
            item.direction_epoch,
        );
        if let Some(quote) = seen.insert(key, item.source_span.quote.clone()) {
            ensure!(
                quote == item.source_span.quote,
                "conflicting duplicate coverage"
            );
            continue;
        }
        let passthrough = is_passthrough(&item);
        let group = if let Some(i) = groups.iter().rposition(|g| {
            g.target == item.target_language && g.epoch == item.direction_epoch && g.passthrough == passthrough
                && g.track == item.track_id && item.audio.start_ms >= g.last_start
                && item.audio.start_ms <= g.last_end.saturating_add(1500)
        }) {
            i
        } else {
            groups.push(Group {
                target: item.target_language.clone(), epoch: item.direction_epoch, passthrough,
                track: item.track_id, last_start: item.audio.start_ms, last_end: item.audio.end_ms,
                buffer: AttributedBuffer::new(JoinMode::Space),
            });
            groups.len() - 1
        };
        groups[group].last_start = item.audio.start_ms;
        groups[group].last_end = item.audio.end_ms;
        groups[group].buffer.append(item.source_span)?;
    }
    let mut requests = Vec::new();
    for Group { target, epoch, passthrough, mut buffer, .. } in groups {
        while let Some(chunk) = buffer.take_chunk(max_graphemes)? {
            let request = TranslationRequested {
                translation_id: Uuid::new_v4(),
                revision: Revision::FIRST,
                attempt: 1,
                target_language: target.clone(),
                direction_epoch: epoch,
                context_spans: vec![],
                source_spans: chunk.source_spans,
                input_text: chunk.input_text,
                normalization_version: chunk.normalization_version.into(),
                backend: if passthrough {
                    PASSTHROUGH_BACKEND
                } else {
                    MODEL_BACKEND
                }
                .into(),
                model_manifest_id: if passthrough {
                    "identity-text-v1"
                } else {
                    model_manifest
                }
                .into(),
            };
            request.validate()?;
            requests.push(request);
        }
    }
    Ok(requests)
}

struct Active {
    request: TranslationRequested,
    started: Instant,
}
struct Waiting {
    message_id: Uuid,
    request: TranslationRequested,
}

pub struct AcknowledgedResult {
    pub request: TranslationRequested,
    pub result: TranslationFinal,
    pub receipt: DurableReceipt,
}

pub struct DurableQueue {
    producer: DurableProducer,
    client: RuntimeClient,
    session_id: Uuid,
    model_manifest: String,
    recovery_loaded: bool,
    recovery_after: Option<PageKey>,
    recovery_budget: RecoveryBudget,
    recovery: VecDeque<TranslationRequested>,
    terminal_rejections: HashSet<Uuid>,
    waiting: Option<Waiting>,
    active: Option<Active>,
    stopping: bool,
    phrase_window: crate::phrase_window::PhraseWindow,
}

impl DurableQueue {
    pub fn open(config: RuntimeConfig, model_manifest: String) -> Result<Self> {
        let client = RuntimeClient::new(config.endpoint.clone());
        let session_id = config.session.session_id;
        let recovery_budget = RecoveryBudget::from_env(&config)?;
        Ok(Self {
            producer: DurableProducer::open(config)?,
            client,
            session_id,
            model_manifest,
            recovery_loaded: false,
            recovery_after: None,
            recovery_budget,
            recovery: VecDeque::new(),
            terminal_rejections: HashSet::new(),
            waiting: None,
            active: None,
            stopping: false,
            phrase_window: Default::default(),
        })
    }
    pub fn mark_ready(&mut self) -> Result<()> {
        // A model load receipt alone must not imply that an old producer's
        // unacknowledged results have been reconciled. The host accepts replay
        // before ready; do it without inventing acknowledgements or new IDs.
        let (_, blocked) = self.flush()?;
        if blocked {
            return Err(forum_runtime::RpcError::new(
                "OUTBOX_PENDING",
                true,
                "Translator is loaded but replayable events still await core receipts",
            )
            .into());
        }
        self.client.call(
            "ready",
            json!({
                "session_id":self.session_id,
                "producer_run_id":self.producer.run_id(),
                "prior_run_ids":self.producer.prior_run_ids()?,
                "outbox_replayed":self.pending_count()? == 0,
                "role":"translator",
                "backend":MODEL_BACKEND,
                "model_manifest_id":self.model_manifest
            }),
        )?;
        Ok(())
    }
    pub fn active_key(&self) -> Option<AttemptKey> {
        self.active.as_ref().map(|a| AttemptKey::from(&a.request))
    }
    pub fn model_matches(&self, request: &TranslationRequested) -> bool {
        request.backend == MODEL_BACKEND && request.model_manifest_id == self.model_manifest
    }
    pub fn pending_count(&self) -> Result<usize> {
        Ok(self.producer.pending()?.len())
    }

    fn request_records(&self) -> Result<RequestPage> {
        let rows = self.client.call(
            "translation_requests",
            json!({"session_id":self.session_id,"limit":100,"states":["requested","failed"],"after":self.recovery_after}),
        )?;
        let page: RequestPage = serde_json::from_value(rows)?;
        ensure!(
            page.items.iter().all(|r| r.session_id == self.session_id),
            "cross-session translation response"
        );
        let mut previous = self.recovery_after.clone();
        for item in &page.items {
            let key = PageKey {
                created_seq: item.created_seq,
                translation_id: item.request.translation_id,
            };
            ensure!(
                key.created_seq > 0 && previous.as_ref().is_none_or(|old| old < &key),
                "unordered translation recovery page"
            );
            previous = Some(key);
        }
        if let Some(next) = &page.next_after {
            ensure!(
                !page.items.is_empty() && previous.as_ref() == Some(next),
                "invalid translation recovery cursor"
            );
        }
        Ok(page)
    }

    /// True means retryable outbox backpressure; it blocks new model work, but
    /// leaves every exact event on disk. Late fenced results are retained and
    /// logged separately rather than being silently deleted or blocking peers.
    fn flush(&mut self) -> Result<(HashSet<Uuid>, bool)> {
        let mut committed = HashSet::new();
        let mut blocked = false;
        let pending = self
            .producer
            .pending()?
            .into_iter()
            .filter(|p| !self.terminal_rejections.contains(&p.message_id))
            .take(8)
            .collect::<Vec<_>>();
        for pending in pending {
            match self.producer.flush_one(pending.message_id) {
                Ok(_) => {
                    committed.insert(pending.message_id);
                    if let Some(warning) = self.producer.cleanup_warning() {
                        tracing::warn!(%warning,"Core acknowledged the event; local outbox cleanup needs attention");
                    }
                }
                Err(error) if error.retryable => {
                    blocked = true;
                    tracing::warn!(message_id=%pending.message_id,code=%error.code,"Translation outbox remains unacknowledged");
                    break;
                }
                Err(error) if error.code == "LATE_RESULT" => {
                    self.terminal_rejections.insert(pending.message_id);
                    tracing::error!(message_id=%pending.message_id,"Fenced translation event retained in outbox for inspection");
                }
                Err(error) => return Err(error.into()),
            }
        }
        // Drain all replayable events before recovery can increment an attempt;
        // otherwise a still-pending old final could be fenced by our own retry.
        blocked |= self
            .producer
            .pending()?
            .iter()
            .any(|p| !self.terminal_rejections.contains(&p.message_id));
        Ok((committed, blocked))
    }

    fn activate(&mut self, request: TranslationRequested) -> TranslationRequested {
        self.active = Some(Active {
            request: request.clone(),
            started: Instant::now(),
        });
        request
    }

    pub fn poll(&mut self) -> Result<Option<TranslationRequested>> {
        let (committed, blocked) = self.flush()?;
        if let Some(waiting) = self.waiting.take() {
            if committed.contains(&waiting.message_id) {
                if self.stopping {
                    return Ok(None);
                }
                return Ok(Some(self.activate(waiting.request)));
            }
            if self.terminal_rejections.contains(&waiting.message_id) { /* stale proposal; do not generate */
            } else {
                self.waiting = Some(waiting);
                return Ok(None);
            }
        }
        if blocked || self.stopping || self.active.is_some() {
            return Ok(None);
        }
        if !self.recovery_loaded && self.recovery.is_empty() {
            let page = self.request_records()?;
            self.recovery_after = page.next_after;
            self.recovery_loaded = self.recovery_after.is_none();
            for record in page.items {
                if matches!(record.state.as_str(), "requested" | "failed")
                    && record.request.attempt < self.recovery_budget.ceiling(&record.request)?
                {
                    let mut request = record.request;
                    request.attempt += 1;
                    request.validate()?;
                    self.recovery.push_back(request);
                }
            }
            // A page containing only exhausted requests cannot hide the next
            // page, or block Stop by draining the entire history synchronously.
            if self.recovery.is_empty() && !self.recovery_loaded {
                return Ok(None);
            }
        }
        let request = if let Some(request) = self.recovery.pop_front() {
            Some(request)
        } else {
            let value = self.client.call(
                "poll_translation",
                json!({"session_id":self.session_id,"limit":64}),
            )?;
            let work: Vec<TranslationWork> = serde_json::from_value(value)?;
            // Only the selected request is reserved. Other proposed IDs are
            // discarded; unselected coverage remains durably pending in core.
            let mut proposal = plan_requests(work, &self.model_manifest, 320)?.into_iter().next();
            if let Some(request) = proposal.as_mut().filter(|r| r.backend != PASSTHROUGH_BACKEND) {
                if self.phrase_window.defer(request, Instant::now()) { return Ok(None); }
                request.context_spans = serde_json::from_value(self.client.call("translation_context",
                    json!({"session_id":self.session_id,"first":request.source_spans[0],"direction_epoch":request.direction_epoch}))?)?;
                request.validate()?;
            }
            proposal
        };
        let Some(request) = request else {
            return Ok(None);
        };
        let pending = self
            .producer
            .append(EventType::TranslationRequested, &request)?;
        self.waiting = Some(Waiting {
            message_id: pending.message_id,
            request,
        });
        let (committed, _) = self.flush()?;
        if committed.contains(&pending.message_id) {
            let waiting = self.waiting.take().unwrap();
            return Ok(Some(self.activate(waiting.request)));
        }
        Ok(None)
    }

    pub fn complete(
        &mut self,
        key: AttemptKey,
        output: std::result::Result<String, String>,
    ) -> Result<Option<AcknowledgedResult>> {
        if self.active_key() != Some(key) {
            return Ok(None);
        } // duplicate, cancelled or old attempt
        let active = self.active.take().unwrap();
        let request = active.request;
        let output = output.and_then(|text| {
            if text.trim().is_empty() {
                Err("EMPTY_TRANSLATION: model returned no text".into())
            } else {
                Ok(text)
            }
        });
        match output {
            Ok(text) => {
                let result = TranslationFinal {
                    translation_id: request.translation_id,
                    revision: request.revision,
                    attempt: request.attempt,
                    direction_epoch: request.direction_epoch,
                    text,
                    status: if request.backend == PASSTHROUGH_BACKEND {
                        TranslationStatus::Passthrough
                    } else {
                        TranslationStatus::Final
                    },
                    backend: request.backend.clone(),
                    model_manifest_id: request.model_manifest_id.clone(),
                    elapsed_ms: active.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                };
                let pending = self.producer.append(EventType::TranslationFinal, &result)?;
                match self.producer.flush_one(pending.message_id) {
                    Ok(receipt) => Ok(Some(AcknowledgedResult {
                        request,
                        result,
                        receipt,
                    })),
                    Err(error) if error.retryable || error.code == "LATE_RESULT" => {
                        tracing::warn!(message_id=%pending.message_id,code=%error.code,"Translation final retained until core acknowledges");
                        Ok(None)
                    }
                    Err(error) => Err(error.into()),
                }
            }
            Err(reason) => {
                self.persist_failure(&request, "GENERATION_FAILED", &reason)?;
                Ok(None)
            }
        }
    }

    fn persist_failure(
        &mut self,
        request: &TranslationRequested,
        code: &str,
        reason: &str,
    ) -> Result<()> {
        let payload = TranslationFailed {
            translation_id: request.translation_id,
            revision: request.revision,
            attempt: request.attempt,
            direction_epoch: request.direction_epoch,
            code: code.into(),
            reason: reason.into(),
        };
        let pending = self
            .producer
            .append(EventType::TranslationFailed, &payload)?;
        match self.producer.flush_one(pending.message_id) {
            Ok(_) => Ok(()),
            Err(error) if error.retryable || error.code == "LATE_RESULT" => {
                tracing::warn!(message_id=%pending.message_id,code=%error.code,"Translation failure retained until core acknowledges");
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Remaining coverage already belongs to core, including never-requested
    /// short tails. An in-flight attempt receives a durable failure before exit.
    pub fn stop(&mut self) -> Result<()> {
        self.stopping = true;
        if let Some(active) = self.active.take() {
            self.persist_failure(
                &active.request,
                "CANCELLED",
                "Translator stopped during generation; source coverage remains recoverable",
            )?;
        }
        self.flush()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn work(
        text: &str,
        source: &str,
        detected: Option<&str>,
        target: &str,
        epoch: u64,
    ) -> TranslationWork {
        TranslationWork {
            track_id: Uuid::from_u128(1),
            audio: forum_contracts::AudioRange {start_sample:0,end_sample:48000,sample_rate:16000,start_ms:0,end_ms:3000},
            source_span: SourceSpan {
                segment_id: Uuid::new_v4(),
                segment_revision: Revision::FIRST,
                start_utf8: 0,
                end_utf8: text.len(),
                quote: text.into(),
            },
            configured_source_language: source.into(),
            detected_language: detected.map(String::from),
            target_language: target.into(),
            direction_epoch: epoch,
        }
    }
    #[test]
    fn configured_hint_is_not_auto_and_mixed_never_passthrough() {
        assert!(is_passthrough(&work("中文", "Chinese", None, "zh", 1)));
        assert!(is_passthrough(&work("Hello", "auto", Some("en"), "en", 1)));
        assert!(!is_passthrough(&work("Hello", "auto", None, "en", 1)));
        assert!(!is_passthrough(&work("API 还没发布", "zh", None, "zh", 1)));
        assert!(!is_passthrough(&work(
            "Hello 中文",
            "auto",
            Some("en"),
            "en",
            1
        )));
        assert!(!is_passthrough(&work(
            "Hello",
            "en",
            Some("mixed"),
            "en",
            1
        )));
    }
    #[test]
    fn phrase_groups_do_not_cross_tracks_or_long_silences() {
        let first = work("Should we build in house", "en", None, "zh", 1);
        let mut next = work("or buy in?", "en", None, "zh", 1);
        next.audio.start_ms = 3000; next.audio.end_ms = 6000;
        assert_eq!(plan_requests(vec![first.clone(), next.clone()], "test", 320).unwrap().len(), 1);
        next.track_id = Uuid::new_v4();
        assert_eq!(plan_requests(vec![first.clone(), next.clone()], "test", 320).unwrap().len(), 2);
        next.track_id = first.track_id;
        next.audio.start_ms = 30000; next.audio.end_ms = 33000;
        assert_eq!(plan_requests(vec![first, next], "test", 320).unwrap().len(), 2);
    }
    #[test]
    fn repeated_work_and_direction_changes_do_not_duplicate_or_cross_epoch() {
        let a = work("不要", "zh", None, "en", 1);
        let b = work("还没完成", "zh", None, "en", 2);
        let requests = plan_requests(vec![a.clone(), a, b], "local:test", 160).unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].source_spans.len(), 1);
        assert_eq!(requests[0].input_text, "不要");
        assert_eq!(requests[1].direction_epoch, 2);
    }
    #[test]
    fn multi_segment_and_split_plans_validate_exact_utf8_input() {
        let requests = plan_requests(
            vec![
                work("中e\u{301}👩‍💻尾", "zh", Some("mixed"), "en", 1),
                work("短句", "zh", None, "en", 1),
            ],
            "local:test",
            2,
        )
        .unwrap();
        assert!(requests.len() >= 3);
        for request in &requests {
            request.validate().unwrap();
            assert!(!request.input_text.ends_with('\u{200d}'));
        }
        let combined = requests
            .iter()
            .flat_map(|r| r.source_spans.iter())
            .map(|s| s.quote.as_str())
            .collect::<String>();
        assert_eq!(combined, "中e\u{301}👩‍💻尾短句");
    }
    #[test]
    fn mixed_can_have_two_target_requests_and_same_language_is_explicit() {
        let a = work("Please 不要发布", "auto", Some("mixed"), "zh", 1);
        let mut b = a.clone();
        b.target_language = "en".into();
        let requests = plan_requests(
            vec![a, b, work("No", "en", None, "en", 1)],
            "local:test",
            160,
        )
        .unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[..2].iter().all(|r| r.backend == MODEL_BACKEND));
        assert_eq!(requests[2].backend, PASSTHROUGH_BACKEND);
    }

    // These tests use the real Unix framing/authentication and disk outbox.
    // The host callback is intentionally small; SQLite transactions/fences are
    // independently tested by forum-core rather than reproduced here.
    #[derive(Default)]
    struct HostState {
        work: Vec<TranslationWork>,
        requests: HashMap<Uuid, (TranslationRequested, String)>,
        receipts: HashMap<Uuid, serde_json::Value>,
        received: Vec<serde_json::Value>,
        deny_requested: bool,
        lose_final_ack: bool,
        fence_final: bool,
        ready_calls: Vec<serde_json::Value>,
        created_seq: HashMap<Uuid, u64>,
    }
    struct Host {
        root: std::path::PathBuf,
        config: RuntimeConfig,
        state: std::sync::Arc<std::sync::Mutex<HostState>>,
        server: forum_runtime::UdsServer,
    }
    impl Host {
        fn new(work: Vec<TranslationWork>) -> Self {
            use std::os::unix::fs::DirBuilderExt;
            let root = std::path::PathBuf::from(format!("/tmp/f04-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&root)
                .unwrap();
            let session = forum_contracts::SessionSpec {
                session_id: Uuid::new_v4(),
                event_id: Uuid::new_v4(),
                room_id: Uuid::new_v4(),
                owner_device_id: Uuid::new_v4(),
                title: "Synthetic queue tests".into(),
            };
            let endpoint = forum_runtime::Endpoint::new(root.join("core.sock"));
            let config = RuntimeConfig {
                endpoint: endpoint.clone(),
                session: session.clone(),
                producer_dir: root.join("translator"),
                producer_name: "translator".into(),
            };
            let state = std::sync::Arc::new(std::sync::Mutex::new(HostState {
                work,
                ..Default::default()
            }));
            let shared = state.clone();
            let server=forum_runtime::UdsServer::bind(endpoint,move|call| {
                let mut state=shared.lock().unwrap();
                match call.method.as_str() {
                    "ready"=>{state.ready_calls.push(call.params.clone());Ok(json!({"ready":true}))},
                    "poll_translation"=>Ok(serde_json::to_value(&state.work).unwrap()),
                    "translation_context"=>Ok(json!([])),
                    "translation_requests"=>{
                        let after:Option<PageKey>=serde_json::from_value(call.params["after"].clone()).unwrap();
                        let limit=call.params["limit"].as_u64().unwrap_or(100) as usize;
                        let mut rows=state.requests.values().filter(|(_,s)|matches!(s.as_str(),"requested"|"failed")).map(|(r,s)|{
                            let key=PageKey{created_seq:state.created_seq[&r.translation_id],translation_id:r.translation_id};
                            (key.clone(),json!({"session_id":session.session_id,"request":r,"state":s,"created_seq":key.created_seq}))
                        }).filter(|(key,_)|after.as_ref().is_none_or(|after|key>after)).collect::<Vec<_>>();
                        rows.sort_by(|a,b|a.0.cmp(&b.0));rows.truncate(limit);
                        let next=if rows.len()==limit{rows.last().map(|row|row.0.clone())}else{None};
                        Ok(json!({"cursor":state.receipts.len(),"items":rows.into_iter().map(|row|row.1).collect::<Vec<_>>(),"next_after":next}))
                    },
                    "ingest"=>{
                        let event=call.params["event"].clone();state.received.push(event.clone());
                        let id:Uuid=serde_json::from_value(event["message_id"].clone()).unwrap();
                        if let Some(receipt)=state.receipts.get(&id) {let mut receipt=receipt.clone();receipt["duplicate"]=json!(true);return Ok(receipt);}
                        match event["type"].as_str().unwrap() {
                            "translation.requested"=>{
                                if state.deny_requested {return Err(forum_runtime::RpcError::new("DATABASE_BUSY",true,"not committed"));}
                                let r:TranslationRequested=serde_json::from_value(event["payload"].clone()).unwrap();r.validate().unwrap();
                                state.work.retain(|w|!r.source_spans.iter().any(|s|s.segment_id==w.source_span.segment_id));
                                let created_seq=state.receipts.len() as u64+1;
                                state.created_seq.entry(r.translation_id).or_insert(created_seq);
                                state.requests.insert(r.translation_id,(r,"requested".into()));
                            }
                            "translation.final"=>{
                                let payload:TranslationFinal=serde_json::from_value(event["payload"].clone()).unwrap();
                                if state.fence_final {return Err(forum_runtime::RpcError::new("LATE_RESULT",false,"attempt superseded"));}
                                let row=state.requests.get_mut(&payload.translation_id).unwrap();
                                assert_eq!(row.0.attempt,payload.attempt);assert_eq!(row.0.direction_epoch,payload.direction_epoch);
                                row.1=if payload.status==TranslationStatus::Passthrough{"passthrough"}else{"final"}.into();
                            }
                            "translation.failed"=>{
                                let payload:TranslationFailed=serde_json::from_value(event["payload"].clone()).unwrap();
                                state.requests.get_mut(&payload.translation_id).unwrap().1="failed".into();
                            }
                            _=>panic!("unexpected event"),
                        }
                        let receipt=json!({"message_id":id,"store_seq":state.receipts.len()+1,"duplicate":false});state.receipts.insert(id,receipt.clone());
                        if event["type"]=="translation.final" && state.lose_final_ack {state.lose_final_ack=false;return Err(forum_runtime::RpcError::new("TRANSPORT_UNAVAILABLE",true,"commit succeeded, ack lost"));}
                        Ok(receipt)
                    }
                    _=>Err(forum_runtime::RpcError::new("METHOD_NOT_FOUND",false,&call.method)),
                }
            }).unwrap();
            Self {
                root,
                config,
                state,
                server,
            }
        }
        fn queue(&self) -> DurableQueue {
            {
                let mut queue = DurableQueue::open(self.config.clone(), "test-local-model".into()).unwrap();
                queue.phrase_window = crate::phrase_window::PhraseWindow::immediate();
                queue
            }
        }
    }
    impl Drop for Host {
        fn drop(&mut self) {
            self.server.stop().unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn explicit_recovery_has_three_durable_extra_attempts_not_unlimited_restarts() {
        let host = Host::new(vec![]);
        let mut requested = plan_requests(
            vec![work("不要", "zh", None, "en", 1)],
            "test-local-model",
            160,
        )
        .unwrap()
        .remove(0);
        requested.attempt = 3;
        {
            let mut state = host.state.lock().unwrap();
            state.created_seq.insert(requested.translation_id, 1);
            state
                .requests
                .insert(requested.translation_id, (requested, "failed".into()));
        }
        let action_id = Uuid::new_v4();
        for expected in 4..=6 {
            let mut queue = host.queue();
            queue.recovery_budget =
                RecoveryBudget::for_action(&host.config, Some(action_id)).unwrap();
            let request = queue.poll().unwrap().unwrap();
            assert_eq!(request.attempt, expected);
            queue.stop().unwrap();
        }
        let mut exhausted = host.queue();
        exhausted.recovery_budget =
            RecoveryBudget::for_action(&host.config, Some(action_id)).unwrap();
        assert!(exhausted.poll().unwrap().is_none());
        drop(exhausted);
        let mut ordinary = host.queue();
        assert!(ordinary.poll().unwrap().is_none());
        drop(ordinary);
        let mut new_action = host.queue();
        new_action.recovery_budget =
            RecoveryBudget::for_action(&host.config, Some(Uuid::new_v4())).unwrap();
        assert_eq!(new_action.poll().unwrap().unwrap().attempt, 7);
    }
    #[test]
    fn exhausted_first_page_does_not_hide_later_recoverable_requests() {
        let host = Host::new(vec![]);
        {
            let mut state = host.state.lock().unwrap();
            for index in 0..105 {
                let mut request = plan_requests(
                    vec![work("不要", "zh", None, "en", 1)],
                    "test-local-model",
                    160,
                )
                .unwrap()
                .remove(0);
                request.attempt = if index < 100 { 3 } else { 1 };
                state.created_seq.insert(request.translation_id, index + 1);
                state
                    .requests
                    .insert(request.translation_id, (request, "failed".into()));
            }
        }
        let mut queue = host.queue();
        assert!(queue.poll().unwrap().is_none());
        for _ in 0..5 {
            let request = queue
                .poll()
                .unwrap()
                .expect("later page remains recoverable");
            assert_eq!(request.attempt, 2);
            queue
                .complete((&request).into(), Ok("Do not".into()))
                .unwrap();
        }
        assert!(queue.poll().unwrap().is_none());
        let state = host.state.lock().unwrap();
        assert_eq!(
            state
                .requests
                .values()
                .filter(|(_, s)| s == "final")
                .count(),
            5
        );
        assert_eq!(
            state
                .requests
                .values()
                .filter(|(r, _)| r.attempt == 3)
                .count(),
            100
        );
    }
    #[test]
    fn ready_waits_for_replay_and_reports_real_prior_runs() {
        let host = Host::new(vec![work("不要", "zh", None, "en", 1)]);
        host.state.lock().unwrap().deny_requested = true;
        let mut queue = host.queue();
        let old_run = queue.producer.run_id();
        assert!(queue.poll().unwrap().is_none());
        drop(queue);
        let mut restarted = host.queue();
        let error = restarted.mark_ready().unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<forum_runtime::RpcError>()
                .unwrap()
                .code,
            "OUTBOX_PENDING"
        );
        assert!(host.state.lock().unwrap().ready_calls.is_empty());
        host.state.lock().unwrap().deny_requested = false;
        restarted.mark_ready().unwrap();
        let state = host.state.lock().unwrap();
        assert_eq!(state.requests.len(), 1);
        let ready = &state.ready_calls[0];
        assert_eq!(ready["prior_run_ids"], json!([old_run]));
        assert_eq!(ready["producer_run_id"], json!(restarted.producer.run_id()));
        assert_eq!(ready["outbox_replayed"], true);
    }
    #[test]
    fn unacknowledged_request_never_reaches_generator_and_keeps_exact_event() {
        let host = Host::new(vec![work("不要", "zh", None, "en", 1)]);
        host.state.lock().unwrap().deny_requested = true;
        let mut queue = host.queue();
        assert!(queue.poll().unwrap().is_none());
        assert!(queue.active_key().is_none());
        let before = queue.producer.pending().unwrap();
        assert_eq!(before.len(), 1);
        let id = before[0].message_id;
        let event = before[0].event.clone();
        host.state.lock().unwrap().deny_requested = false;
        let requested = queue.poll().unwrap().unwrap();
        assert_eq!(requested.input_text, "不要");
        assert_eq!(queue.pending_count().unwrap(), 0);
        let state = host.state.lock().unwrap();
        let copies = state
            .received
            .iter()
            .filter(|e| e["message_id"] == json!(id))
            .collect::<Vec<_>>();
        assert!(copies.len() >= 2);
        assert!(copies.iter().all(|e| **e == event));
    }
    #[test]
    fn final_ack_loss_replays_same_event_after_restart_without_new_translation() {
        let host = Host::new(vec![work("不要发布", "zh", None, "en", 1)]);
        let mut queue = host.queue();
        let requested = queue.poll().unwrap().unwrap();
        host.state.lock().unwrap().lose_final_ack = true;
        assert!(queue
            .complete((&requested).into(), Ok("Do not publish".into()))
            .unwrap()
            .is_none());
        let before = queue.producer.pending().unwrap();
        assert_eq!(before.len(), 1);
        let old = before[0].event.clone();
        drop(queue);
        let mut restarted = host.queue();
        assert!(restarted.poll().unwrap().is_none());
        assert_eq!(restarted.pending_count().unwrap(), 0);
        let state = host.state.lock().unwrap();
        let finals = state
            .received
            .iter()
            .filter(|e| e["type"] == "translation.final")
            .collect::<Vec<_>>();
        assert_eq!(finals.len(), 2);
        assert!(finals.iter().all(|e| **e == old));
        assert_eq!(state.requests.len(), 1);
    }
    #[test]
    fn stop_then_restart_recovers_unpunctuated_short_tail_with_new_attempt() {
        let host = Host::new(vec![work("不要", "zh", None, "en", 1)]);
        let mut queue = host.queue();
        let requested = queue.poll().unwrap().unwrap();
        let old_key = (&requested).into();
        queue.stop().unwrap();
        assert!(queue
            .complete(old_key, Ok("late output".into()))
            .unwrap()
            .is_none());
        drop(queue);
        let mut restarted = host.queue();
        let recovered = restarted.poll().unwrap().unwrap();
        assert_eq!(recovered.translation_id, requested.translation_id);
        assert_eq!(recovered.revision, requested.revision);
        assert_eq!(recovered.attempt, 2);
        assert_eq!(recovered.input_text, "不要");
        assert!(restarted
            .complete(old_key, Ok("old attempt".into()))
            .unwrap()
            .is_none());
        assert_eq!(restarted.active_key(), Some((&recovered).into()));
        let accepted = restarted
            .complete((&recovered).into(), Ok("Do not".into()))
            .unwrap()
            .unwrap();
        assert_eq!(accepted.result.attempt, 2);
        assert!(restarted
            .complete((&recovered).into(), Ok("duplicate".into()))
            .unwrap()
            .is_none());
    }
    #[test]
    fn core_fence_rejects_late_final_without_deleting_its_outbox() {
        let host = Host::new(vec![work("还没批准", "zh", None, "en", 3)]);
        let mut queue = host.queue();
        let requested = queue.poll().unwrap().unwrap();
        host.state.lock().unwrap().fence_final = true;
        assert!(queue
            .complete((&requested).into(), Ok("Not approved".into()))
            .unwrap()
            .is_none());
        assert_eq!(queue.pending_count().unwrap(), 1);
        let pending = queue.producer.pending().unwrap()[0].event.clone();
        assert!(queue.poll().unwrap().is_none());
        assert_eq!(queue.producer.pending().unwrap()[0].event, pending);
        queue.mark_ready().unwrap();
        assert_eq!(
            host.state.lock().unwrap().ready_calls[0]["outbox_replayed"],
            false
        );
    }
    #[test]
    fn empty_targets_mean_no_model_request_and_passthrough_is_persisted() {
        let empty = Host::new(vec![]);
        let mut idle = empty.queue();
        assert!(idle.poll().unwrap().is_none());
        drop(idle);
        let host = Host::new(vec![work("Hello", "en", None, "en", 1)]);
        let mut queue = host.queue();
        let requested = queue.poll().unwrap().unwrap();
        assert_eq!(requested.backend, PASSTHROUGH_BACKEND);
        let accepted = queue
            .complete((&requested).into(), Ok(requested.input_text.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(accepted.result.status, TranslationStatus::Passthrough);
        assert_eq!(accepted.result.text, "Hello");
    }
}
