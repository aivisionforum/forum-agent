use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranslationWork {
    pub source_span: SourceSpan,
    pub configured_source_language: String,
    pub detected_language: Option<String>,
    pub target_language: String,
    pub direction_epoch: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranslationRecord {
    pub session_id: Uuid,
    pub request: TranslationRequested,
    pub result: Option<TranslationFinal>,
    pub state: String,
    pub text: Option<String>,
    pub error: Option<String>,
    pub created_seq: u64,
    pub updated_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionExport {
    pub snapshot: SessionSnapshot,
    /// Every registered capture, including missing, empty and failed ASR.
    pub sources: Vec<SnapshotItem>,
    pub status: SessionStatus,
    pub translations: Vec<TranslationRecord>,
    pub gaps: Vec<AudioGap>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranslationPageKey {
    pub created_seq: u64,
    pub translation_id: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TranslationPage {
    pub cursor: u64,
    pub items: Vec<TranslationRecord>,
    pub next_after: Option<TranslationPageKey>,
}

fn save_translation_history(
    tx: &Transaction<'_>,
    session_id: Uuid,
    id: Uuid,
    seq: u64,
) -> Result<()> {
    let record = load_translation(tx, session_id, id)?;
    tx.execute("INSERT OR REPLACE INTO translation_history(translation_id,store_seq,record_json) VALUES(?1,?2,?3)",params![id.to_string(),seq,serde_json::to_string(&record)?])?;
    Ok(())
}
fn save_all_translation_history(tx: &Transaction<'_>, session_id: Uuid, seq: u64) -> Result<()> {
    let mut s = tx.prepare("SELECT id FROM translations WHERE session_id=?1")?;
    let ids = s
        .query_map([session_id.to_string()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in ids {
        save_translation_history(tx, session_id, parse_uuid(&id)?, seq)?;
    }
    Ok(())
}
pub(crate) fn invalidate_source_translations(
    tx: &Transaction<'_>,
    session_id: Uuid,
    segment: Uuid,
    revision: Revision,
    seq: u64,
) -> Result<()> {
    tx.execute("UPDATE translation_attempts SET state='stale',updated_seq=?1 WHERE (translation_id,revision) IN
     (SELECT p.translation_id,p.revision FROM translation_sources p JOIN translations t ON t.id=p.translation_id WHERE t.session_id=?2 AND p.segment_id=?3 AND p.segment_revision=?4)",
        params![seq,session_id.to_string(),segment.to_string(),revision.get()])?;
    let mut s=tx.prepare("SELECT DISTINCT t.id FROM translations t JOIN translation_sources p ON p.translation_id=t.id WHERE t.session_id=?1 AND p.revision=t.current_revision AND p.segment_id=?2 AND p.segment_revision=?3")?;
    let ids = s
        .query_map(
            params![session_id.to_string(), segment.to_string(), revision.get()],
            |r| r.get::<_, String>(0),
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in ids {
        let id = parse_uuid(&id)?;
        let record = load_translation(tx, session_id, id)?;
        // Invalidate the whole combined translation and release unaffected
        // source ranges. Otherwise another segment in this request is stranded.
        release_coverage(tx, session_id, id, record.request.revision)?;
        save_translation_history(tx, session_id, id, seq)?;
    }
    Ok(())
}

fn current_sources(
    connection: &Connection,
    session_id: Uuid,
    p: &TranslationRequested,
) -> Result<()> {
    for span in &p.source_spans {
        let (_, _, current) = capture_context(connection, session_id, span.segment_id)?;
        let epoch: u64 = connection.query_row(
            "SELECT direction_epoch FROM capture_segments WHERE segment_id=?1",
            [span.segment_id.to_string()],
            |r| r.get(0),
        )?;
        if epoch != p.direction_epoch {
            return Err(StoreError::LateResult);
        }
        if current != Some(span.segment_revision.get()) {
            return Err(if current.is_none() {
                StoreError::DependencyNotReady
            } else {
                StoreError::LateResult
            });
        }
        let record = load_revision(
            connection,
            session_id,
            span.segment_id,
            span.segment_revision,
        )?;
        if record.payload.status != TranscriptStatus::Success {
            return Err(StoreError::DependencyNotReady);
        }
        span.validate_against(&record.payload.text)?;
    }
    Ok(())
}

fn load_translation(
    connection: &Connection,
    session_id: Uuid,
    id: Uuid,
) -> Result<TranslationRecord> {
    let row:Option<(String,String,String,Option<String>,Option<String>,Option<String>,u64,u64)>=connection.query_row(
        "SELECT t.session_id,a.request_json,a.state,a.text,a.error,a.result_json,(SELECT MIN(v.created_seq) FROM translation_attempts v WHERE v.translation_id=t.id),a.updated_seq FROM translations t JOIN translation_attempts a ON a.translation_id=t.id AND a.revision=t.current_revision AND a.attempt=t.current_attempt WHERE t.id=?1",[id.to_string()],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
    let (owner, request, state, text, error, result, created_seq, updated_seq) =
        row.ok_or(StoreError::DependencyNotReady)?;
    if owner != session_id.to_string() {
        return Err(StoreError::ScopeMismatch);
    }
    Ok(TranslationRecord {
        session_id,
        request: serde_json::from_str(&request)?,
        state,
        text,
        error,
        result: result.map(|s| serde_json::from_str(&s)).transpose()?,
        created_seq,
        updated_seq,
    })
}

fn fence(
    connection: &Connection,
    session_id: Uuid,
    id: Uuid,
    revision: Revision,
    attempt: u32,
    epoch: u64,
) -> Result<TranslationRecord> {
    let record = load_translation(connection, session_id, id)?;
    if record.request.revision != revision
        || record.request.attempt != attempt
        || record.request.direction_epoch != epoch
        || record.state != "requested"
    {
        return Err(StoreError::LateResult);
    }
    current_sources(connection, session_id, &record.request)?;
    Ok(record)
}

fn release_coverage(
    tx: &Transaction<'_>,
    session_id: Uuid,
    id: Uuid,
    revision: Revision,
) -> Result<()> {
    require_session(tx, session_id)?;
    tx.execute("UPDATE translation_coverage SET state=CASE WHEN direction_epoch=(SELECT c.direction_epoch FROM capture_segments c WHERE c.segment_id=translation_coverage.segment_id) AND segment_revision=(SELECT current_revision FROM segments WHERE id=translation_coverage.segment_id) THEN 'pending' ELSE 'stale' END,translation_id=NULL,translation_revision=NULL,attempt=NULL WHERE translation_id=?1 AND translation_revision=?2",
        params![id.to_string(),revision.get()])?;
    Ok(())
}
fn coverage_piece(
    tx: &Transaction<'_>,
    span: &SourceSpan,
    p: &TranslationRequested,
    start: usize,
    end: usize,
    owned: bool,
) -> Result<()> {
    if start == end {
        return Ok(());
    }
    tx.execute("INSERT INTO translation_coverage(segment_id,segment_revision,target_language,direction_epoch,start_utf8,end_utf8,state,translation_id,translation_revision,attempt) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![span.segment_id.to_string(),span.segment_revision.get(),p.target_language,p.direction_epoch,start,end,
            if owned{"requested"}else{"pending"},owned.then(||p.translation_id.to_string()),owned.then(||p.revision.get()),owned.then_some(p.attempt)])?;
    Ok(())
}
fn claim_span(tx: &Transaction<'_>, span: &SourceSpan, p: &TranslationRequested) -> Result<()> {
    let rows: Vec<(usize, usize, String)> = {
        let mut s=tx.prepare("SELECT start_utf8,end_utf8,state FROM translation_coverage WHERE segment_id=?1 AND segment_revision=?2 AND target_language=?3 AND direction_epoch=?4 AND end_utf8>?5 AND start_utf8<?6 ORDER BY start_utf8")?;
        let result = s
            .query_map(
                params![
                    span.segment_id.to_string(),
                    span.segment_revision.get(),
                    p.target_language,
                    p.direction_epoch,
                    span.start_utf8,
                    span.end_utf8
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?
            .collect::<std::result::Result<_, _>>()?;
        result
    };
    let mut cursor = span.start_utf8;
    for (start, end, state) in rows {
        let left = start.max(span.start_utf8);
        let right = end.min(span.end_utf8);
        if state != "pending" || left != cursor {
            return Err(StoreError::CoverageConflict);
        }
        tx.execute("DELETE FROM translation_coverage WHERE segment_id=?1 AND segment_revision=?2 AND target_language=?3 AND direction_epoch=?4 AND start_utf8=?5 AND end_utf8=?6",
            params![span.segment_id.to_string(),span.segment_revision.get(),p.target_language,p.direction_epoch,start,end])?;
        coverage_piece(tx, span, p, start, left, false)?;
        coverage_piece(tx, span, p, left, right, true)?;
        coverage_piece(tx, span, p, right, end, false)?;
        cursor = right;
    }
    if cursor != span.end_utf8 {
        return Err(StoreError::CoverageConflict);
    }
    Ok(())
}

impl Store {
    pub fn translation_records_for_segments_at(
        &self,
        session_id: Uuid,
        segments: Vec<Uuid>,
        cursor: u64,
    ) -> Result<Vec<TranslationRecord>> {
        require_session(&self.connection, session_id)?;
        if segments.len() > 1000 {
            return Err(ValidationError::Invalid("segment_limit").into());
        }
        let max: u64 = self.connection.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |r| r.get(0),
        )?;
        let floor: u64 = self.connection.query_row(
            "SELECT min_cursor FROM snapshot_floors WHERE projection='translation_history'",
            [],
            |r| r.get(0),
        )?;
        if cursor > max || cursor < floor {
            return Err(StoreError::InvalidCursor);
        }
        let mut s=self.connection.prepare("SELECT h.record_json FROM translation_history h JOIN translations t ON t.id=h.translation_id WHERE t.session_id=?1 AND h.store_seq=(SELECT MAX(v.store_seq) FROM translation_history v WHERE v.translation_id=t.id AND v.store_seq<=?2) AND EXISTS(SELECT 1 FROM json_each(json_extract(h.record_json,'$.request.source_spans')) p WHERE json_extract(p.value,'$.segment_id') IN (SELECT value FROM json_each(?3))) ORDER BY t.id")?;
        let bodies = s
            .query_map(
                params![
                    session_id.to_string(),
                    cursor,
                    serde_json::to_string(&segments)?
                ],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        bodies
            .into_iter()
            .map(|body| serde_json::from_str(&body).map_err(StoreError::from))
            .collect()
    }
    pub fn translation_records_for_segments(
        &self,
        session_id: Uuid,
        segments: Vec<Uuid>,
    ) -> Result<Vec<TranslationRecord>> {
        require_session(&self.connection, session_id)?;
        if segments.len() > 1000 {
            return Err(ValidationError::Invalid("segment_limit").into());
        }
        let mut s=self.connection.prepare("SELECT DISTINCT t.id FROM translations t JOIN translation_sources p ON p.translation_id=t.id AND p.revision=t.current_revision WHERE t.session_id=?1 AND p.segment_id IN (SELECT value FROM json_each(?2)) ORDER BY t.id")?;
        let ids = s
            .query_map(
                params![session_id.to_string(), serde_json::to_string(&segments)?],
                |r| r.get::<_, String>(0),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| load_translation(&self.connection, session_id, parse_uuid(&id)?))
            .collect()
    }
    /// Unbounded local export. Call through the actor for a stable operation;
    /// stream snapshot_page/translation_page for interactive clients instead.
    pub fn export_session(&mut self, session_id: Uuid) -> Result<SessionExport> {
        // Store is the only permitted writer and this method cannot interleave
        // with another actor command. Snapshot cursor binds both projections.
        let snapshot = self.session_snapshot(session_id)?;
        let status = self.session_status(session_id)?;
        let mut sources = Vec::new();
        let mut source_after = None;
        loop {
            let page = self.snapshot_page(session_id, Some(snapshot.cursor), source_after, 1000)?;
            sources.extend(page.items);
            source_after = page.next_after;
            if source_after.is_none() {
                break;
            }
        }
        let mut translations = Vec::new();
        let mut after = None;
        loop {
            let page = self.translation_page(session_id, Some(snapshot.cursor), after, 1000)?;
            translations.extend(page.items);
            after = page.next_after;
            if after.is_none() {
                break;
            }
        }
        let gaps = self.audio_gaps(session_id)?;
        Ok(SessionExport {
            snapshot,
            sources,
            status,
            translations,
            gaps,
        })
    }
    pub fn translation_page(
        &mut self,
        session_id: Uuid,
        cursor: Option<u64>,
        after: Option<TranslationPageKey>,
        limit: u32,
    ) -> Result<TranslationPage> {
        validate_limit(limit)?;
        let tx = self.connection.transaction()?;
        require_session(&tx, session_id)?;
        let max: u64 = tx.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |r| r.get(0),
        )?;
        let floor: u64 = tx.query_row(
            "SELECT min_cursor FROM snapshot_floors WHERE projection='translation_history'",
            [],
            |r| r.get(0),
        )?;
        let cursor = cursor.unwrap_or(max);
        if cursor > max || cursor < floor {
            return Err(StoreError::InvalidCursor);
        }
        if after
            .as_ref()
            .is_some_and(|k| k.created_seq > cursor || k.translation_id.is_nil())
        {
            return Err(StoreError::InvalidCursor);
        }
        let (after_seq, after_id) = after
            .map(|k| (k.created_seq, k.translation_id.to_string()))
            .unwrap_or((0, String::new()));
        let items = {
            let mut s=tx.prepare("SELECT h.record_json FROM translation_history h JOIN translations t ON t.id=h.translation_id WHERE t.session_id=?1 AND h.store_seq=(SELECT MAX(v.store_seq) FROM translation_history v WHERE v.translation_id=t.id AND v.store_seq<=?2) AND (json_extract(h.record_json,'$.created_seq'),t.id)>(?3,?4) ORDER BY json_extract(h.record_json,'$.created_seq'),t.id LIMIT ?5")?;
            let rows = s
                .query_map(
                    params![session_id.to_string(), cursor, after_seq, after_id, limit],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|body| serde_json::from_str(&body).map_err(StoreError::from))
                .collect::<Result<Vec<TranslationRecord>>>()?
        };
        let next_after = if items.len() == limit as usize {
            items.last().map(|r| TranslationPageKey {
                created_seq: r.created_seq,
                translation_id: r.request.translation_id,
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(TranslationPage {
            cursor,
            items,
            next_after,
        })
    }
    pub fn pending_translation_work(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<TranslationWork>> {
        validate_limit(limit)?;
        require_session(&self.connection, session_id)?;
        let mut s=self.connection.prepare("SELECT c.segment_id,c.segment_revision,c.target_language,c.direction_epoch,c.start_utf8,c.end_utf8,r.record_json FROM translation_coverage c JOIN segments s ON s.id=c.segment_id JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=c.segment_revision WHERE s.session_id=?1 AND c.state='pending' AND s.current_revision=c.segment_revision AND c.direction_epoch=(SELECT direction_epoch FROM capture_segments WHERE segment_id=s.id) ORDER BY r.created_seq,c.start_utf8,c.target_language LIMIT ?2")?;
        let rows = s.query_map(params![session_id.to_string(), limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, usize>(4)?,
                r.get::<_, usize>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?;
        rows.map(|row| {
            let (segment, revision, target_language, direction_epoch, start_utf8, end_utf8, body) =
                row?;
            let record: TranscriptRecord = serde_json::from_str(&body)?;
            let quote = record
                .payload
                .text
                .get(start_utf8..end_utf8)
                .ok_or(ValidationError::InvalidSpan)?
                .to_owned();
            Ok(TranslationWork {
                source_span: SourceSpan {
                    segment_id: parse_uuid(&segment)?,
                    segment_revision: revision.try_into()?,
                    start_utf8,
                    end_utf8,
                    quote,
                },
                configured_source_language: record.payload.configured_source_language,
                detected_language: record.payload.detected_language,
                target_language,
                direction_epoch,
            })
        })
        .collect()
    }
    pub fn translation_requests(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<TranslationRecord>> {
        self.query_translations(session_id, limit, false)
    }
    pub fn unresolved_translation_page(
        &mut self,
        session_id: Uuid,
        after: Option<TranslationPageKey>,
        limit: u32,
    ) -> Result<TranslationPage> {
        validate_limit(limit)?;
        let tx = self.connection.transaction()?;
        require_session(&tx, session_id)?;
        let cursor: u64 = tx.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |r| r.get(0),
        )?;
        if after
            .as_ref()
            .is_some_and(|k| k.created_seq > cursor || k.translation_id.is_nil())
        {
            return Err(StoreError::InvalidCursor);
        }
        let (after_seq, after_id) = after
            .map(|k| (k.created_seq, k.translation_id.to_string()))
            .unwrap_or((0, String::new()));
        let ids = {
            let mut s=tx.prepare("SELECT t.id FROM translations t JOIN translation_attempts a ON a.translation_id=t.id AND a.revision=t.current_revision AND a.attempt=t.current_attempt WHERE t.session_id=?1 AND a.state IN ('requested','failed') AND ((SELECT MIN(v.created_seq) FROM translation_attempts v WHERE v.translation_id=t.id),t.id)>(?2,?3) ORDER BY (SELECT MIN(v.created_seq) FROM translation_attempts v WHERE v.translation_id=t.id),t.id LIMIT ?4")?;
            let ids = s
                .query_map(
                    params![session_id.to_string(), after_seq, after_id, limit],
                    |r| r.get::<_, String>(0),
                )?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ids
        };
        let items = ids
            .into_iter()
            .map(|id| load_translation(&tx, session_id, parse_uuid(&id)?))
            .collect::<Result<Vec<_>>>()?;
        let next_after = if items.len() == limit as usize {
            items.last().map(|r| TranslationPageKey {
                created_seq: r.created_seq,
                translation_id: r.request.translation_id,
            })
        } else {
            None
        };
        tx.commit()?;
        Ok(TranslationPage {
            cursor,
            items,
            next_after,
        })
    }
    pub fn unresolved_translation_requests(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<TranslationRecord>> {
        self.query_translations(session_id, limit, true)
    }
    pub fn translation_records(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<TranslationRecord>> {
        self.translation_requests(session_id, limit)
    }
    pub fn translation_snapshot(
        &self,
        session_id: Uuid,
        limit: u32,
    ) -> Result<Vec<TranslationRecord>> {
        self.translation_requests(session_id, limit)
    }
    fn query_translations(
        &self,
        session_id: Uuid,
        limit: u32,
        unresolved: bool,
    ) -> Result<Vec<TranslationRecord>> {
        validate_limit(limit)?;
        require_session(&self.connection, session_id)?;
        let mut s=self.connection.prepare("SELECT t.id FROM translations t JOIN translation_attempts a ON a.translation_id=t.id AND a.revision=t.current_revision AND a.attempt=t.current_attempt WHERE t.session_id=?1 AND (?2=0 OR a.state IN ('requested','failed')) ORDER BY a.created_seq LIMIT ?3")?;
        let ids = s
            .query_map(params![session_id.to_string(), unresolved, limit], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| load_translation(&self.connection, session_id, parse_uuid(&id)?))
            .collect()
    }
    pub fn request_translation(&mut self, event: &Event<TranslationRequested>) -> Result<Receipt> {
        event.validate_envelope(EventType::TranslationRequested)?;
        event.payload.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let p = &event.payload;
        current_sources(&tx, event.session_id, p)?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM translations WHERE id=?1)",
            [p.translation_id.to_string()],
            |r| r.get(0),
        )?;
        let mut new_revision = true;
        if existing {
            let old = load_translation(&tx, event.session_id, p.translation_id)?;
            if p.revision == old.request.revision {
                let mut same = old.request.clone();
                same.attempt = p.attempt;
                if old.request.attempt.checked_add(1) != Some(p.attempt)
                    || same != *p
                    || !matches!(old.state.as_str(), "requested" | "failed")
                {
                    return Err(StoreError::LateResult);
                }
                new_revision = false;
            } else if old.request.revision.next()? != p.revision || p.attempt != 1 {
                return Err(StoreError::LateResult);
            }
            release_coverage(
                &tx,
                event.session_id,
                p.translation_id,
                old.request.revision,
            )?;
            tx.execute("UPDATE translation_attempts SET state='stale',updated_seq=?1 WHERE translation_id=?2 AND revision=?3 AND attempt=?4 AND state='requested'",params![receipt.store_seq,p.translation_id.to_string(),old.request.revision.get(),old.request.attempt])?;
            tx.execute(
                "UPDATE translations SET current_revision=?1,current_attempt=?2 WHERE id=?3",
                params![p.revision.get(), p.attempt, p.translation_id.to_string()],
            )?;
        } else {
            if p.revision != Revision::FIRST || p.attempt != 1 {
                return Err(StoreError::LateResult);
            }
            tx.execute("INSERT INTO translations(id,session_id,current_revision,current_attempt) VALUES(?1,?2,?3,?4)",params![p.translation_id.to_string(),event.session_id.to_string(),p.revision.get(),p.attempt])?;
        }
        tx.execute("INSERT INTO translation_attempts(translation_id,revision,attempt,request_json,state,created_seq,updated_seq) VALUES(?1,?2,?3,?4,'requested',?5,?5)",params![p.translation_id.to_string(),p.revision.get(),p.attempt,serde_json::to_string(p)?,receipt.store_seq])?;
        if new_revision {
            for (index, span) in p.source_spans.iter().enumerate() {
                tx.execute("INSERT INTO translation_sources(translation_id,revision,source_index,segment_id,segment_revision,start_utf8,end_utf8,quote) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![p.translation_id.to_string(),p.revision.get(),index,span.segment_id.to_string(),span.segment_revision.get(),span.start_utf8,span.end_utf8,span.quote])?;
            }
        }
        for span in &p.source_spans {
            claim_span(&tx, span, p)?;
        }
        save_translation_history(&tx, event.session_id, p.translation_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn finish_translation(&mut self, event: &Event<TranslationFinal>) -> Result<Receipt> {
        event.validate_envelope(EventType::TranslationFinal)?;
        event.payload.validate()?;
        let p = &event.payload;
        if p.status == TranslationStatus::Final {
            non_blank(&p.text, "text")?;
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        let record = fence(
            &tx,
            event.session_id,
            p.translation_id,
            p.revision,
            p.attempt,
            p.direction_epoch,
        )?;
        if p.backend != record.request.backend
            || p.model_manifest_id != record.request.model_manifest_id
            || (p.status == TranslationStatus::Passthrough && p.text != record.request.input_text)
        {
            return Err(ValidationError::Invalid("translation_result_provenance").into());
        }
        let state = if p.status == TranslationStatus::Final {
            "final"
        } else {
            "passthrough"
        };
        tx.execute("UPDATE translation_attempts SET state=?1,text=?2,result_json=?3,updated_seq=?4 WHERE translation_id=?5 AND revision=?6 AND attempt=?7",params![state,p.text,serde_json::to_string(p)?,receipt.store_seq,p.translation_id.to_string(),p.revision.get(),p.attempt])?;
        let changed=tx.execute("UPDATE translation_coverage SET state='final' WHERE translation_id=?1 AND translation_revision=?2 AND attempt=?3 AND state='requested'",params![p.translation_id.to_string(),p.revision.get(),p.attempt])?;
        if changed == 0 {
            return Err(StoreError::CoverageConflict);
        }
        save_translation_history(&tx, event.session_id, p.translation_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn fail_translation(&mut self, event: &Event<TranslationFailed>) -> Result<Receipt> {
        event.validate_envelope(EventType::TranslationFailed)?;
        event.payload.validate()?;
        let p = &event.payload;
        non_blank(&p.code, "code")?;
        non_blank(&p.reason, "reason")?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        fence(
            &tx,
            event.session_id,
            p.translation_id,
            p.revision,
            p.attempt,
            p.direction_epoch,
        )?;
        tx.execute("UPDATE translation_attempts SET state='failed',error=?1,updated_seq=?2 WHERE translation_id=?3 AND revision=?4 AND attempt=?5",params![format!("{}: {}",p.code,p.reason),receipt.store_seq,p.translation_id.to_string(),p.revision.get(),p.attempt])?;
        tx.execute("UPDATE translation_coverage SET state='failed' WHERE translation_id=?1 AND translation_revision=?2 AND attempt=?3 AND state='requested'",params![p.translation_id.to_string(),p.revision.get(),p.attempt])?;
        save_translation_history(&tx, event.session_id, p.translation_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn change_direction(&mut self, event: &Event<DirectionChanged>) -> Result<Receipt> {
        event.validate_envelope(EventType::DirectionChanged)?;
        let p = &event.payload;
        if p.direction_epoch == 0
            || p.expected_epoch.checked_add(1) != Some(p.direction_epoch)
            || p.direction_epoch > i64::MAX as u64
        {
            return Err(ValidationError::Invalid("direction_epoch").into());
        }
        let mut unique = std::collections::HashSet::new();
        for target in &p.target_languages {
            non_blank(target, "target_languages")?;
            if !unique.insert(target) {
                return Err(ValidationError::Invalid("target_languages").into());
            }
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (receipt, body) = register_event(&tx, event)?;
        if receipt.duplicate {
            return Ok(receipt);
        }
        if load_session_status(&tx, event.session_id)?.direction_epoch != p.expected_epoch {
            return Err(StoreError::LateResult);
        }
        tx.execute(
            "UPDATE sessions SET direction_epoch=?1,targets_json=?2 WHERE id=?3",
            params![
                p.direction_epoch,
                serde_json::to_string(&p.target_languages)?,
                event.session_id.to_string()
            ],
        )?;
        if apply_direction_boundary(&tx, event, receipt.store_seq)? {
            save_history(&tx, event.session_id, receipt.store_seq)?;
            append_outbox(&tx, event, &receipt, &body)?;
            tx.commit()?;
            return Ok(receipt);
        }
        tx.execute(
            "UPDATE sessions SET retroactive_epoch=?1,retroactive_targets_json=?2 WHERE id=?3",
            params![
                p.direction_epoch,
                serde_json::to_string(&p.target_languages)?,
                event.session_id.to_string()
            ],
        )?;
        tx.execute("UPDATE capture_segments SET direction_epoch=?1,configured_source_language=NULL,targets_json=?2 WHERE segment_id IN (SELECT id FROM segments WHERE session_id=?3)",params![p.direction_epoch,serde_json::to_string(&p.target_languages)?,event.session_id.to_string()])?;
        tx.execute("UPDATE translation_coverage SET state='stale' WHERE segment_id IN (SELECT id FROM segments WHERE session_id=?1)",[event.session_id.to_string()])?;
        tx.execute("UPDATE translation_attempts SET state='stale',updated_seq=?1 WHERE translation_id IN (SELECT id FROM translations WHERE session_id=?2)",params![receipt.store_seq,event.session_id.to_string()])?;
        let records: Vec<String> = {
            let mut s=tx.prepare("SELECT r.record_json FROM segment_revisions r JOIN segments s ON s.id=r.segment_id AND s.current_revision=r.revision WHERE s.session_id=?1 AND r.status='success'")?;
            let result = s
                .query_map([event.session_id.to_string()], |r| r.get(0))?
                .collect::<std::result::Result<_, _>>()?;
            result
        };
        for body in records {
            let mut record: TranscriptRecord = serde_json::from_str(&body)?;
            record.payload.direction_epoch = p.direction_epoch;
            record.payload.target_languages = p.target_languages.clone();
            insert_coverage(&tx, &record)?;
        }
        save_all_translation_history(&tx, event.session_id, receipt.store_seq)?;
        super::reliable::save_history(&tx, event.session_id, receipt.store_seq)?;
        append_outbox(&tx, event, &receipt, &body)?;
        tx.commit()?;
        Ok(receipt)
    }
}
