use super::*;
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionStatus {
    pub session_id: Uuid,
    pub state: SessionState,
    pub direction_epoch: u64,
    pub target_languages: Vec<String>,
    pub capture_stopped: bool,
    pub transcript_sealed: bool,
    pub incomplete: bool,
    pub state_seq: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionSummary {
    pub session: SessionSpec,
    pub status: SessionStatus,
    pub cursor: u64,
    pub segment_count: u64,
    pub translation_pending: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecoverySegment {
    pub segment_id: Uuid,
    pub track_id: Uuid,
    pub audio: AudioRange,
    pub recording_ref: Option<String>,
    pub next_revision: Revision,
    pub previous_status: Option<TranscriptStatus>,
    pub reason: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RecoveryRevision {
    pub segment_id: Uuid,
    pub next_revision: Option<Revision>,
    pub terminal: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PageKey {
    pub start_ms: u64,
    pub track_id: Uuid,
    pub segment_id: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SnapshotItem {
    pub segment_id: Uuid,
    pub track_id: Uuid,
    pub audio: AudioRange,
    pub recording_ref: Option<String>,
    pub transcript: Option<TranscriptRecord>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SnapshotPage {
    pub session: SessionSpec,
    pub status: SessionStatus,
    pub cursor: u64,
    pub items: Vec<SnapshotItem>,
    pub next_after: Option<PageKey>,
}

pub(crate) fn load_session_status(
    connection: &Connection,
    session_id: Uuid,
) -> Result<SessionStatus> {
    let row:Option<(String,u64,String,bool,bool,bool,u64)>=connection.query_row(
        "SELECT state,direction_epoch,targets_json,capture_stopped_seq IS NOT NULL,sealed_seq IS NOT NULL,incomplete,state_seq FROM sessions WHERE id=?1",
        [session_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let (
        state,
        direction_epoch,
        targets,
        capture_stopped,
        transcript_sealed,
        incomplete,
        state_seq,
    ) = row.ok_or(StoreError::NotFound("session"))?;
    Ok(SessionStatus {
        session_id,
        state: serde_json::from_value(serde_json::Value::String(state))?,
        direction_epoch,
        target_languages: serde_json::from_str(&targets)?,
        capture_stopped,
        transcript_sealed,
        incomplete,
        state_seq,
    })
}
pub(crate) fn require_capture_state(connection: &Connection, session_id: Uuid) -> Result<()> {
    let status = load_session_status(connection, session_id)?;
    if status.capture_stopped
        || !matches!(
            status.state,
            SessionState::Recording | SessionState::Stopping | SessionState::Interrupted
        )
    {
        return Err(StoreError::InvalidState);
    }
    Ok(())
}
pub(crate) fn save_history(tx: &Transaction<'_>, session_id: Uuid, seq: u64) -> Result<()> {
    tx.execute(
        "UPDATE sessions SET state_seq=?1 WHERE id=?2",
        params![seq, session_id.to_string()],
    )?;
    let status = load_session_status(tx, session_id)?;
    tx.execute(
        "INSERT INTO session_history(session_id,store_seq,status_json) VALUES(?1,?2,?3)",
        params![session_id.to_string(), seq, serde_json::to_string(&status)?],
    )?;
    Ok(())
}
pub(crate) fn refresh_integrity(tx: &Transaction<'_>, session_id: Uuid) -> Result<()> {
    let incomplete:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM audio_gaps WHERE session_id=?1) OR EXISTS(
      SELECT 1 FROM segments s LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision
      WHERE s.session_id=?1 AND (r.revision IS NULL OR r.status='failed'))",[session_id.to_string()],|r|r.get(0))?;
    tx.execute(
        "UPDATE sessions SET incomplete=?1 WHERE id=?2",
        params![incomplete, session_id.to_string()],
    )?;
    Ok(())
}
fn track_matches(
    connection: &Connection,
    session_id: Uuid,
    track_id: Uuid,
    rate: u32,
) -> Result<()> {
    let row: Option<(String, u32)> = connection
        .query_row(
            "SELECT session_id,sample_rate FROM tracks WHERE id=?1",
            [track_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((session, sample_rate))
            if session == session_id.to_string() && sample_rate == rate =>
        {
            Ok(())
        }
        Some(_) => Err(StoreError::ScopeMismatch),
        None => Err(StoreError::NotFound("track")),
    }
}

impl Store {
    /// Trusted host only. The host must verify old process ownership/exit and
    /// replay completion; this operation is intentionally unavailable via ingest_json.
    pub fn reconcile_abandoned_producer(
        &mut self,
        session_id: Uuid,
        producer_run_id: Uuid,
        evidence: ProducerRecoveryEvidence,
    ) -> Result<Receipt> {
        non_nil(producer_run_id, "producer_run_id")?;
        non_blank(&evidence.reason, "reason")?;
        if !evidence.owned_process_exited || !evidence.outbox_replayed {
            return Err(StoreError::DependencyNotReady);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let spec = require_session(&tx, session_id)?;
        let status = load_session_status(&tx, session_id)?;
        if !status.capture_stopped {
            return Err(StoreError::InvalidState);
        }
        let hash: String = tx.query_row(
            "SELECT manifest_sha256 FROM capture_seals WHERE session_id=?1",
            [session_id.to_string()],
            |r| r.get(0),
        )?;
        if hash != evidence.capture_manifest_sha256 {
            return Err(StoreError::SealMismatch);
        }
        let existing:Option<(u64,String,String,String)>=tx.query_row("SELECT p.created_seq,e.message_id,p.payload_json,p.session_id FROM producer_reconciliations p JOIN ingested_events e ON e.store_seq=p.created_seq WHERE p.producer_run_id=?1",[producer_run_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((store_seq, message_id, body, owner)) = existing {
            if owner != session_id.to_string() {
                return Err(StoreError::ScopeMismatch);
            }
            let previous: ProducerReconciled = serde_json::from_str(&body)?;
            if previous.evidence != evidence {
                return Err(StoreError::EventIdConflict);
            }
            return Ok(Receipt {
                message_id: parse_uuid(&message_id)?,
                store_seq,
                duplicate: true,
            });
        }
        let foreign:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ingested_events WHERE producer_run_id=?1 AND session_id!=?2)",params![producer_run_id.to_string(),session_id.to_string()],|r|r.get(0))?;
        if foreign {
            return Err(StoreError::ScopeMismatch);
        }
        let already_sealed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM producer_seals WHERE producer_run_id=?1)",
            [producer_run_id.to_string()],
            |r| r.get(0),
        )?;
        if already_sealed {
            return Err(StoreError::InvalidState);
        }
        let final_seq = tx.query_row(
            "SELECT COALESCE(MAX(producer_seq),0) FROM ingested_events WHERE producer_run_id=?1",
            [producer_run_id.to_string()],
            |r| r.get(0),
        )?;
        let event = Event {
            schema_version: SCHEMA_VERSION,
            message_id: Uuid::new_v4(),
            event_type: EventType::ProducerReconciled,
            event_id: spec.event_id,
            room_id: spec.room_id,
            session_id,
            producer: Producer {
                name: "core-recovery".into(),
                run_id: Uuid::new_v4(),
                seq: 1,
            },
            payload: ProducerReconciled {
                producer_run_id,
                final_seq,
                evidence,
            },
        };
        let (receipt, body) = register_event(&tx, &event)?;
        tx.execute("INSERT INTO producer_reconciliations(producer_run_id,session_id,final_seq,payload_json,created_seq) VALUES(?1,?2,?3,?4,?5)",params![producer_run_id.to_string(),session_id.to_string(),final_seq,serde_json::to_string(&event.payload)?,receipt.store_seq])?;
        append_outbox(&tx, &event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn audio_gaps(&self, session_id: Uuid) -> Result<Vec<AudioGap>> {
        require_session(&self.connection, session_id)?;
        let mut s = self.connection.prepare(
            "SELECT payload_json FROM audio_gaps WHERE session_id=?1 ORDER BY created_seq",
        )?;
        let bodies = s
            .query_map([session_id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        bodies
            .into_iter()
            .map(|body| serde_json::from_str(&body).map_err(StoreError::from))
            .collect()
    }
    pub fn unsealed_producer_runs(&self, session_id: Uuid) -> Result<Vec<Uuid>> {
        require_session(&self.connection, session_id)?;
        let mut s=self.connection.prepare("SELECT DISTINCT e.producer_run_id FROM ingested_events e WHERE e.session_id=?1 AND e.event_type='transcript.final' AND NOT EXISTS(SELECT 1 FROM producer_seals p WHERE p.producer_run_id=e.producer_run_id) AND NOT EXISTS(SELECT 1 FROM producer_reconciliations p WHERE p.producer_run_id=e.producer_run_id) ORDER BY e.producer_run_id")?;
        let ids = s
            .query_map([session_id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| parse_uuid(&id)).collect()
    }
    pub fn capture_seal(&self, session_id: Uuid) -> Result<Option<CaptureStopped>> {
        require_session(&self.connection, session_id)?;
        let body: Option<String> = self
            .connection
            .query_row(
                "SELECT payload_json FROM capture_seals WHERE session_id=?1",
                [session_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        body.map(|body| serde_json::from_str(&body).map_err(StoreError::from))
            .transpose()
    }
    /// Terminal captures that need no ASR replay, without a recovery page limit.
    pub fn terminal_segment_ids_for_replay(&self, session_id: Uuid) -> Result<Vec<Uuid>> {
        require_session(&self.connection, session_id)?;
        let mut s=self.connection.prepare("SELECT s.id FROM segments s JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.session_id=?1 AND r.status IN ('success','empty') ORDER BY s.id")?;
        let ids = s
            .query_map([session_id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter().map(|id| parse_uuid(&id)).collect()
    }
    pub fn terminal_segment_ids(&self, session_id: Uuid) -> Result<Vec<Uuid>> {
        require_session(&self.connection, session_id)?;
        let mut s=self.connection.prepare("SELECT id FROM segments WHERE session_id=?1 AND current_revision IS NOT NULL ORDER BY id")?;
        let result = s
            .query_map([session_id.to_string()], |r| r.get::<_, String>(0))?
            .map(|r| parse_uuid(&r?))
            .collect();
        result
    }
    pub fn session_status(&self, session_id: Uuid) -> Result<SessionStatus> {
        load_session_status(&self.connection, session_id)
    }
    pub fn list_sessions(&self, limit: u32) -> Result<Vec<SessionSummary>> {
        validate_limit(limit)?;
        let mut statement = self
            .connection
            .prepare("SELECT spec_json FROM sessions ORDER BY rowid DESC LIMIT ?1")?;
        let specs = statement
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        specs.into_iter().map(|body|{let session:SessionSpec=serde_json::from_str(&body)?; let id=session.session_id;
            let cursor=self.connection.query_row("SELECT COALESCE(MAX(store_seq),0) FROM ingested_events WHERE session_id=?1",[id.to_string()],|r|r.get(0))?;
            let segment_count=self.connection.query_row("SELECT COUNT(*) FROM segments WHERE session_id=?1",[id.to_string()],|r|r.get(0))?;
            let translation_pending=self.connection.query_row("SELECT COUNT(*) FROM translation_coverage c JOIN segments s ON s.id=c.segment_id WHERE s.session_id=?1 AND c.state IN ('pending','requested','failed')",[id.to_string()],|r|r.get(0))?;
            Ok(SessionSummary{session,status:self.session_status(id)?,cursor,segment_count,translation_pending})}).collect()
    }
    pub fn transition_session(&mut self, event: &Event<SessionTransition>) -> Result<Receipt> {
        event.validate_envelope(EventType::SessionChanged)?;
        non_blank(&event.payload.reason, "reason")?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let status = load_session_status(&tx, event.session_id)?;
        if status.state != event.payload.expected_state
            || !status.state.permits(event.payload.next_state)
        {
            return Err(StoreError::InvalidState);
        }
        if event.payload.next_state == SessionState::Recording {
            let tracks: u32 = tx.query_row(
                "SELECT COUNT(*) FROM tracks WHERE session_id=?1",
                [event.session_id.to_string()],
                |r| r.get(0),
            )?;
            if tracks == 0 {
                return Err(StoreError::DependencyNotReady);
            }
        }
        tx.execute(
            "UPDATE sessions SET state=?1 WHERE id=?2",
            params![
                event.payload.next_state.as_str(),
                event.session_id.to_string()
            ],
        )?;
        if event.payload.next_state == SessionState::Interrupted {
            tx.execute(
                "UPDATE sessions SET incomplete=1 WHERE id=?1",
                [event.session_id.to_string()],
            )?;
        }
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub(crate) fn recover_interrupted_sessions(&mut self) -> Result<()> {
        let specs: Vec<SessionSpec> = {
            let mut s=self.connection.prepare("SELECT spec_json FROM sessions WHERE state IN ('preparing','ready','recording','stopping','draining')")?;
            let result = s
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .map(|s| serde_json::from_str(&s))
                .collect::<std::result::Result<_, _>>()?;
            result
        };
        for spec in specs {
            let state = self.session_status(spec.session_id)?.state;
            self.transition_session(&Event {
                schema_version: SCHEMA_VERSION,
                message_id: Uuid::new_v4(),
                event_type: EventType::SessionChanged,
                event_id: spec.event_id,
                room_id: spec.room_id,
                session_id: spec.session_id,
                producer: Producer {
                    name: "core-recovery".into(),
                    run_id: Uuid::new_v4(),
                    seq: 1,
                },
                payload: SessionTransition {
                    expected_state: state,
                    next_state: SessionState::Interrupted,
                    reason: "previous core process exited before finalization".into(),
                },
            })?;
        }
        Ok(())
    }
    pub fn register_gap(&mut self, event: &Event<AudioGap>) -> Result<Receipt> {
        event.validate_envelope(EventType::AudioGap)?;
        event.payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        track_matches(
            &tx,
            event.session_id,
            event.payload.track_id,
            event.payload.audio.sample_rate,
        )?;
        require_capture_state(&tx, event.session_id)?;
        tx.execute("INSERT INTO audio_gaps(gap_id,session_id,track_id,payload_json,created_seq) VALUES(?1,?2,?3,?4,?5)",params![event.payload.gap_id.to_string(),event.session_id.to_string(),event.payload.track_id.to_string(),serde_json::to_string(&event.payload)?,receipt.store_seq])?;
        tx.execute(
            "UPDATE sessions SET incomplete=1 WHERE id=?1",
            [event.session_id.to_string()],
        )?;
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn registered_segment_ids(&self, session_id: Uuid, track_id: Uuid) -> Result<Vec<Uuid>> {
        require_session(&self.connection, session_id)?;
        let mut query = self
            .connection
            .prepare("SELECT id FROM segments WHERE session_id=?1 AND track_id=?2 ORDER BY id")?;
        let result = query
            .query_map(params![session_id.to_string(), track_id.to_string()], |r| {
                r.get::<_, String>(0)
            })?
            .map(|r| parse_uuid(&r?))
            .collect();
        result
    }
    pub fn capture_stopped(&mut self, event: &Event<CaptureStopped>) -> Result<Receipt> {
        event.validate_envelope(EventType::CaptureStopped)?;
        event.payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let status = load_session_status(&tx, event.session_id)?;
        if status.capture_stopped
            || !matches!(
                status.state,
                SessionState::Stopping | SessionState::Interrupted
            )
        {
            return Err(StoreError::InvalidState);
        }
        let tracks: BTreeSet<String> = {
            let mut s = tx.prepare("SELECT id FROM tracks WHERE session_id=?1")?;
            let result = s
                .query_map([event.session_id.to_string()], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            result
        };
        if tracks
            != event
                .payload
                .tracks
                .iter()
                .map(|t| t.track_id.to_string())
                .collect()
        {
            return Err(StoreError::SealMismatch);
        }
        for track in &event.payload.tracks {
            let rows: Vec<(String, String)> = {
                let mut s=tx.prepare("SELECT s.id,c.audio_json FROM segments s JOIN capture_segments c ON c.segment_id=s.id WHERE s.session_id=?1 AND s.track_id=?2")?;
                let result = s
                    .query_map(
                        params![event.session_id.to_string(), track.track_id.to_string()],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?
                    .collect::<std::result::Result<_, _>>()?;
                result
            };
            let expected: BTreeSet<_> = track.segment_ids.iter().map(|id| id.to_string()).collect();
            let actual: BTreeSet<_> = rows.iter().map(|r| r.0.clone()).collect();
            if expected != actual {
                return Err(if actual.is_subset(&expected) {
                    StoreError::DependencyNotReady
                } else {
                    StoreError::SealMismatch
                });
            }
            for (_, audio) in rows {
                if serde_json::from_str::<AudioRange>(&audio)?.end_sample > track.final_sample {
                    return Err(StoreError::SealMismatch);
                }
            }
        }
        tx.execute("INSERT INTO capture_seals(session_id,manifest_sha256,payload_json,created_seq) VALUES(?1,?2,?3,?4)",params![event.session_id.to_string(),event.payload.manifest_sha256,serde_json::to_string(&event.payload)?,receipt.store_seq])?;
        tx.execute(
            "UPDATE sessions SET state='draining',capture_stopped_seq=?1 WHERE id=?2",
            params![receipt.store_seq, event.session_id.to_string()],
        )?;
        refresh_integrity(&tx, event.session_id)?;
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn producer_sealed(&mut self, event: &Event<ProducerSeal>) -> Result<Receipt> {
        event.validate_envelope(EventType::ProducerSealed)?;
        let p = &event.payload;
        if p.producer_run_id != event.producer.run_id
            || p.final_seq.checked_add(1) != Some(event.producer.seq)
        {
            return Err(StoreError::SealMismatch);
        }
        let expected: BTreeSet<_> = p.segment_ids.iter().copied().collect();
        if expected.len() != p.segment_ids.len() {
            return Err(StoreError::SealMismatch);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let run = p.producer_run_id.to_string();
        let max:u64=tx.query_row("SELECT COALESCE(MAX(producer_seq),0) FROM ingested_events WHERE producer_run_id=?1 AND store_seq!=?2",params![run,receipt.store_seq],|r|r.get(0))?;
        if max != p.final_seq {
            return Err(StoreError::DependencyNotReady);
        }
        let foreign:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ingested_events WHERE producer_run_id=?1 AND session_id!=?2)",params![run,event.session_id.to_string()],|r|r.get(0))?;
        if foreign {
            return Err(StoreError::ScopeMismatch);
        }
        let ids: Vec<String> = {
            let mut s=tx.prepare("SELECT json_extract(body_json,'$.payload.segment_id') FROM ingested_events WHERE producer_run_id=?1 AND event_type='transcript.final'")?;
            let result = s
                .query_map([run.clone()], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            result
        };
        let actual: BTreeSet<_> = ids.iter().map(|s| parse_uuid(s)).collect::<Result<_>>()?;
        if expected != actual {
            return Err(StoreError::SealMismatch);
        }
        tx.execute("INSERT INTO producer_seals(producer_run_id,session_id,final_seq,payload_json,created_seq) VALUES(?1,?2,?3,?4,?5)",params![run,event.session_id.to_string(),p.final_seq,serde_json::to_string(p)?,receipt.store_seq])?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn seal_transcript(&mut self, event: &Event<TranscriptSeal>) -> Result<Receipt> {
        event.validate_envelope(EventType::TranscriptSealed)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let status = load_session_status(&tx, event.session_id)?;
        if !status.capture_stopped
            || status.transcript_sealed
            || !matches!(
                status.state,
                SessionState::Draining | SessionState::Interrupted
            )
        {
            return Err(StoreError::InvalidState);
        }
        let hash: String = tx.query_row(
            "SELECT manifest_sha256 FROM capture_seals WHERE session_id=?1",
            [event.session_id.to_string()],
            |r| r.get(0),
        )?;
        if hash != event.payload.capture_manifest_sha256 {
            return Err(StoreError::SealMismatch);
        }
        let missing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM segments WHERE session_id=?1 AND current_revision IS NULL)",[event.session_id.to_string()],|r|r.get(0))?;
        if missing {
            return Err(StoreError::DependencyNotReady);
        }
        let unsealed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ingested_events e WHERE e.session_id=?1 AND e.event_type='transcript.final' AND NOT EXISTS(SELECT 1 FROM producer_seals p WHERE p.producer_run_id=e.producer_run_id) AND NOT EXISTS(SELECT 1 FROM producer_reconciliations p WHERE p.producer_run_id=e.producer_run_id))",[event.session_id.to_string()],|r|r.get(0))?;
        if unsealed {
            return Err(StoreError::DependencyNotReady);
        }
        tx.execute(
            "UPDATE sessions SET state='completed',sealed_seq=?1 WHERE id=?2",
            params![receipt.store_seq, event.session_id.to_string()],
        )?;
        refresh_integrity(&tx, event.session_id)?;
        save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    /// Exact bounded recovery lookup: one reply per input ID in input order.
    /// Missing/cross-session/duplicate IDs reject the whole request. A null next
    /// revision means current success/empty, never an implicit revision 1.
    pub fn recovery_revisions(
        &self,
        session_id: Uuid,
        segment_ids: Vec<Uuid>,
    ) -> Result<Vec<RecoveryRevision>> {
        require_session(&self.connection, session_id)?;
        if segment_ids.len() > 256 {
            return Err(ValidationError::Invalid("recovery_id_limit").into());
        }
        let mut seen = BTreeSet::new();
        let mut result = Vec::with_capacity(segment_ids.len());
        for segment_id in segment_ids {
            non_nil(segment_id, "segment_id")?;
            if !seen.insert(segment_id) {
                return Err(ValidationError::Invalid("duplicate_segment").into());
            }
            let (_, _, current) = capture_context(&self.connection, session_id, segment_id)?;
            let next_revision = match current {
                None => Some(Revision::FIRST),
                Some(current) => {
                    let record = load_revision(
                        &self.connection,
                        session_id,
                        segment_id,
                        current.try_into()?,
                    )?;
                    if record.payload.status == TranscriptStatus::Failed {
                        Some(record.payload.revision.next()?)
                    } else {
                        None
                    }
                }
            };
            result.push(RecoveryRevision {
                segment_id,
                terminal: next_revision.is_none(),
                next_revision,
            });
        }
        Ok(result)
    }
    pub fn recovery_segments(&self, session_id: Uuid, limit: u32) -> Result<Vec<RecoverySegment>> {
        validate_limit(limit)?;
        require_session(&self.connection, session_id)?;
        let mut s=self.connection.prepare("SELECT s.id,s.track_id,c.audio_json,c.recording_ref,r.record_json FROM segments s JOIN capture_segments c ON c.segment_id=s.id LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.session_id=?1 AND (r.revision IS NULL OR r.status='failed') ORDER BY c.created_seq LIMIT ?2")?;
        let rows = s.query_map(params![session_id.to_string(), limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        rows.map(|row| {
            let (segment, track, audio, recording_ref, body) = row?;
            let previous: Option<TranscriptRecord> =
                body.map(|s| serde_json::from_str(&s)).transpose()?;
            Ok(RecoverySegment {
                segment_id: parse_uuid(&segment)?,
                track_id: parse_uuid(&track)?,
                audio: serde_json::from_str(&audio)?,
                recording_ref,
                next_revision: previous
                    .as_ref()
                    .map(|r| r.payload.revision.next())
                    .transpose()?
                    .unwrap_or(Revision::FIRST),
                previous_status: previous.as_ref().map(|r| r.payload.status),
                reason: previous.and_then(|r| r.payload.reason),
            })
        })
        .collect()
    }
    /// Bounded live projection: last audio-ordered captures, returned ascending.
    pub fn snapshot_tail(&mut self, session_id: Uuid, limit: u32) -> Result<SnapshotPage> {
        validate_limit(limit)?;
        let tx = self.connection.transaction()?;
        let cursor: u64 = tx.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |r| r.get(0),
        )?;
        let session = require_session(&tx, session_id)?;
        let status = load_session_status(&tx, session_id)?;
        let mut items = {
            let mut s=tx.prepare("SELECT s.id,s.track_id,c.audio_json,c.recording_ref,r.record_json FROM segments s JOIN capture_segments c ON c.segment_id=s.id LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.session_id=?1 ORDER BY json_extract(c.audio_json,'$.start_ms') DESC,s.track_id DESC,s.id DESC LIMIT ?2")?;
            let rows = s.query_map(params![session_id.to_string(), limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            rows.map(|row| {
                let (segment, track, audio, recording_ref, body) = row?;
                Ok(SnapshotItem {
                    segment_id: parse_uuid(&segment)?,
                    track_id: parse_uuid(&track)?,
                    audio: serde_json::from_str(&audio)?,
                    recording_ref,
                    transcript: body.map(|s| serde_json::from_str(&s)).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?
        };
        items.reverse();
        tx.commit()?;
        Ok(SnapshotPage {
            session,
            status,
            cursor,
            items,
            next_after: None,
        })
    }
    pub fn snapshot_page(
        &mut self,
        session_id: Uuid,
        cursor: Option<u64>,
        after: Option<PageKey>,
        limit: u32,
    ) -> Result<SnapshotPage> {
        validate_limit(limit)?;
        let tx = self.connection.transaction()?;
        let max: u64 = tx.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |r| r.get(0),
        )?;
        let cursor = cursor.unwrap_or(max);
        if cursor > max {
            return Err(StoreError::InvalidCursor);
        }
        let session = require_session(&tx, session_id)?;
        let mut status = load_session_status(&tx, session_id)?;
        if status.state_seq > cursor {
            let body:Option<String>=tx.query_row("SELECT status_json FROM session_history WHERE session_id=?1 AND store_seq<=?2 ORDER BY store_seq DESC LIMIT 1",params![session_id.to_string(),cursor],|r|r.get(0)).optional()?;
            status = if let Some(body) = body {
                serde_json::from_str(&body)?
            } else {
                SessionStatus {
                    session_id,
                    state: SessionState::Created,
                    direction_epoch: 1,
                    target_languages: vec![],
                    capture_stopped: false,
                    transcript_sealed: false,
                    incomplete: false,
                    state_seq: 0,
                }
            };
        }
        if after.as_ref().is_some_and(|k| {
            k.start_ms > i64::MAX as u64 || k.track_id.is_nil() || k.segment_id.is_nil()
        }) {
            return Err(StoreError::InvalidCursor);
        }
        let (after_ms, after_track, after_segment) = after
            .map(|k| {
                (
                    k.start_ms as i64,
                    k.track_id.to_string(),
                    k.segment_id.to_string(),
                )
            })
            .unwrap_or((-1, String::new(), String::new()));
        let items = {
            let mut s=tx.prepare("SELECT s.id,s.track_id,c.audio_json,c.recording_ref,r.record_json FROM segments s JOIN capture_segments c ON c.segment_id=s.id LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=(SELECT MAX(v.revision) FROM segment_revisions v WHERE v.segment_id=s.id AND v.created_seq<=?2) WHERE s.session_id=?1 AND c.created_seq<=?2 AND (json_extract(c.audio_json,'$.start_ms'),s.track_id,s.id)>(?3,?4,?5) ORDER BY json_extract(c.audio_json,'$.start_ms'),s.track_id,s.id LIMIT ?6")?;
            let rows = s.query_map(
                params![
                    session_id.to_string(),
                    cursor,
                    after_ms,
                    after_track,
                    after_segment,
                    limit
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                },
            )?;
            rows.map(|row| {
                let (segment, track, audio, recording_ref, body) = row?;
                Ok(SnapshotItem {
                    segment_id: parse_uuid(&segment)?,
                    track_id: parse_uuid(&track)?,
                    audio: serde_json::from_str(&audio)?,
                    recording_ref,
                    transcript: body.map(|s| serde_json::from_str(&s)).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?
        };
        let next_after = if items.len() == limit as usize {
            items.last().map(|r| PageKey {
                start_ms: r.audio.start_ms,
                track_id: r.track_id,
                segment_id: r.segment_id,
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(SnapshotPage {
            session,
            status,
            cursor,
            items,
            next_after,
        })
    }
}
pub(crate) fn validate_limit(limit: u32) -> Result<()> {
    if !(1..=1000).contains(&limit) {
        Err(ValidationError::Invalid("page_limit").into())
    } else {
        Ok(())
    }
}
