//! Read-only legacy input, atomically copied into explicitly marked transcript
//! provenance. No audio file, ASR run, producer seal or capture seal is invented.
use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyLine {
    t_start: f64,
    t_end: f64,
    speaker_id: serde_json::Value,
    lang: String,
    text: String,
}
fn import_id(bytes: impl AsRef<[u8]>) -> Uuid {
    let hash = Sha256::digest(bytes.as_ref());
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(&hash[..16]);
    Uuid::from_bytes(uuid)
}
fn invalid() -> StoreError {
    ValidationError::Invalid("legacy_jsonl_requires_valid_timestamps_language_text").into()
}
impl Store {
    /// `content` is already read by an authorized local operator. This method
    /// never accepts or reads a filesystem path. Re-import is scoped to event+SHA.
    pub fn import_legacy_transcript(
        &mut self,
        mut spec: SessionSpec,
        content: String,
    ) -> Result<SessionSummary> {
        if content.len() > 16 * 1024 * 1024 || content.trim().is_empty() {
            return Err(invalid());
        }
        spec.title = format!("[旧数据] {}", spec.title.trim());
        spec.validate()?;
        let file_sha = format!("{:x}", Sha256::digest(content.as_bytes()));
        let mut rows = vec![];
        for (index, line) in content.lines().enumerate() {
            if index >= 10_000 {
                return Err(invalid());
            }
            if line.trim().is_empty() {
                continue;
            }
            let row: LegacyLine = serde_json::from_str(line)?;
            if !row.t_start.is_finite()
                || !row.t_end.is_finite()
                || row.t_start < 0.0
                || row.t_end <= row.t_start
                || row.t_end > 86_400.0 * 365.0
                || row.lang.trim().is_empty()
                || row.lang.len() > 64
                || row.text.trim().is_empty()
                || row.text.len() > 1_000_000
            {
                return Err(invalid());
            }
            let speaker = match &row.speaker_id {
                serde_json::Value::Null => None,
                serde_json::Value::String(s) if s.len() <= 256 => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => return Err(invalid()),
            };
            let start = (row.t_start * 1000.0).round() as u64;
            let end = (row.t_end * 1000.0).round() as u64;
            if end <= start {
                return Err(invalid());
            }
            rows.push((index as u32 + 1, line.to_owned(), row, speaker, start, end));
        }
        if rows.is_empty() {
            return Err(invalid());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT session_id FROM legacy_imports WHERE event_id=?1 AND file_sha256=?2",
                params![spec.event_id.to_string(), file_sha],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let id = parse_uuid(&id)?;
            let session = require_session(&tx, id)?;
            let cursor = tx.query_row(
                "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events WHERE session_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            let segment_count = tx.query_row(
                "SELECT COUNT(*) FROM segments WHERE session_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            return Ok(SessionSummary {
                session,
                status: load_session_status(&tx, id)?,
                cursor,
                segment_count,
                translation_pending: 0,
            });
        }
        let room_event: Option<String> = tx
            .query_row(
                "SELECT event_id FROM rooms WHERE id=?1",
                [spec.room_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if room_event
            .as_deref()
            .is_some_and(|e| e != spec.event_id.to_string())
        {
            return Err(StoreError::ScopeMismatch);
        }
        tx.execute(
            "INSERT OR IGNORE INTO events(id) VALUES(?1)",
            [spec.event_id.to_string()],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO rooms(id,event_id) VALUES(?1,?2)",
            params![spec.room_id.to_string(), spec.event_id.to_string()],
        )?;
        tx.execute("INSERT INTO sessions(id,event_id,room_id,owner_device_id,title,spec_json,state,incomplete) VALUES(?1,?2,?3,?4,?5,?6,'completed',1)",params![spec.session_id.to_string(),spec.event_id.to_string(),spec.room_id.to_string(),spec.owner_device_id.to_string(),spec.title,serde_json::to_string(&spec)?])?;
        let track = TrackSpec {
            track_id: import_id(format!("legacy-track:{}", spec.session_id)),
            session_id: spec.session_id,
            kind: TrackKind::LegacyImport,
            sample_rate: 1000,
        };
        tx.execute(
            "INSERT INTO tracks(id,session_id,sample_rate,spec_json) VALUES(?1,?2,1000,?3)",
            params![
                track.track_id.to_string(),
                spec.session_id.to_string(),
                serde_json::to_string(&track)?
            ],
        )?;
        let producer = import_id(format!("legacy-import:{}:{}", spec.event_id, file_sha));
        let mut seq = 0;
        for (line, original, row, speaker, start, end) in &rows {
            let segment = import_id(format!("legacy-segment:{}:{line}", spec.session_id));
            let audio = AudioRange {
                start_sample: *start,
                end_sample: *end,
                sample_rate: 1000,
                start_ms: *start,
                end_ms: *end,
            };
            audio.validate()?;
            let payload = TranscriptFinal {
                track_id: track.track_id,
                segment_id: segment,
                revision: Revision::FIRST,
                audio: audio.clone(),
                text: row.text.clone(),
                configured_source_language: row.lang.clone(),
                detected_language: Some(row.lang.clone()),
                target_languages: vec![],
                direction_epoch: 1,
                speaker_id: speaker
                    .as_ref()
                    .map(|label| import_id(format!("legacy-speaker:{}:{label}", spec.session_id))),
                status: TranscriptStatus::Success,
                reason: None,
                backend: "legacy-jsonl-import".into(),
                model_manifest_id: format!("not-applicable:legacy-file:{file_sha}"),
            };
            payload.validate()?;
            let message = import_id(format!("legacy-row-message:{}:{line}", spec.session_id));
            let body = serde_json::json!({"schema_version":1,"event_type":"transcript.imported","message_id":message,"session_id":spec.session_id,"file_sha256":file_sha,"line":line,"original":serde_json::from_str::<serde_json::Value>(original)?});
            let body = serde_json::to_string(&body)?;
            tx.execute("INSERT INTO ingested_events(message_id,event_id,session_id,producer_run_id,producer_seq,event_type,body_sha256,body_json) VALUES(?1,?2,?3,?4,?5,'transcript.imported',?6,?7)",params![message.to_string(),spec.event_id.to_string(),spec.session_id.to_string(),producer.to_string(),line,format!("{:x}",Sha256::digest(body.as_bytes())),body])?;
            seq = tx.last_insert_rowid() as u64;
            tx.execute(
                "INSERT INTO segments(id,session_id,track_id) VALUES(?1,?2,?3)",
                params![
                    segment.to_string(),
                    spec.session_id.to_string(),
                    track.track_id.to_string()
                ],
            )?;
            // Same timeline table, explicitly distinguishable via track.kind + origin.
            tx.execute("INSERT INTO capture_segments(segment_id,audio_json,created_seq,result_seq,direction_epoch,configured_source_language,targets_json) VALUES(?1,?2,?3,?3,1,?4,'[]')",params![segment.to_string(),serde_json::to_string(&audio)?,seq,row.lang])?;
            let record = TranscriptRecord {
                session_id: spec.session_id,
                payload,
                origin: RevisionOrigin::LegacyImport {
                    file_sha256: file_sha.clone(),
                    line: *line,
                    speaker_label: speaker.clone(),
                },
                created_seq: seq,
            };
            insert_revision(&tx, &record)?;
            tx.execute(
                "INSERT INTO legacy_import_rows(session_id,line,original_json) VALUES(?1,?2,?3)",
                params![spec.session_id.to_string(), line, original],
            )?;
            tx.execute("INSERT INTO outbox(store_seq,message_id,session_id,event_type,body_json) VALUES(?1,?2,?3,'transcript.imported',?4)",params![seq,message.to_string(),spec.session_id.to_string(),body])?;
        }
        // Milestones refer to the imported text boundary, never to forged seals.
        // Incomplete remains true: JSONL cannot prove audio/meeting completeness.
        tx.execute(
            "UPDATE sessions SET state_seq=?2,capture_stopped_seq=?2,sealed_seq=?2 WHERE id=?1",
            params![spec.session_id.to_string(), seq],
        )?;
        tx.execute("INSERT INTO legacy_imports(event_id,file_sha256,session_id,line_count) VALUES(?1,?2,?3,?4)",params![spec.event_id.to_string(),file_sha,spec.session_id.to_string(),rows.len()])?;
        save_history(&tx, spec.session_id, seq)?;
        let status = load_session_status(&tx, spec.session_id)?;
        tx.commit()?;
        Ok(SessionSummary {
            session: spec,
            status,
            cursor: seq,
            segment_count: rows.len() as u64,
            translation_pending: 0,
        })
    }
}
