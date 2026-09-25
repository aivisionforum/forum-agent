//! Durable analysis workflow. Every mutation and its internal/public change log
//! share a transaction. Model output never directly enters the public projection.
use super::*;
use std::collections::HashSet;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
fn invalid(field: &'static str) -> StoreError {
    ValidationError::Invalid(field).into()
}
fn json<T: Serialize>(x: &T) -> Result<String> {
    Ok(canonical_json(&serde_json::to_value(x)?))
}
fn state(s: AnalysisJobState) -> Result<String> {
    Ok(serde_json::to_value(s)?.as_str().unwrap().to_owned())
}
fn cursor(c: &Connection) -> Result<u64> {
    Ok(c.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM analysis_changes",
        [],
        |r| r.get(0),
    )?)
}
fn load_job(c: &Connection, id: Uuid) -> Result<AnalysisJob> {
    let s: Option<String> = c
        .query_row(
            "SELECT body_json FROM analysis_jobs WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(
        &s.ok_or(StoreError::NotFound("analysis_job"))?,
    )?)
}
fn load_artifact(c: &Connection, id: Uuid) -> Result<ArtifactRecord> {
    let s: Option<String> = c
        .query_row(
            "SELECT body_json FROM analysis_artifacts WHERE id=?1",
            [id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(serde_json::from_str(
        &s.ok_or(StoreError::NotFound("artifact"))?,
    )?)
}
fn load_snapshot(c: &Connection, id: Uuid) -> Result<AnalysisSnapshotData> {
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT body_json,sha256 FROM analysis_snapshots WHERE id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (compact_json, sha256) = row.ok_or(StoreError::NotFound("analysis_snapshot"))?;
    Ok(AnalysisSnapshotData {
        snapshot: serde_json::from_str(&compact_json)?,
        compact_json,
        sha256,
    })
}
fn save_job(c: &Connection, j: &AnalysisJob) -> Result<()> {
    let body = json(j)?;
    c.execute(
        "UPDATE analysis_jobs SET state=?2,body_json=?3 WHERE id=?1",
        params![j.job_id.to_string(), state(j.state)?, body],
    )?;
    c.execute("INSERT INTO analysis_attempts(job_id,attempt,body_json) VALUES(?1,?2,?3) ON CONFLICT(job_id,attempt) DO UPDATE SET body_json=excluded.body_json",params![j.job_id.to_string(),j.attempt,body])?;
    c.execute("INSERT INTO analysis_changes(entity_kind,entity_id,created_at_ms,body_json) VALUES('job',?1,?2,?3)",params![j.job_id.to_string(),j.created_at_ms,body])?;
    Ok(())
}
fn save_artifact(c: &Connection, a: &ArtifactRecord) -> Result<()> {
    let body = json(a)?;
    c.execute("INSERT INTO analysis_artifacts(id,created_at_ms,body_json) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET body_json=excluded.body_json",params![a.artifact_id.to_string(),a.created_at_ms,body])?;
    c.execute("INSERT INTO artifact_revisions(artifact_id,revision,body_json) VALUES(?1,?2,?3) ON CONFLICT(artifact_id,revision) DO UPDATE SET body_json=excluded.body_json",params![a.artifact_id.to_string(),a.revision.get(),body])?;
    c.execute("INSERT INTO analysis_changes(entity_kind,entity_id,created_at_ms,body_json) VALUES('artifact',?1,?2,?3)",params![a.artifact_id.to_string(),a.created_at_ms,body])?;
    Ok(())
}
fn check_attempt(j: &AnalysisJob, attempt: u32) -> Result<()> {
    if j.attempt != attempt {
        return Err(StoreError::LateResult);
    }
    Ok(())
}
fn running(j: &AnalysisJob, attempt: u32) -> Result<()> {
    check_attempt(j, attempt)?;
    if j.state != AnalysisJobState::Running {
        return Err(StoreError::LateResult);
    }
    if now_ms() > j.deadline_at_ms {
        return Err(invalid("analysis_deadline_elapsed"));
    }
    Ok(())
}
fn hashes(config: &AnalysisConfig) -> Result<()> {
    let manifest = config
        .model_manifest_id
        .strip_prefix("sha256:")
        .ok_or_else(|| invalid("model_manifest_prefix"))?;
    if manifest.len() != 64
        || !manifest
            .bytes()
            .all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase())
    {
        return Err(invalid("model_manifest_sha256"));
    }
    for h in [
        &config.profile_sha256,
        &config.prompt_sha256,
        &config.projection_policy_hash,
        &config.effective_config_hash,
    ] {
        if h.len() != 64
            || !h
                .bytes()
                .all(|x| x.is_ascii_hexdigit() && !x.is_ascii_uppercase())
        {
            return Err(invalid("analysis_config_sha256"));
        }
    }
    if config.computed_hash() != config.effective_config_hash {
        return Err(invalid("analysis_effective_config_hash"));
    }
    for s in [
        &config.model_profile,
        &config.prompt_version,
        &config.profile_id,
        &config.profile_version,
    ] {
        if s.trim().is_empty() || s.len() > 256 {
            return Err(invalid("analysis_config_id"));
        }
    }
    Ok(())
}

fn insight_input_cursor(c: &Connection, session: Uuid) -> Result<u64> {
    Ok(c.query_row("SELECT COALESCE(MAX(json_extract(s.body_json,'$.input_cursor')),0) FROM analysis_jobs j JOIN analysis_snapshots s ON s.id=json_extract(j.body_json,'$.snapshot_id') WHERE j.state IN ('succeeded','succeeded_partial') AND json_extract(j.body_json,'$.kind')='insight' AND EXISTS(SELECT 1 FROM json_each(j.body_json,'$.session_ids') WHERE value=?1)", [session.to_string()], |r| r.get(0))?)
}

fn new_insight_start(c: &Connection, session: Uuid, after: u64) -> Result<Option<u64>> {
    Ok(c.query_row("SELECT MIN(json_extract(c.audio_json,'$.start_ms')) FROM segments s JOIN capture_segments c ON c.segment_id=s.id JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.session_id=?1 AND r.created_seq>?2 AND json_extract(r.record_json,'$.payload.status')='success' AND length(trim(json_extract(r.record_json,'$.payload.text')))>0", params![session.to_string(),after], |r|r.get(0))?)
}

fn snapshot_for(
    c: &Connection,
    r: &CreateAnalysisJob,
    selected: Option<&[PublicSelection]>,
) -> Result<AnalysisSnapshotData> {
    let mut ids = r.session_ids.clone();
    ids.sort();
    if ids.is_empty()
        || ids.len() > 100
        || ids.iter().any(Uuid::is_nil)
        || ids.windows(2).any(|p| p[0] == p[1])
    {
        return Err(invalid("analysis_session_selection"));
    }
    if !matches!(
        r.kind,
        AnalysisKind::EventReport | AnalysisKind::ClosingBrief
    ) && ids.len() != 1
    {
        return Err(StoreError::ScopeMismatch);
    }
    let event = super::forum::analysis_session_event(c, ids[0])?;
    for id in &ids {
        if super::forum::analysis_session_event(c, *id)? != event {
            return Err(StoreError::ScopeMismatch);
        }
    }
    let mut s = AnalysisSnapshot {
        schema_version: 1,
        snapshot_id: Uuid::nil(),
        event_id: event,
        session_ids: ids.clone(),
        input_cursor: c.query_row(
            "SELECT COALESCE(MAX(store_seq),0) FROM ingested_events",
            [],
            |r| r.get(0),
        )?,
        kind: r.kind,
        effective_config_hash: r.config.effective_config_hash.clone(),
        segments: vec![],
        published_artifacts: vec![],
        input_complete: true,
    };
    if matches!(
        r.kind,
        AnalysisKind::EventReport | AnalysisKind::ClosingBrief
    ) {
        let mut stmt=c.prepare("SELECT a.body_json,p.body_json FROM analysis_artifacts a JOIN publication_projection p ON a.id=p.artifact_id WHERE p.active=1 ORDER BY a.created_at_ms,a.id")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let mut included = HashSet::new();
        for (a, p) in rows {
            let a: ArtifactRecord = serde_json::from_str(&a)?;
            if a.kind == r.kind
                || !a.session_ids.iter().all(|i| ids.contains(i))
                || a.publication != ArtifactPublication::Published
                || a.review != ArtifactReview::Approved
                || a.validation != ArtifactValidation::Valid
            {
                continue;
            }
            let p: PublicArtifact = serde_json::from_str(&p)?;
            included.extend(a.session_ids.iter().copied());
            s.published_artifacts.push(AnalysisPublishedInput {
                artifact_id: a.artifact_id,
                revision: a.revision,
                session_ids: a.session_ids,
                kind: a.kind,
                title: p.title,
                text: p.text,
            });
        }
        for id in &ids {
            for input in super::forum::analysis_peer_inputs(c, *id)? {
                if input.kind != r.kind {
                    included.insert(*id);
                    s.published_artifacts.push(input);
                }
            }
        }
        if ids.iter().any(|i| !included.contains(i)) {
            return Err(invalid("selected_session_has_no_published_artifact"));
        }
        s.input_cursor = c.query_row(
            "SELECT COALESCE(MAX(publication_seq),0) FROM publication_changes",
            [],
            |r| r.get(0),
        )?;
    } else {
        for id in &ids {
            let st = load_session_status(c, *id)?;
            if r.kind == AnalysisKind::Minutes && (!st.capture_stopped || !st.transcript_sealed) {
                s.input_complete = false
            }
            let gaps: u64 = c.query_row(
                "SELECT COUNT(*) FROM audio_gaps WHERE session_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?;
            if gaps > 0 {
                s.input_complete = false
            }
            let imported: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM legacy_imports WHERE session_id=?1)",
                [id.to_string()],
                |r| r.get(0),
            )?;
            if imported {
                s.input_complete = false;
            }
            let mut stmt=c.prepare("SELECT s.id,c.audio_json,r.record_json FROM segments s JOIN capture_segments c ON c.segment_id=s.id LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.session_id=?1 ORDER BY json_extract(c.audio_json,'$.start_ms'),s.track_id,s.id")?;
            let rows = stmt
                .query_map([id.to_string()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for (seg, a, record) in rows {
                let record: Option<TranscriptRecord> =
                    record.map(|r| serde_json::from_str(&r)).transpose()?;
                let status = record.as_ref().map(|r| r.payload.status);
                if !matches!(
                    status,
                    Some(TranscriptStatus::Success | TranscriptStatus::Empty)
                ) {
                    s.input_complete = false
                }
                s.segments.push(AnalysisSource {
                    session_id: *id,
                    segment_id: parse_uuid(&seg)?,
                    revision: record.as_ref().map(|r| r.payload.revision),
                    text: record
                        .as_ref()
                        .map(|r| r.payload.text.clone())
                        .unwrap_or_default(),
                    status,
                    audio: serde_json::from_str(&a)?,
                    speaker_id: super::forum::analysis_speaker(
                        c,
                        *id,
                        parse_uuid(&seg)?,
                        record.as_ref().and_then(|r| r.payload.speaker_id),
                    )?,
                });
            }
        }
        if r.kind == AnalysisKind::Insight {
            let tail = s.segments.iter().map(|x| x.audio.end_ms).max().unwrap_or(0);
            // The initial/manual pass covers the meeting from its beginning.
            // Subsequent passes add every unconsumed source, with overlap;
            // the live topic map composes all of these source-backed results.
            let mut begin = 0;
            let mut late_sources = HashSet::new();
            if r.automatic {
                // Analyze new committed text plus a short source-only overlap.
                // Earlier insights remain separate evidence-backed artifacts.
                let after = insight_input_cursor(c, ids[0])?;
                let start = new_insight_start(c, ids[0], after)?
                    .ok_or_else(|| invalid("analysis_no_new_input"))?;
                begin = start.saturating_sub(15_000);
                if after > 0 {
                    // Late ASR finals and corrections can precede the rolling
                    // window. Consume those revisions too, without replaying
                    // the entire intervening meeting or repeatedly scheduling
                    // the same unconsumed revision.
                    let mut q = c.prepare("SELECT s.id FROM segments s JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.session_id=?1 AND r.created_seq>?2 AND json_extract(r.record_json,'$.payload.status')='success' AND length(trim(json_extract(r.record_json,'$.payload.text')))>0")?;
                    late_sources = q
                        .query_map(params![ids[0].to_string(), after], |r| {
                            r.get::<_, String>(0)
                        })?
                        .collect::<std::result::Result<HashSet<_>, _>>()?;
                }
            }
            s.segments.retain(|x| {
                x.audio.end_ms > begin || late_sources.contains(&x.segment_id.to_string())
            });
            let gaps:u64=c.query_row("SELECT COUNT(*) FROM audio_gaps WHERE session_id=?1 AND json_extract(payload_json,'$.audio.end_ms')>?2 AND json_extract(payload_json,'$.audio.start_ms')<?3",params![ids[0].to_string(),begin,tail],|r|r.get(0))?;
            let imported: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM legacy_imports WHERE session_id=?1)",
                [ids[0].to_string()],
                |r| r.get(0),
            )?;
            s.input_complete = !imported
                && gaps == 0
                && s.segments.iter().all(|x| {
                    matches!(
                        x.status,
                        Some(TranscriptStatus::Success | TranscriptStatus::Empty)
                    )
                });
            // Freeze only committed ASR revisions. A pending tail is not an
            // evidence dependency: its first final belongs to the next round.
            // Keep input_complete=false so this partial view cannot be published
            // as complete coverage of the meeting.
            s.segments.retain(|x| x.revision.is_some());
        }
        if s.segments.is_empty() {
            return Err(invalid("analysis_empty_input"));
        }
    }
    if !matches!(
        r.kind,
        AnalysisKind::EventReport | AnalysisKind::ClosingBrief
    ) {
        s.input_cursor = 0;
        for source in &s.segments {
            let seq:u64=c.query_row("SELECT MAX(c.created_seq,COALESCE(r.created_seq,0)) FROM capture_segments c JOIN segments s ON s.id=c.segment_id LEFT JOIN segment_revisions r ON r.segment_id=s.id AND r.revision=s.current_revision WHERE s.id=?1",[source.segment_id.to_string()],|r|r.get(0))?;
            s.input_cursor = s.input_cursor.max(seq);
        }
    }
    if let Some(selected) = selected {
        if !matches!(
            r.kind,
            AnalysisKind::EventReport | AnalysisKind::ClosingBrief
        ) || selected.is_empty()
            || selected.len() > 1000
        {
            return Err(invalid("analysis_public_selection"));
        }
        let mut keep = HashSet::new();
        let mut unique = HashSet::new();
        for selection in selected {
            if !ids.contains(&selection.session_id)
                || !unique.insert((
                    selection.owner_device_id,
                    selection.session_id,
                    selection.public_id,
                ))
            {
                return Err(StoreError::ScopeMismatch);
            }
            let alias = super::forum::selection_alias(c, selection)?;
            if !s.published_artifacts.iter().any(|a| {
                a.artifact_id == alias
                    && a.revision == selection.revision
                    && a.session_ids.contains(&selection.session_id)
            }) {
                return Err(StoreError::LateResult);
            }
            keep.insert(alias);
        }
        s.published_artifacts
            .retain(|a| keep.contains(&a.artifact_id));
        if ids.iter().any(|id| {
            !s.published_artifacts
                .iter()
                .any(|a| a.session_ids.contains(id))
        }) {
            return Err(invalid("selected_session_has_no_selected_publication"));
        }
    }
    // A stable UUID from canonical input makes identical snapshots deduplicate.
    let bytes = json(&s)?;
    let h = Sha256::digest(bytes);
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    s.snapshot_id = Uuid::from_bytes(b);
    let compact_json = json(&s)?;
    let sha256 = digest(&compact_json);
    Ok(AnalysisSnapshotData {
        snapshot: s,
        compact_json,
        sha256,
    })
}
fn snapshot_current(c: &Connection, s: &AnalysisSnapshot) -> Result<bool> {
    // A delayed durable gap may arrive after snapshot creation. Its input cannot
    // become "complete" merely because all text ranges were processed.
    if s.input_complete && snapshot_has_gap(c, s)? {
        return Ok(false);
    }

    for input in &s.segments {
        let (_, _, rev) = capture_context(c, input.session_id, input.segment_id)?;
        if rev != input.revision.map(Revision::get) {
            return Ok(false);
        }
        if super::forum::analysis_speaker(c, input.session_id, input.segment_id, input.speaker_id)?
            != input.speaker_id
        {
            return Ok(false);
        }
    }
    for input in &s.published_artifacts {
        if let Some(current) = super::forum::peer_input_current(c, input)? {
            if !current {
                return Ok(false);
            }
            continue;
        }
        let a = load_artifact(c, input.artifact_id)?;
        if a.revision != input.revision
            || a.publication != ArtifactPublication::Published
            || a.review != ArtifactReview::Approved
            || a.validation != ArtifactValidation::Valid
        {
            return Ok(false);
        }
        let p: Option<String> = c
            .query_row(
                "SELECT body_json FROM publication_projection WHERE artifact_id=?1 AND active=1",
                [input.artifact_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let Some(p) = p else { return Ok(false) };
        let p: PublicArtifact = serde_json::from_str(&p)?;
        if p.text != input.text || p.title != input.title {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Store {
    pub fn automatic_insight_ready(&self, session: Uuid) -> Result<bool> {
        require_session(&self.connection, session)?;
        let active: bool = self.connection.query_row("SELECT EXISTS(SELECT 1 FROM analysis_jobs WHERE state IN ('queued','waiting','running','cancel_requested') AND json_extract(body_json,'$.kind')='insight' AND EXISTS(SELECT 1 FROM json_each(body_json,'$.session_ids') WHERE value=?1))",[session.to_string()],|r|r.get(0))?;
        Ok(!active
            && new_insight_start(
                &self.connection,
                session,
                insight_input_cursor(&self.connection, session)?,
            )?
            .is_some())
    }

    /// Refresh the entire private insight history only when an artifact changes.
    /// Revisions, hides and source invalidations replace the cached view too.
    pub fn live_insight_history(
        &self,
        session: Uuid,
        known: u64,
    ) -> Result<(u64, Option<Vec<ArtifactRecord>>)> {
        require_session(&self.connection, session)?;
        let current: u64 = self.connection.query_row("SELECT COALESCE(MAX(seq),0) FROM analysis_changes WHERE entity_kind='artifact' AND json_extract(body_json,'$.kind')='insight' AND EXISTS(SELECT 1 FROM json_each(body_json,'$.session_ids') WHERE value=?1)",[session.to_string()],|r|r.get(0))?;
        if known != 0 && known == current {
            return Ok((current, None));
        }
        let mut q = self.connection.prepare("SELECT body_json FROM analysis_artifacts WHERE json_extract(body_json,'$.kind')='insight' AND EXISTS(SELECT 1 FROM json_each(body_json,'$.session_ids') WHERE value=?1) ORDER BY created_at_ms DESC,id DESC")?;
        let rows = q
            .query_map([session.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok((
            current,
            Some(
                rows.into_iter()
                    .map(|s| Ok(serde_json::from_str(&s)?))
                    .collect::<Result<_>>()?,
            ),
        ))
    }

    pub fn create_analysis_job(&mut self, r: &CreateAnalysisJob) -> Result<AnalysisJob> {
        self.create_analysis_job_selected(r, None)
    }
    pub fn create_selected_analysis_job(
        &mut self,
        r: &CreateAnalysisJob,
        selected: &[PublicSelection],
    ) -> Result<AnalysisJob> {
        self.create_analysis_job_selected(r, Some(selected))
    }
    fn create_analysis_job_selected(
        &mut self,
        r: &CreateAnalysisJob,
        selected: Option<&[PublicSelection]>,
    ) -> Result<AnalysisJob> {
        hashes(&r.config)?;
        if r.request_id.is_nil()
            || r.budget_ms < 1000
            || r.budget_ms > 86_400_000
            || r.max_attempts == 0
            || r.max_attempts > 3
        {
            return Err(invalid("analysis_job_budget"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let request_sha = digest(match selected {
            Some(s) => json(&(r, s))?,
            None => json(r)?,
        });
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT j.body_json,k.request_sha256 FROM analysis_request_keys k JOIN analysis_jobs j ON j.id=k.job_id WHERE k.request_id=?1",
                [r.request_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((body, sha)) = existing {
            if sha != request_sha {
                return Err(StoreError::EventIdConflict);
            }
            return Ok(serde_json::from_str(&body)?);
        }
        let snapshot = snapshot_for(&tx, r, selected)?;
        if r.automatic {
            let existing:Option<String>=tx.query_row("SELECT body_json FROM analysis_jobs WHERE json_extract(body_json,'$.automatic')=1 AND json_extract(body_json,'$.snapshot_sha256')=?1 ORDER BY created_at_ms,id LIMIT 1",[&snapshot.sha256],|row|row.get(0)).optional()?;
            if let Some(body) = existing {
                let job: AnalysisJob = serde_json::from_str(&body)?;
                tx.execute("INSERT INTO analysis_request_keys(request_id,request_sha256,job_id) VALUES(?1,?2,?3)",params![r.request_id.to_string(),request_sha,job.job_id.to_string()])?;
                tx.commit()?;
                return Ok(job);
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO analysis_snapshots(id,sha256,body_json) VALUES(?1,?2,?3)",
            params![
                snapshot.snapshot.snapshot_id.to_string(),
                snapshot.sha256,
                snapshot.compact_json
            ],
        )?;
        super::forum::save_peer_provenance(&tx, &snapshot.snapshot)?;
        if r.automatic && r.kind == AnalysisKind::Insight {
            let mut stmt = tx.prepare(
                "SELECT body_json FROM analysis_jobs WHERE state IN ('queued','waiting')",
            )?;
            let old = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            for body in old {
                let mut old: AnalysisJob = serde_json::from_str(&body)?;
                if old.automatic
                    && old.kind == r.kind
                    && old.session_ids == snapshot.snapshot.session_ids
                {
                    old.state = AnalysisJobState::Cancelled;
                    old.error = Some("coalesced_by_newer_snapshot".into());
                    save_job(&tx, &old)?;
                }
            }
        }
        let now = now_ms();
        let j = AnalysisJob {
            job_id: Uuid::new_v4(),
            request_id: r.request_id,
            kind: r.kind,
            event_id: snapshot.snapshot.event_id,
            session_ids: snapshot.snapshot.session_ids,
            state: AnalysisJobState::Queued,
            attempt: 1,
            max_attempts: r.max_attempts,
            budget_ms: r.budget_ms,
            created_at_ms: now,
            deadline_at_ms: now.saturating_add(r.budget_ms),
            started_at_ms: None,
            snapshot_id: snapshot.snapshot.snapshot_id,
            snapshot_sha256: snapshot.sha256,
            config: r.config.clone(),
            automatic: r.automatic,
            progress: AnalysisProgress {
                phase: "queued".into(),
                completed_units: 0,
                total_units: 0,
                wait_reason: None,
            },
            error: None,
            result: None,
        };
        tx.execute("INSERT INTO analysis_jobs(id,request_id,request_sha256,state,created_at_ms,body_json) VALUES(?1,?2,?3,'queued',?4,?5)",params![j.job_id.to_string(),j.request_id.to_string(),request_sha,j.created_at_ms,json(&j)?])?;
        tx.execute(
            "INSERT INTO analysis_request_keys(request_id,request_sha256,job_id) VALUES(?1,?2,?3)",
            params![r.request_id.to_string(), request_sha, j.job_id.to_string()],
        )?;
        save_job(&tx, &j)?;
        tx.commit()?;
        Ok(j)
    }
    pub fn analysis_job(&self, id: Uuid) -> Result<AnalysisJob> {
        load_job(&self.connection, id)
    }
    pub fn analysis_snapshot(&self, job: Uuid) -> Result<AnalysisSnapshotData> {
        let j = load_job(&self.connection, job)?;
        load_snapshot(&self.connection, j.snapshot_id)
    }
    pub fn next_analysis_jobs(&self, limit: u32) -> Result<Vec<AnalysisJob>> {
        if limit == 0 || limit > 100 {
            return Err(invalid("analysis_limit"));
        }
        let mut s=self.connection.prepare("SELECT body_json FROM analysis_jobs WHERE state IN ('queued','waiting') ORDER BY json_extract(body_json,'$.automatic'),created_at_ms,id LIMIT ?1")?;
        let rows = s
            .query_map([limit], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_str(&b)?))
            .collect()
    }
    fn change_job(
        &mut self,
        id: Uuid,
        attempt: u32,
        f: impl FnOnce(&Connection, &mut AnalysisJob) -> Result<()>,
    ) -> Result<AnalysisJob> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut j = load_job(&tx, id)?;
        check_attempt(&j, attempt)?;
        f(&tx, &mut j)?;
        save_job(&tx, &j)?;
        tx.commit()?;
        Ok(j)
    }
    pub fn claim_analysis_job(&mut self, id: Uuid, attempt: u32) -> Result<AnalysisJob> {
        self.change_job(id,attempt,|c,j|{if !matches!(j.state,AnalysisJobState::Queued|AnalysisJobState::Waiting){return Err(StoreError::InvalidState)}if now_ms()>=j.deadline_at_ms{j.state=AnalysisJobState::Failed;j.error=Some("budget_exhausted_in_queue".into());return Ok(())}let active:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM analysis_jobs WHERE state IN ('running','cancel_requested'))",[],|r|r.get(0))?;if active{return Err(StoreError::QueueFull)}if !snapshot_current(c,&load_snapshot(c,j.snapshot_id)?.snapshot)?{j.state=AnalysisJobState::Failed;j.error=Some("snapshot_stale".into());return Ok(())}j.state=AnalysisJobState::Running;j.started_at_ms=Some(now_ms());j.progress.phase="running".into();j.progress.wait_reason=None;Ok(())})
    }
    pub fn wait_analysis_job(
        &mut self,
        id: Uuid,
        attempt: u32,
        reason: String,
    ) -> Result<AnalysisJob> {
        self.change_job(id, attempt, |_, j| {
            if !matches!(
                j.state,
                AnalysisJobState::Queued | AnalysisJobState::Waiting
            ) {
                return Err(StoreError::InvalidState);
            }
            j.state = AnalysisJobState::Waiting;
            j.progress.phase = "waiting".into();
            j.progress.wait_reason = Some(reason);
            Ok(())
        })
    }
    pub fn update_analysis_progress(
        &mut self,
        id: Uuid,
        attempt: u32,
        p: AnalysisProgress,
    ) -> Result<AnalysisJob> {
        self.change_job(id, attempt, |_, j| {
            running(j, attempt)?;
            if p.completed_units > p.total_units
                || p.phase.len() > 256
                || p.wait_reason.as_ref().is_some_and(|s| s.len() > 4096)
            {
                return Err(invalid("analysis_progress"));
            }
            j.progress = p;
            Ok(())
        })
    }
    pub fn cancel_analysis_job(&mut self, id: Uuid, expected_attempt: u32) -> Result<AnalysisJob> {
        self.change_job(id, expected_attempt, |_, j| {
            j.state = match j.state {
                AnalysisJobState::Queued | AnalysisJobState::Waiting => AnalysisJobState::Cancelled,
                AnalysisJobState::Running | AnalysisJobState::CancelRequested => {
                    AnalysisJobState::CancelRequested
                }
                AnalysisJobState::Cancelled => AnalysisJobState::Cancelled,
                _ => return Err(StoreError::InvalidState),
            };
            Ok(())
        })
    }
    /// Host calls only after its owned worker group has actually exited.
    pub fn complete_analysis_cancel(&mut self, id: Uuid, attempt: u32) -> Result<AnalysisJob> {
        self.change_job(id, attempt, |_, j| {
            if !matches!(
                j.state,
                AnalysisJobState::CancelRequested | AnalysisJobState::Cancelled
            ) {
                return Err(StoreError::InvalidState);
            }
            j.state = AnalysisJobState::Cancelled;
            Ok(())
        })
    }
    pub fn fail_analysis_job(
        &mut self,
        id: Uuid,
        attempt: u32,
        error: String,
    ) -> Result<AnalysisJob> {
        self.change_job(id, attempt, |_, j| {
            if !matches!(
                j.state,
                AnalysisJobState::Running | AnalysisJobState::Queued | AnalysisJobState::Waiting
            ) {
                return Err(StoreError::InvalidState);
            }
            j.state = AnalysisJobState::Failed;
            j.error = Some(error.chars().take(8192).collect());
            Ok(())
        })
    }
    pub fn retry_analysis_job(&mut self, id: Uuid, expected_attempt: u32) -> Result<AnalysisJob> {
        self.change_job(id, expected_attempt, |c, j| {
            if !matches!(
                j.state,
                AnalysisJobState::Failed
                    | AnalysisJobState::Interrupted
                    | AnalysisJobState::SucceededPartial
                    | AnalysisJobState::Cancelled
            ) || j.attempt >= j.max_attempts
            {
                return Err(StoreError::InvalidState);
            }
            if !snapshot_current(c, &load_snapshot(c, j.snapshot_id)?.snapshot)? {
                return Err(StoreError::LateResult);
            }
            j.attempt += 1;
            j.state = AnalysisJobState::Queued;
            j.started_at_ms = None;
            j.deadline_at_ms = now_ms().saturating_add(j.budget_ms);
            j.error = None;
            j.progress = AnalysisProgress {
                phase: "queued".into(),
                completed_units: 0,
                total_units: 0,
                wait_reason: None,
            };
            Ok(())
        })
    }
    pub(crate) fn recover_analysis_jobs(&mut self) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut s = tx.prepare(
                "SELECT body_json FROM analysis_jobs WHERE state IN ('running','cancel_requested')",
            )?;
            let v = s
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            v
        };
        for b in rows {
            let mut j: AnalysisJob = serde_json::from_str(&b)?;
            j.state = if j.state == AnalysisJobState::CancelRequested {
                AnalysisJobState::Cancelled
            } else {
                AnalysisJobState::Interrupted
            };
            j.error = Some("host_restarted".into());
            save_job(&tx, &j)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn confirm_analysis_checkpoint(&mut self, p: &AnalysisCheckpoint) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let j = load_job(&tx, p.job_id)?;
        running(&j, p.attempt)?;
        if p.snapshot_sha256 != j.snapshot_sha256
            || p.effective_config_hash != j.config.effective_config_hash
            || p.input_sha256.len() != 64
            || !p
                .input_sha256
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || p.result_sha256 != digest(canonical_json(&p.result))
        {
            return Err(invalid("analysis_checkpoint_hash"));
        }
        if json(p)?.len() > 900_000 {
            return Err(invalid("analysis_checkpoint_size"));
        }
        let prior:Option<String>=tx.query_row("SELECT body_json FROM analysis_checkpoints WHERE job_id=?1 AND step_index=?2 AND input_sha256=?3 AND effective_config_hash=?4",params![p.job_id.to_string(),p.step_index,p.input_sha256,p.effective_config_hash],|r|r.get(0)).optional()?;
        if let Some(prior) = prior {
            let prior: AnalysisCheckpoint = serde_json::from_str(&prior)?;
            if prior.result_sha256 != p.result_sha256 || prior.snapshot_sha256 != p.snapshot_sha256
            {
                return Err(StoreError::EventIdConflict);
            }
            return Ok(());
        }
        tx.execute("INSERT INTO analysis_checkpoints(job_id,step_index,input_sha256,effective_config_hash,body_json) VALUES(?1,?2,?3,?4,?5)",params![p.job_id.to_string(),p.step_index,p.input_sha256,p.effective_config_hash,json(p)?])?;
        tx.commit()?;
        Ok(())
    }
    pub fn analysis_checkpoints(&self, id: Uuid) -> Result<Vec<AnalysisCheckpoint>> {
        load_job(&self.connection, id)?;
        let mut s = self.connection.prepare(
            "SELECT body_json FROM analysis_checkpoints WHERE job_id=?1 ORDER BY step_index",
        )?;
        let rows = s
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_str(&b)?))
            .collect()
    }
}

fn validate_evidence(s: &AnalysisSnapshot, e: &AnalysisEvidence) -> Result<()> {
    match e {
        AnalysisEvidence::Source { session_id, span } => {
            let input = s
                .segments
                .iter()
                .find(|v| {
                    v.session_id == *session_id
                        && v.segment_id == span.segment_id
                        && v.revision == Some(span.segment_revision)
                })
                .ok_or(StoreError::ScopeMismatch)?;
            if input.status != Some(TranscriptStatus::Success) {
                return Err(invalid("evidence_source_not_success"));
            }
            span.validate_against(&input.text)?;
        }
        AnalysisEvidence::Artifact {
            artifact_id,
            revision,
            start_utf8,
            end_utf8,
            quote,
        } => {
            let input = s
                .published_artifacts
                .iter()
                .find(|v| v.artifact_id == *artifact_id && v.revision == *revision)
                .ok_or(StoreError::ScopeMismatch)?;
            if start_utf8 >= end_utf8
                || input.text.get(*start_utf8..*end_utf8) != Some(quote.as_str())
            {
                return Err(ValidationError::InvalidSpan.into());
            }
        }
    }
    Ok(())
}
fn validate_content(s: &AnalysisSnapshot, content: &ArtifactContent) -> Result<ArtifactValidation> {
    if content.title.trim().is_empty() || content.title.len() > 2048 || content.sections.is_empty()
    {
        return Err(invalid("artifact_content"));
    }
    let mut claims = HashSet::new();
    let mut topic_count = 0;
    let mut validation = ArtifactValidation::Valid;
    for sec in &content.sections {
        if sec.heading.len() > 2048 {
            return Err(invalid("artifact_heading"));
        }
        if sec.topics.len() > 5 {
            return Err(invalid("artifact_topics"));
        }
        for topic in &sec.topics {
            if topic.label.trim().is_empty() || topic.label.chars().count() > 12
                || topic.evidence.is_empty() || topic.evidence.len() > 2
            {
                return Err(invalid("artifact_topic"));
            }
            for evidence in &topic.evidence {
                validate_evidence(s, evidence)?;
            }
            topic_count += 1;
        }
        for claim in &sec.claims {
            if claim.claim_id.is_nil()
                || !claims.insert(claim.claim_id)
                || claim.text.trim().is_empty()
            {
                return Err(invalid("artifact_claim"));
            }
            match claim.grounding {
                GroundingStatus::Cited => {
                    if claim.evidence.is_empty() {
                        return Err(invalid("cited_claim_requires_evidence"));
                    }
                }
                GroundingStatus::Unsupported => {
                    validation = ArtifactValidation::NeedsReview;
                }
            }
            for e in &claim.evidence {
                validate_evidence(s, e)?;
            }
        }
    }
    if claims.is_empty() && topic_count == 0 {
        return Err(invalid("artifact_claims_empty"));
    }
    Ok(validation)
}
fn coverage_complete(s: &AnalysisSnapshot, c: &AnalysisCoverage) -> Result<bool> {
    let expected: Vec<(CoverageTarget, &str, bool, bool)> = s
        .segments
        .iter()
        .map(|i| {
            (
                CoverageTarget::Source {
                    segment_id: i.segment_id,
                    segment_revision: i.revision,
                },
                i.text.as_str(),
                i.status == Some(TranscriptStatus::Success),
                i.status == Some(TranscriptStatus::Empty),
            )
        })
        .chain(s.published_artifacts.iter().map(|i| {
            (
                CoverageTarget::Artifact {
                    artifact_id: i.artifact_id,
                    revision: i.revision,
                },
                i.text.as_str(),
                true,
                false,
            )
        }))
        .collect();
    for u in &c.units {
        let (_, text, success, empty) = expected
            .iter()
            .find(|(t, ..)| t == &u.target)
            .ok_or(StoreError::ScopeMismatch)?;
        if u.start_utf8 > u.end_utf8 || text.get(u.start_utf8..u.end_utf8).is_none() {
            return Err(ValidationError::InvalidSpan.into());
        }
        match u.status {
            CoverageStatus::Processed if !success || u.start_utf8 == u.end_utf8 => {
                return Err(invalid("coverage_processed_invalid"))
            }
            CoverageStatus::IgnoredEmpty if !empty || u.start_utf8 != 0 || u.end_utf8 != 0 => {
                return Err(invalid("coverage_empty_invalid"))
            }
            CoverageStatus::Failed if u.reason.as_ref().is_none_or(|r| r.trim().is_empty()) => {
                return Err(invalid("coverage_failure_reason"))
            }
            _ => {}
        }
    }
    let mut complete = s.input_complete;
    for (target, text, success, empty) in expected {
        let mut units = c
            .units
            .iter()
            .filter(|u| u.target == target)
            .collect::<Vec<_>>();
        units.sort_by_key(|u| (u.start_utf8, u.end_utf8));
        if units.is_empty() {
            complete = false;
            continue;
        }
        if text.is_empty() {
            if units.len() != 1 {
                return Err(invalid("coverage_duplicate_empty"));
            }
            if !empty || units[0].status != CoverageStatus::IgnoredEmpty {
                complete = false
            }
            continue;
        }
        let mut end = 0;
        for u in units {
            if u.start_utf8 < end {
                return Err(invalid("coverage_overlap"));
            }
            if u.start_utf8 != end || u.status != CoverageStatus::Processed {
                complete = false
            }
            end = u.end_utf8;
        }
        if end != text.len() || !success {
            complete = false
        }
    }
    Ok(complete)
}
fn deps(c: &Connection, a: &ArtifactRecord, s: &AnalysisSnapshot) -> Result<()> {
    c.execute(
        "DELETE FROM artifact_dependencies WHERE artifact_id=?1",
        [a.artifact_id.to_string()],
    )?;
    for input in &s.segments {
        c.execute("INSERT INTO artifact_dependencies(artifact_id,dependency_kind,dependency_id,dependency_revision) VALUES(?1,'source',?2,?3)",params![a.artifact_id.to_string(),input.segment_id.to_string(),input.revision.map(Revision::get)])?;
    }
    for input in &s.published_artifacts {
        c.execute("INSERT INTO artifact_dependencies(artifact_id,dependency_kind,dependency_id,dependency_revision) VALUES(?1,'artifact',?2,?3)",params![a.artifact_id.to_string(),input.artifact_id.to_string(),input.revision.get()])?;
    }
    Ok(())
}
fn expected_revision(a: &ArtifactRecord, r: Revision) -> Result<()> {
    if a.revision != r {
        return Err(StoreError::RevisionConflict {
            expected: r.get(),
            actual: Some(a.revision.get()),
        });
    }
    Ok(())
}
fn operator(operator: &str, reason: &str) -> Result<()> {
    if operator.trim().is_empty()
        || operator.len() > 256
        || reason.trim().is_empty()
        || reason.len() > 4096
    {
        return Err(invalid("review_operator_reason"));
    }
    Ok(())
}
fn tombstone(c: &Connection, id: Uuid) -> Result<()> {
    let row: Option<(String, bool)> = c
        .query_row(
            "SELECT public_id,active FROM publication_projection WHERE artifact_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((public, true)) = row {
        c.execute(
            "INSERT INTO publication_changes(artifact_id,public_id,body_json) VALUES(?1,?2,'{}')",
            params![id.to_string(), public],
        )?;
        let change = PublicationChange {
            publication_seq: c.last_insert_rowid() as u64,
            public_id: parse_uuid(&public)?,
            withdrawn: true,
            artifact: None,
        };
        c.execute(
            "UPDATE publication_changes SET body_json=?2 WHERE publication_seq=?1",
            params![change.publication_seq, json(&change)?],
        )?;
        c.execute(
            "UPDATE publication_projection SET active=0 WHERE artifact_id=?1",
            [id.to_string()],
        )?;
    }
    Ok(())
}
fn invalidate_artifacts(c: &Connection, initial: Vec<Uuid>) -> Result<()> {
    let mut pending = initial;
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let mut a = load_artifact(c, id)?;
        if a.validation != ArtifactValidation::Stale {
            a.validation = ArtifactValidation::Stale;
            a.reason = Some("source_or_published_dependency_changed".into());
            if a.publication == ArtifactPublication::Published {
                a.publication = ArtifactPublication::Withdrawn
            }
            tombstone(c, id)?;
            save_artifact(c, &a)?;
        }
        let mut q=c.prepare("SELECT artifact_id FROM artifact_dependencies WHERE dependency_kind='artifact' AND dependency_id=?1")?;
        let ids = q
            .query_map([id.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for id in ids {
            pending.push(parse_uuid(&id)?);
        }
    }
    Ok(())
}
pub(super) fn invalidate_dependents(c: &Connection, id: Uuid) -> Result<()> {
    let mut q=c.prepare("SELECT artifact_id FROM artifact_dependencies WHERE dependency_kind='artifact' AND dependency_id=?1")?;
    let ids = q
        .query_map([id.to_string()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    invalidate_artifacts(
        c,
        ids.into_iter()
            .map(|id| parse_uuid(&id))
            .collect::<Result<_>>()?,
    )
}
pub(crate) fn invalidate_analysis_source(c: &Connection, segment: Uuid) -> Result<()> {
    let mut q=c.prepare("SELECT artifact_id FROM artifact_dependencies WHERE dependency_kind='source' AND dependency_id=?1")?;
    let ids = q
        .query_map([segment.to_string()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    invalidate_artifacts(
        c,
        ids.into_iter()
            .map(|id| parse_uuid(&id))
            .collect::<Result<_>>()?,
    )
}

impl Store {
    pub fn interrupt_analysis_job(
        &mut self,
        id: Uuid,
        attempt: u32,
        reason: String,
    ) -> Result<AnalysisJob> {
        self.change_job(id, attempt, |_, j| {
            if !matches!(
                j.state,
                AnalysisJobState::Running | AnalysisJobState::CancelRequested
            ) {
                return Err(StoreError::InvalidState);
            }
            j.state = if j.state == AnalysisJobState::CancelRequested {
                AnalysisJobState::Cancelled
            } else {
                AnalysisJobState::Interrupted
            };
            j.error = Some(reason);
            Ok(())
        })
    }
    pub fn interrupt_running_analysis_jobs(&mut self, reason: String) -> Result<Vec<AnalysisJob>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let rows = {
            let mut q = tx.prepare(
                "SELECT body_json FROM analysis_jobs WHERE state IN ('running','cancel_requested')",
            )?;
            let r = q
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            r
        };
        let mut out = vec![];
        for row in rows {
            let mut j: AnalysisJob = serde_json::from_str(&row)?;
            j.state = if j.state == AnalysisJobState::CancelRequested {
                AnalysisJobState::Cancelled
            } else {
                AnalysisJobState::Interrupted
            };
            j.error = Some(reason.clone());
            save_job(&tx, &j)?;
            out.push(j)
        }
        tx.commit()?;
        Ok(out)
    }
    pub fn finish_analysis_job(&mut self, result: &AnalysisResult) -> Result<ArtifactRecord> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut j = load_job(&tx, result.job_id)?;
        let result_sha = digest(json(result)?);
        let prior:Option<(String,String,u32)>=tx.query_row("SELECT result_sha256,artifact_id,revision FROM analysis_results WHERE job_id=?1 AND attempt=?2",params![result.job_id.to_string(),result.attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((sha, id, revision)) = prior {
            if sha != result_sha {
                return Err(StoreError::EventIdConflict);
            }
            let body: String = tx.query_row(
                "SELECT body_json FROM artifact_revisions WHERE artifact_id=?1 AND revision=?2",
                params![id, revision],
                |r| r.get(0),
            )?;
            return Ok(serde_json::from_str(&body)?);
        }
        running(&j, result.attempt)?;
        if result.schema_version != 1
            || result.snapshot_id != j.snapshot_id
            || result.snapshot_sha256 != j.snapshot_sha256
            || result.effective_config_hash != j.config.effective_config_hash
        {
            return Err(StoreError::LateResult);
        }
        let s = load_snapshot(&tx, j.snapshot_id)?.snapshot;
        if !snapshot_current(&tx, &s)? {
            return Err(StoreError::LateResult);
        }
        let validation = validate_content(&s, &result.content)?;
        let complete = coverage_complete(&s, &result.coverage)?;
        let a = ArtifactRecord {
            artifact_id: Uuid::new_v4(),
            revision: Revision::FIRST,
            kind: j.kind,
            event_id: j.event_id,
            session_ids: j.session_ids.clone(),
            job_id: j.job_id,
            snapshot_id: j.snapshot_id,
            content: result.content.clone(),
            coverage: result.coverage.clone(),
            coverage_complete: complete,
            validation,
            review: ArtifactReview::Draft,
            publication: ArtifactPublication::Private,
            config: j.config.clone(),
            created_at_ms: now_ms(),
            operator_id: None,
            reason: None,
        };
        save_artifact(&tx, &a)?;
        deps(&tx, &a, &s)?;
        tx.execute("INSERT INTO analysis_results(job_id,attempt,result_sha256,artifact_id,revision) VALUES(?1,?2,?3,?4,?5)",params![j.job_id.to_string(),j.attempt,result_sha,a.artifact_id.to_string(),a.revision.get()])?;
        j.state = if complete {
            AnalysisJobState::Succeeded
        } else {
            AnalysisJobState::SucceededPartial
        };
        j.result = Some(ArtifactRef {
            artifact_id: a.artifact_id,
            revision: a.revision,
        });
        // A worker may commit its last checkpoint without sending a final
        // progress notification. Only verified complete coverage closes the
        // progress counters; partial output keeps its reported work progress.
        if complete {
            if j.progress.total_units == 0 {
                j.progress.total_units = u32::try_from(result.coverage.units.len())
                    .map_err(|_| invalid("analysis_progress_units"))?
                    .max(1);
            }
            j.progress.completed_units = j.progress.total_units;
        }
        j.progress.phase = "finished".into();
        j.progress.wait_reason = None;
        save_job(&tx, &j)?;
        tx.commit()?;
        Ok(a)
    }
    pub fn artifact(&self, id: Uuid) -> Result<ArtifactRecord> {
        load_artifact(&self.connection, id)
    }
    pub fn artifact_revision(&self, id: Uuid, revision: Revision) -> Result<ArtifactRecord> {
        let b: Option<String> = self
            .connection
            .query_row(
                "SELECT body_json FROM artifact_revisions WHERE artifact_id=?1 AND revision=?2",
                params![id.to_string(), revision.get()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(serde_json::from_str(
            &b.ok_or(StoreError::NotFound("artifact_revision"))?,
        )?)
    }
    pub fn edit_artifact(&mut self, cmd: &ArtifactEdit) -> Result<ArtifactRecord> {
        operator(&cmd.operator_id, &cmd.reason)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut a = load_artifact(&tx, cmd.artifact_id)?;
        expected_revision(&a, cmd.expected_revision)?;
        let s = load_snapshot(&tx, a.snapshot_id)?.snapshot;
        if !snapshot_current(&tx, &s)? {
            return Err(StoreError::LateResult);
        }
        let validation = validate_content(&s, &cmd.content)?;
        tombstone(&tx, a.artifact_id)?;
        invalidate_dependents(&tx, a.artifact_id)?;
        a.revision = a.revision.next()?;
        a.content = cmd.content.clone();
        a.validation = validation;
        a.review = ArtifactReview::Draft;
        a.publication = ArtifactPublication::Private;
        a.operator_id = Some(cmd.operator_id.clone());
        a.reason = Some(cmd.reason.clone());
        save_artifact(&tx, &a)?;
        tx.commit()?;
        Ok(a)
    }
    pub fn review_artifact(&mut self, cmd: &ArtifactReviewCommand) -> Result<ArtifactRecord> {
        operator(&cmd.operator_id, &cmd.reason)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut a = load_artifact(&tx, cmd.artifact_id)?;
        expected_revision(&a, cmd.expected_revision)?;
        if a.validation == ArtifactValidation::Stale
            || !snapshot_current(&tx, &load_snapshot(&tx, a.snapshot_id)?.snapshot)?
        {
            return Err(StoreError::LateResult);
        }
        if cmd.review == ArtifactReview::Approved && a.validation != ArtifactValidation::Valid {
            return Err(invalid("artifact_not_validated"));
        }
        a.review = cmd.review;
        a.operator_id = Some(cmd.operator_id.clone());
        a.reason = Some(cmd.reason.clone());
        if a.review != ArtifactReview::Approved && a.publication == ArtifactPublication::Published {
            a.publication = ArtifactPublication::Withdrawn;
            tombstone(&tx, a.artifact_id)?;
            invalidate_dependents(&tx, a.artifact_id)?;
        }
        save_artifact(&tx, &a)?;
        tx.commit()?;
        Ok(a)
    }
    pub fn publish_artifact(&mut self, cmd: &ArtifactPublishCommand) -> Result<PublicArtifact> {
        operator(&cmd.operator_id, &cmd.reason)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut a = load_artifact(&tx, cmd.artifact_id)?;
        expected_revision(&a, cmd.expected_revision)?;
        if a.validation != ArtifactValidation::Valid
            || a.review != ArtifactReview::Approved
            || !a.coverage_complete
            || cmd.policy_hash != a.config.projection_policy_hash
            || matches!(
                a.kind,
                AnalysisKind::SuggestedQuestions | AnalysisKind::RedactionReview
            )
        {
            return Err(invalid("artifact_publication_policy"));
        }
        let s = load_snapshot(&tx, a.snapshot_id)?.snapshot;
        if !snapshot_current(&tx, &s)? {
            return Err(StoreError::LateResult);
        }
        if cmd.reviewed_title.trim().is_empty()
            || cmd.reviewed_text.trim().is_empty()
            || cmd.evidence.is_empty()
        {
            return Err(invalid("reviewed_public_projection_empty"));
        }
        let citations: Vec<&AnalysisEvidence> = a
            .content
            .sections
            .iter()
            .flat_map(|s| s.claims.iter())
            .flat_map(|c| c.evidence.iter())
            .collect();
        for e in &cmd.evidence {
            if !citations.contains(&&e.evidence) || e.reviewed_text.trim().is_empty() {
                return Err(invalid("public_evidence_not_reviewed_claim"));
            }
            validate_evidence(&s, &e.evidence)?;
        }
        let command_sha = digest(json(cmd)?);
        let prior:Option<String>=tx.query_row("SELECT body_json FROM publication_projection WHERE artifact_id=?1 AND active=1 AND command_sha256=?2",params![a.artifact_id.to_string(),command_sha],|r|r.get(0)).optional()?;
        if let Some(body) = prior {
            return Ok(serde_json::from_str(&body)?);
        }
        let previous: Option<String> = tx
            .query_row(
                "SELECT body_json FROM publication_projection WHERE artifact_id=?1",
                [a.artifact_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            let previous: PublicArtifact = serde_json::from_str(&previous)?;
            if previous.revision == a.revision
                && (previous.title != cmd.reviewed_title
                    || previous.text != cmd.reviewed_text
                    || previous
                        .evidence
                        .iter()
                        .map(|e| e.text.as_str())
                        .collect::<Vec<_>>()
                        != cmd
                            .evidence
                            .iter()
                            .map(|e| e.reviewed_text.as_str())
                            .collect::<Vec<_>>())
            {
                return Err(invalid(
                    "public_projection_change_requires_artifact_revision",
                ));
            }
        }
        // Re-activation never makes earlier dependency invalidations disappear.
        invalidate_dependents(&tx, a.artifact_id)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT public_id FROM publication_projection WHERE artifact_id=?1",
                [a.artifact_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let public_id = old
            .map(|id| parse_uuid(&id))
            .transpose()?
            .unwrap_or_else(Uuid::new_v4);
        tx.execute(
            "INSERT INTO publication_changes(artifact_id,public_id,body_json) VALUES(?1,?2,'{}')",
            params![a.artifact_id.to_string(), public_id.to_string()],
        )?;
        let seq = tx.last_insert_rowid() as u64;
        tx.execute(
            "INSERT INTO publication_reviews(publication_seq,command_json) VALUES(?1,?2)",
            params![seq, json(cmd)?],
        )?;
        let public = PublicArtifact {
            public_id,
            revision: a.revision,
            kind: a.kind,
            title: cmd.reviewed_title.clone(),
            text: cmd.reviewed_text.clone(),
            evidence: cmd
                .evidence
                .iter()
                .map(|e| PublicEvidence {
                    public_evidence_id: Uuid::new_v4(),
                    revision: Revision::FIRST,
                    text: e.reviewed_text.clone(),
                })
                .collect(),
            publication_seq: seq,
        };
        tx.execute("INSERT INTO publication_projection(artifact_id,public_id,active,body_json) VALUES(?1,?2,1,?3) ON CONFLICT(artifact_id) DO UPDATE SET active=1,body_json=excluded.body_json",params![a.artifact_id.to_string(),public_id.to_string(),json(&public)?])?;
        tx.execute(
            "UPDATE publication_projection SET command_sha256=?2 WHERE artifact_id=?1",
            params![a.artifact_id.to_string(), command_sha],
        )?;
        let change = PublicationChange {
            publication_seq: seq,
            public_id,
            withdrawn: false,
            artifact: Some(public.clone()),
        };
        tx.execute(
            "UPDATE publication_changes SET body_json=?2 WHERE publication_seq=?1",
            params![seq, json(&change)?],
        )?;
        a.publication = ArtifactPublication::Published;
        a.operator_id = Some(cmd.operator_id.clone());
        a.reason = Some(cmd.reason.clone());
        save_artifact(&tx, &a)?;
        tx.commit()?;
        Ok(public)
    }
    pub fn set_artifact_visibility(
        &mut self,
        cmd: &ArtifactVisibilityCommand,
    ) -> Result<ArtifactRecord> {
        operator(&cmd.operator_id, &cmd.reason)?;
        if !matches!(
            cmd.publication,
            ArtifactPublication::Hidden
                | ArtifactPublication::Withdrawn
                | ArtifactPublication::Private
        ) {
            return Err(invalid("publish_requires_reviewed_projection"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut a = load_artifact(&tx, cmd.artifact_id)?;
        expected_revision(&a, cmd.expected_revision)?;
        tombstone(&tx, a.artifact_id)?;
        invalidate_dependents(&tx, a.artifact_id)?;
        a.publication = cmd.publication;
        a.operator_id = Some(cmd.operator_id.clone());
        a.reason = Some(cmd.reason.clone());
        save_artifact(&tx, &a)?;
        tx.commit()?;
        Ok(a)
    }
}

fn page_records<T: serde::de::DeserializeOwned>(
    c: &Connection,
    kind: &str,
    session: Uuid,
    after: Option<AnalysisPageKey>,
    limit: u32,
) -> Result<(u64, Vec<T>, Option<AnalysisPageKey>)> {
    super::forum::analysis_session_event(c, session)?;
    if limit == 0 || limit > 100 {
        return Err(invalid("analysis_page_limit"));
    }
    let latest = cursor(c)?;
    let cur = after.as_ref().map(|v| v.cursor).unwrap_or(latest);
    if cur > latest {
        return Err(StoreError::InvalidCursor);
    }
    let (time, id) = after
        .as_ref()
        .map(|k| (k.created_at_ms, k.id.to_string()))
        .unwrap_or((i64::MAX as u64, "~".into()));
    let mut stmt=c.prepare("SELECT h.body_json,h.created_at_ms,h.entity_id FROM analysis_changes h WHERE h.entity_kind=?1 AND h.seq=(SELECT MAX(x.seq) FROM analysis_changes x WHERE x.entity_kind=h.entity_kind AND x.entity_id=h.entity_id AND x.seq<=?2) AND EXISTS(SELECT 1 FROM json_each(h.body_json,'$.session_ids') WHERE value=?3) AND (h.created_at_ms<?4 OR (h.created_at_ms=?4 AND h.entity_id<?5)) ORDER BY h.created_at_ms DESC,h.entity_id DESC LIMIT ?6")?;
    let mut rows = stmt
        .query_map(
            params![kind, cur, session.to_string(), time, id, limit + 1],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let more = rows.len() > limit as usize;
    if more {
        rows.pop();
    }
    let next = if more {
        rows.last()
            .map(|(_, t, id)| {
                Ok::<_, StoreError>(AnalysisPageKey {
                    cursor: cur,
                    created_at_ms: *t,
                    id: parse_uuid(id)?,
                })
            })
            .transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|(b, ..)| Ok(serde_json::from_str(&b)?))
        .collect::<Result<Vec<_>>>()?;
    Ok((cur, items, next))
}
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn content_markdown(a: &ArtifactRecord) -> String {
    let mut out = format!(
        "# {}\n\n状态：{:?} / {:?} / {:?}；覆盖完整：{}；版本：{}\n\n",
        a.content.title,
        a.validation,
        a.review,
        a.publication,
        a.coverage_complete,
        a.revision.get()
    );
    for s in &a.content.sections {
        out.push_str(&format!("## {}\n\n", s.heading));
        for claim in &s.claims {
            out.push_str(&format!("- {} [{:?}]\n", claim.text, claim.grounding));
            for e in &claim.evidence {
                match e {
                    AnalysisEvidence::Source { span, .. } => out.push_str(&format!(
                        "  > {} （原文 {} v{} 字节 {}..{}）\n",
                        span.quote,
                        span.segment_id,
                        span.segment_revision.get(),
                        span.start_utf8,
                        span.end_utf8
                    )),
                    AnalysisEvidence::Artifact {
                        artifact_id,
                        revision,
                        quote,
                        ..
                    } => out.push_str(&format!(
                        "  > {} （产物 {} v{}）\n",
                        quote,
                        artifact_id,
                        revision.get()
                    )),
                }
            }
        }
        out.push('\n')
    }
    out
}
impl Store {
    /// Closing speech uses only the separately reviewed public copy. Call again
    /// during playback so withdrawal, edits and peer disconnects stop narration.
    pub fn public_artifact_for_revision(
        &self,
        id: Uuid,
        revision: Revision,
    ) -> Result<PublicArtifact> {
        let tx = self.connection.unchecked_transaction()?;
        let a = load_artifact(&tx, id)?;
        expected_revision(&a, revision)?;
        if a.kind != AnalysisKind::ClosingBrief
            || a.publication != ArtifactPublication::Published
            || a.review != ArtifactReview::Approved
            || a.validation != ArtifactValidation::Valid
            || !a.coverage_complete
            || !snapshot_current(&tx, &load_snapshot(&tx, a.snapshot_id)?.snapshot)?
        {
            return Err(invalid("closing_speech_requires_current_publication"));
        }
        let body: Option<String> = tx
            .query_row(
                "SELECT body_json FROM publication_projection WHERE artifact_id=?1 AND active=1",
                [id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let p: PublicArtifact =
            serde_json::from_str(&body.ok_or(StoreError::NotFound("public_closing_brief"))?)?;
        if p.revision != revision {
            return Err(StoreError::LateResult);
        }
        tx.commit()?;
        Ok(p)
    }
    pub fn list_analysis_jobs(
        &self,
        session: Uuid,
        after: Option<AnalysisPageKey>,
        limit: u32,
    ) -> Result<AnalysisJobPage> {
        let (cursor, items, next_after) =
            page_records(&self.connection, "job", session, after, limit)?;
        Ok(AnalysisJobPage {
            cursor,
            items,
            next_after,
        })
    }
    pub fn list_artifacts(
        &self,
        session: Uuid,
        after: Option<AnalysisPageKey>,
        limit: u32,
    ) -> Result<ArtifactPage> {
        let (cursor, items, next_after) =
            page_records(&self.connection, "artifact", session, after, limit)?;
        Ok(ArtifactPage {
            cursor,
            items,
            next_after,
        })
    }
    pub fn public_snapshot(&self, session: Uuid) -> Result<PublicSnapshot> {
        require_session(&self.connection, session)?;
        let tx = self.connection.unchecked_transaction()?;
        let cursor = tx.query_row(
            "SELECT COALESCE(MAX(publication_seq),0) FROM publication_changes",
            [],
            |r| r.get(0),
        )?;
        let mut stmt=tx.prepare("SELECT p.body_json FROM publication_projection p JOIN analysis_artifacts a ON a.id=p.artifact_id WHERE p.active=1 AND EXISTS(SELECT 1 FROM json_each(a.body_json,'$.session_ids') WHERE value=?1) ORDER BY json_extract(p.body_json,'$.publication_seq')")?;
        let rows = stmt
            .query_map([session.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let artifacts = rows
            .into_iter()
            .map(|b| Ok(serde_json::from_str(&b)?))
            .collect::<Result<_>>()?;
        tx.commit()?;
        Ok(PublicSnapshot { cursor, artifacts })
    }
    pub fn public_changes(
        &self,
        session: Uuid,
        after: u64,
        limit: u32,
    ) -> Result<Vec<PublicationChange>> {
        require_session(&self.connection, session)?;
        if limit == 0 || limit > 1000 {
            return Err(invalid("publication_limit"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let latest: u64 = tx.query_row(
            "SELECT COALESCE(MAX(publication_seq),0) FROM publication_changes",
            [],
            |r| r.get(0),
        )?;
        if after > latest {
            return Err(StoreError::InvalidCursor);
        }
        let mut stmt=tx.prepare("SELECT p.body_json,q.active,q.body_json FROM publication_changes p JOIN analysis_artifacts a ON a.id=p.artifact_id LEFT JOIN publication_projection q ON q.artifact_id=p.artifact_id WHERE p.publication_seq>?1 AND EXISTS(SELECT 1 FROM json_each(a.body_json,'$.session_ids') WHERE value=?2) ORDER BY p.publication_seq LIMIT ?3")?;
        let rows = stmt
            .query_map(params![after, session.to_string(), limit], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<bool>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        let mut out = vec![];
        for (body, active, projection) in rows {
            let mut change: PublicationChange = serde_json::from_str(&body)?;
            let current: Option<PublicArtifact> =
                projection.map(|p| serde_json::from_str(&p)).transpose()?;
            if active != Some(true)
                || current.is_none_or(|p| p.publication_seq != change.publication_seq)
            {
                change.artifact = None;
                change.withdrawn = true;
            }
            out.push(change)
        }
        tx.commit()?;
        Ok(out)
    }
    pub fn resolve_analysis_evidence(&self, e: &AnalysisEvidence) -> Result<AnalysisEvidenceView> {
        match e {
            AnalysisEvidence::Source { session_id, span } => {
                let r = load_revision(
                    &self.connection,
                    *session_id,
                    span.segment_id,
                    span.segment_revision,
                )?;
                span.validate_against(&r.payload.text)?;
                let (_, _, rev) = capture_context(&self.connection, *session_id, span.segment_id)?;
                Ok(AnalysisEvidenceView {
                    text: r.payload.text,
                    quote: span.quote.clone(),
                    current: rev == Some(span.segment_revision.get()),
                })
            }
            AnalysisEvidence::Artifact {
                artifact_id,
                revision,
                start_utf8,
                end_utf8,
                quote,
            } => {
                if let Some(view) = super::forum::peer_evidence(
                    &self.connection,
                    *artifact_id,
                    *revision,
                    *start_utf8,
                    *end_utf8,
                    quote,
                )? {
                    return Ok(view);
                }
                let a = load_artifact(&self.connection, *artifact_id)?;
                let mut stmt=self.connection.prepare("SELECT body_json FROM publication_changes WHERE artifact_id=?1 ORDER BY publication_seq DESC")?;
                let rows = stmt
                    .query_map([artifact_id.to_string()], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                for b in rows {
                    let p: PublicationChange = serde_json::from_str(&b)?;
                    if let Some(p) = p.artifact {
                        if p.revision == *revision
                            && start_utf8 < end_utf8
                            && p.text.get(*start_utf8..*end_utf8) == Some(quote.as_str())
                        {
                            let active:Option<String>=self.connection.query_row("SELECT body_json FROM publication_projection WHERE artifact_id=?1 AND active=1",[artifact_id.to_string()],|r|r.get(0)).optional()?;
                            let current = active
                                .map(|v| serde_json::from_str::<PublicArtifact>(&v))
                                .transpose()?
                                .is_some_and(|v| v.text == p.text && v.title == p.title);
                            return Ok(AnalysisEvidenceView {
                                text: p.text,
                                quote: quote.clone(),
                                current: current
                                    && a.revision == *revision
                                    && a.validation == ArtifactValidation::Valid,
                            });
                        }
                    }
                }
                Err(ValidationError::InvalidSpan.into())
            }
        }
    }
    /// Internal operator export: retains status and evidence. Never serve from LAN.
    pub fn export_artifact(&self, id: Uuid, revision: Revision, format: &str) -> Result<String> {
        let a = self.artifact_revision(id, revision)?;
        match format {
            "json" => Ok(serde_json::to_string_pretty(&a)?),
            "markdown" => Ok(content_markdown(&a)),
            "html" => Ok(format!(
                "<!doctype html><meta charset=\"utf-8\"><title>{}</title><pre>{}</pre>",
                html_escape(&a.content.title),
                html_escape(&content_markdown(&a))
            )),
            _ => Err(invalid("artifact_export_format")),
        }
    }
}
impl Store {
    /// Stable catalog membership while paging; newer meetings appear on refresh.
    pub fn list_sessions_page(
        &self,
        after: Option<u64>,
        limit: u32,
    ) -> Result<(Vec<SessionSummary>, Option<u64>)> {
        if limit == 0 || limit > 100 {
            return Err(invalid("session_page_limit"));
        }
        let tx = self.connection.unchecked_transaction()?;
        let mut q=tx.prepare("SELECT rowid,spec_json FROM sessions WHERE (?1 IS NULL OR rowid<?1) ORDER BY rowid DESC LIMIT ?2")?;
        let mut rows = q
            .query_map(params![after, limit + 1], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(q);
        let more = rows.len() > limit as usize;
        if more {
            rows.pop();
        }
        let next = if more {
            rows.last().map(|(id, _)| *id)
        } else {
            None
        };
        let mut out = vec![];
        for (_, body) in rows {
            let session: SessionSpec = serde_json::from_str(&body)?;
            let id = session.session_id;
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
            let translation_pending=tx.query_row("SELECT COUNT(*) FROM translation_coverage c JOIN segments s ON s.id=c.segment_id WHERE s.session_id=?1 AND c.state IN ('pending','requested','failed')",[id.to_string()],|r|r.get(0))?;
            out.push(SessionSummary {
                session,
                status: load_session_status(&tx, id)?,
                cursor,
                segment_count,
                translation_pending,
            });
        }
        tx.commit()?;
        Ok((out, next))
    }
}
impl Store {
    /// Confirmed map outputs only. A worker must match the current chunk's exact
    /// input hash and revalidate its quotes before constructing current evidence.
    /// Caps bound host memory; omission only causes regeneration, never data loss.
    pub fn reusable_analysis_checkpoints(&self, id: Uuid) -> Result<Vec<AnalysisCheckpoint>> {
        let job = load_job(&self.connection, id)?;
        let mut q=self.connection.prepare("SELECT c.body_json FROM analysis_checkpoints c JOIN analysis_jobs j ON j.id=c.job_id WHERE c.effective_config_hash=?1 AND json_extract(j.body_json,'$.event_id')=?2 AND json_extract(j.body_json,'$.kind')=?3 AND json_extract(j.body_json,'$.session_ids')=json(?4) ORDER BY j.created_at_ms DESC,j.id DESC,c.step_index LIMIT 3000")?;
        let kind = serde_json::to_value(job.kind)?.as_str().unwrap().to_owned();
        let mut rows = q.query(params![
            job.config.effective_config_hash,
            job.event_id.to_string(),
            kind,
            json(&job.session_ids)?
        ])?;
        let mut out = vec![];
        let mut seen = HashSet::new();
        let mut bytes = 0;
        while let Some(row) = rows.next()? {
            let body: String = row.get(0)?;
            if bytes + body.len() > 16 * 1024 * 1024 || out.len() >= 1000 {
                break;
            }
            let checkpoint: AnalysisCheckpoint = serde_json::from_str(&body)?;
            if seen.insert(checkpoint.input_sha256.clone()) {
                bytes += body.len();
                out.push(checkpoint);
            }
        }
        Ok(out)
    }
}

fn snapshot_has_gap(c: &Connection, s: &AnalysisSnapshot) -> Result<bool> {
    for id in &s.session_ids {
        if !s.segments.iter().any(|input| input.session_id == *id) {
            continue;
        }
        let count: u64 = if s.kind == AnalysisKind::Insight {
            let tail = s
                .segments
                .iter()
                .filter(|v| v.session_id == *id)
                .map(|v| v.audio.end_ms)
                .max()
                .unwrap_or(0);
            c.query_row("SELECT COUNT(*) FROM audio_gaps WHERE session_id=?1 AND json_extract(payload_json,'$.audio.end_ms')>?2 AND json_extract(payload_json,'$.audio.start_ms')<?3",params![id.to_string(),tail.saturating_sub(900_000),tail],|r|r.get(0))?
        } else {
            c.query_row(
                "SELECT COUNT(*) FROM audio_gaps WHERE session_id=?1",
                [id.to_string()],
                |r| r.get(0),
            )?
        };
        if count > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(crate) fn invalidate_analysis_gap(c: &Connection, session: Uuid) -> Result<()> {
    let mut q=c.prepare("SELECT body_json FROM analysis_artifacts WHERE EXISTS(SELECT 1 FROM json_each(body_json,'$.session_ids') WHERE value=?1)")?;
    let rows = q
        .query_map([session.to_string()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(q);
    let mut ids = vec![];
    for body in rows {
        let a: ArtifactRecord = serde_json::from_str(&body)?;
        if a.validation != ArtifactValidation::Stale
            && a.coverage_complete
            && snapshot_has_gap(c, &load_snapshot(c, a.snapshot_id)?.snapshot)?
        {
            ids.push(a.artifact_id)
        }
    }
    invalidate_artifacts(c, ids)
}

impl Store {
    /// Trusted runtime hook, only after capture really reaches Stopped. This
    /// durable intent requires neither a model grant nor an analysis snapshot.
    /// A later Stop for the same session gets a fresh marker, including replay.
    pub fn record_analysis_stop_intent(&mut self, session: Uuid) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_session(&tx, session)?;
        tx.execute(
            "DELETE FROM analysis_stop_intents WHERE session_id=?1",
            [session.to_string()],
        )?;
        tx.execute(
            "INSERT INTO analysis_stop_intents(session_id) VALUES(?1)",
            [session.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Oldest confirmed Stop first; the manager creates/deduplicates a durable
    /// job before acknowledging the marker. No model work runs on the writer.
    pub fn pending_analysis_stop_intents(&self, limit: u32) -> Result<Vec<(Uuid, u64)>> {
        if limit == 0 || limit > 1000 {
            return Err(invalid("analysis_stop_intent_limit"));
        }
        let mut q = self.connection.prepare(
            "SELECT session_id,marker FROM analysis_stop_intents ORDER BY marker LIMIT ?1",
        )?;
        let rows = q
            .query_map([limit], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, u64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, marker)| Ok((parse_uuid(&id)?, marker)))
            .collect()
    }

    /// A failed startup with no captured segments has nothing to summarize.
    /// Consume only the observed marker; a later real Stop creates a new one.
    pub fn ack_empty_analysis_stop_intent(&mut self, session: Uuid, marker: u64) -> Result<bool> {
        if marker == 0 || marker > i64::MAX as u64 {
            return Err(invalid("analysis_stop_intent_marker"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_session(&tx, session)?;
        let captured: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM segments s JOIN capture_segments c ON c.segment_id=s.id WHERE s.session_id=?1)",
            [session.to_string()], |r| r.get(0))?;
        if captured {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM analysis_stop_intents WHERE session_id=?1 AND marker=?2",
            params![session.to_string(), marker],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Compare-and-delete. A late/duplicate acknowledgement cannot consume a
    /// new Stop, and an ACK retry after deletion remains harmless.
    pub fn ack_analysis_stop_intent(&mut self, session: Uuid, marker: u64) -> Result<()> {
        if marker == 0 || marker > i64::MAX as u64 {
            return Err(invalid("analysis_stop_intent_marker"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_session(&tx, session)?;
        tx.execute(
            "DELETE FROM analysis_stop_intents WHERE session_id=?1 AND marker=?2",
            params![session.to_string(), marker],
        )?;
        tx.commit()?;
        Ok(())
    }
}
