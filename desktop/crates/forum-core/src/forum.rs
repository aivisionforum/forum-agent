//! Private speaker assignment and authenticated-peer public projection storage.
//! Network authentication is performed by the gateway; these methods still enforce
//! owner/session scope, immutable versions, strict cursors and publication-only DTOs.
use super::*;
use std::collections::HashSet;
fn invalid(name: &'static str) -> StoreError {
    ValidationError::Invalid(name).into()
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn json<T: Serialize>(v: &T) -> Result<String> {
    Ok(canonical_json(&serde_json::to_value(v)?))
}
fn sha(v: &str) -> String {
    format!("{:x}", Sha256::digest(v.as_bytes()))
}
fn digest_ok(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn automatic(origin: &SpeakerAssignmentOrigin) -> bool {
    matches!(origin, SpeakerAssignmentOrigin::Automatic { .. })
}
fn assignment(c: &Connection, session: Uuid, segment: Uuid) -> Result<Option<SpeakerAssignment>> {
    let (_, _, source) = capture_context(c, session, segment)?;
    let body: Option<String> = c
        .query_row(
            "SELECT body_json FROM speaker_assignments WHERE segment_id=?1",
            [segment.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    body.map(|b| {
        let mut a: SpeakerAssignment = serde_json::from_str(&b)?;
        a.current = Some(a.source_revision.get()) == source;
        Ok(a)
    })
    .transpose()
}
fn apply_assignment(
    c: &Connection,
    cmd: &SpeakerAssignmentCommand,
    hash: &str,
) -> Result<SpeakerAssignment> {
    non_nil(cmd.request_id, "speaker_request")?;
    let duplicate: Option<(String, String)> = c
        .query_row(
            "SELECT command_sha256,body_json FROM speaker_assignment_requests WHERE request_id=?1",
            [cmd.request_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((prior, body)) = duplicate {
        if prior != hash {
            return Err(StoreError::EventIdConflict);
        }
        return Ok(serde_json::from_str(&body)?);
    }
    let (_, _, current) = capture_context(c, cmd.session_id, cmd.segment_id)?;
    if current != Some(cmd.source_revision.get()) {
        return Err(StoreError::LateResult);
    }
    let source = load_revision(c, cmd.session_id, cmd.segment_id, cmd.source_revision)?;
    if source.payload.status != TranscriptStatus::Success {
        return Err(invalid("speaker_source_not_success"));
    }
    let prior = assignment(c, cmd.session_id, cmd.segment_id)?;
    if prior.as_ref().map(|a| a.revision) != cmd.expected_revision {
        return Err(StoreError::RevisionConflict {
            expected: cmd.expected_revision.map_or(0, Revision::get),
            actual: prior.as_ref().map(|a| a.revision.get()),
        });
    }
    if automatic(&cmd.origin) && prior.as_ref().is_some_and(|a| !automatic(&a.origin)) {
        return Err(invalid("speaker_human_assignment_locked"));
    }
    match &cmd.origin {
        SpeakerAssignmentOrigin::Automatic {
            model_manifest_id,
            model_version,
        } => {
            if !model_manifest_id
                .strip_prefix("sha256:")
                .is_some_and(digest_ok)
                || model_version.trim().is_empty()
                || model_version.len() > 256
            {
                return Err(invalid("speaker_model_provenance"));
            }
        }
        SpeakerAssignmentOrigin::Human {
            operator_id,
            reason,
        } => {
            if operator_id.trim().is_empty()
                || operator_id.len() > 256
                || reason.trim().is_empty()
                || reason.len() > 4096
            {
                return Err(invalid("speaker_operator_reason"));
            }
        }
    }
    if let SpeakerLabel::Anonymous { speaker_id } = cmd.label {
        non_nil(speaker_id, "speaker_id")?;
        let owner: Option<String> = c
            .query_row(
                "SELECT session_id FROM speaker_clusters WHERE speaker_id=?1",
                [speaker_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if owner
            .as_ref()
            .is_some_and(|id| *id != cmd.session_id.to_string())
        {
            return Err(StoreError::ScopeMismatch);
        }
        if owner.is_none() {
            c.execute(
                "INSERT INTO speaker_clusters VALUES(?1,?2,'human','[]')",
                params![speaker_id.to_string(), cmd.session_id.to_string()],
            )?;
        }
    }
    let changed = prior
        .as_ref()
        .is_none_or(|a| a.label != cmd.label || a.source_revision != cmd.source_revision);
    let a = SpeakerAssignment {
        session_id: cmd.session_id,
        segment_id: cmd.segment_id,
        source_revision: cmd.source_revision,
        revision: prior
            .map(|a| a.revision.next())
            .transpose()?
            .unwrap_or(Revision::FIRST),
        label: cmd.label.clone(),
        origin: cmd.origin.clone(),
        created_at_ms: now(),
        current: true,
    };
    let body = json(&a)?;
    c.execute("INSERT INTO speaker_assignments VALUES(?1,?2,?3) ON CONFLICT(segment_id) DO UPDATE SET body_json=excluded.body_json",params![cmd.segment_id.to_string(),cmd.session_id.to_string(),body])?;
    c.execute(
        "INSERT INTO speaker_assignment_revisions VALUES(?1,?2,?3)",
        params![cmd.segment_id.to_string(), a.revision.get(), body],
    )?;
    c.execute(
        "INSERT INTO speaker_assignment_requests VALUES(?1,?2,?3)",
        params![cmd.request_id.to_string(), hash, body],
    )?;
    if changed {
        super::analysis::invalidate_analysis_source(c, cmd.segment_id)?;
    }
    Ok(a)
}
impl Store {
    pub fn apply_speaker_assignment(
        &mut self,
        cmd: &SpeakerAssignmentCommand,
    ) -> Result<SpeakerAssignment> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let a = apply_assignment(&tx, cmd, &sha(&json(cmd)?))?;
        tx.commit()?;
        Ok(a)
    }
    /// Provisional ECAPA thresholds: accept >= .78 with >= .08 separation;
    /// enroll a new anonymous voice only when every similarity is below .60.
    /// Ambiguous audio remains unknown. No cross-session or cross-model matching.
    pub fn apply_speaker_embedding(
        &mut self,
        cmd: &SpeakerEmbeddingCommand,
    ) -> Result<SpeakerAssignment> {
        if cmd.embedding.len() != 192
            || cmd.embedding.iter().any(|v| !v.is_finite())
            || !digest_ok(&cmd.pcm_sha256)
        {
            return Err(invalid("speaker_embedding"));
        }
        let norm = cmd.embedding.iter().map(|v| v * v).sum::<f32>().sqrt();
        if (norm - 1.0).abs() > 0.02 {
            return Err(invalid("speaker_embedding_normalization"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let hash = sha(&json(cmd)?);
        let duplicate:Option<(String,String)>=tx.query_row("SELECT command_sha256,body_json FROM speaker_assignment_requests WHERE request_id=?1",[cmd.request_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((old, body)) = duplicate {
            if old != hash {
                return Err(StoreError::EventIdConflict);
            }
            return Ok(serde_json::from_str(&body)?);
        }
        let mut q=tx.prepare("SELECT speaker_id,embedding_json FROM speaker_clusters WHERE session_id=?1 AND model_manifest_id=?2 LIMIT 101")?;
        let rows = q
            .query_map(
                params![cmd.session_id.to_string(), cmd.model_manifest_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(q);
        if rows.len() > 100 {
            return Err(invalid("speaker_cluster_limit"));
        }
        let mut scores = vec![];
        for (id, b) in rows {
            let e: Vec<f32> = serde_json::from_str(&b)?;
            if e.len() == 192 {
                scores.push((
                    cmd.embedding.iter().zip(e).map(|(a, b)| a * b).sum::<f32>(),
                    parse_uuid(&id)?,
                ));
            }
        }
        scores.sort_by(|a, b| b.0.total_cmp(&a.0));
        let label = if scores.first().is_some_and(|s| s.0 >= 0.78)
            && scores.get(1).is_none_or(|s| scores[0].0 - s.0 >= 0.08)
        {
            SpeakerLabel::Anonymous {
                speaker_id: scores[0].1,
            }
        } else if scores.first().is_none_or(|s| s.0 < 0.60) && scores.len() < 100 {
            let id = Uuid::new_v4();
            tx.execute(
                "INSERT INTO speaker_clusters VALUES(?1,?2,?3,?4)",
                params![
                    id.to_string(),
                    cmd.session_id.to_string(),
                    cmd.model_manifest_id,
                    json(&cmd.embedding)?
                ],
            )?;
            SpeakerLabel::Anonymous { speaker_id: id }
        } else {
            SpeakerLabel::Unknown
        };
        let a = apply_assignment(
            &tx,
            &SpeakerAssignmentCommand {
                request_id: cmd.request_id,
                session_id: cmd.session_id,
                segment_id: cmd.segment_id,
                source_revision: cmd.source_revision,
                expected_revision: cmd.expected_revision,
                label,
                origin: SpeakerAssignmentOrigin::Automatic {
                    model_manifest_id: cmd.model_manifest_id.clone(),
                    model_version: cmd.model_version.clone(),
                },
            },
            &hash,
        )?;
        tx.commit()?;
        Ok(a)
    }
    pub fn speaker_assignment(
        &self,
        session: Uuid,
        segment: Uuid,
    ) -> Result<Option<SpeakerAssignment>> {
        assignment(&self.connection, session, segment)
    }
    pub fn list_speaker_assignments(&self, session: Uuid) -> Result<Vec<SpeakerAssignment>> {
        require_session(&self.connection, session)?;
        let mut q=self.connection.prepare("SELECT a.body_json,s.current_revision FROM speaker_assignments a JOIN segments s ON s.id=a.segment_id WHERE a.session_id=?1 ORDER BY a.rowid LIMIT 10000")?;
        let rows = q
            .query_map([session.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<u32>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(b, r)| {
                let mut a: SpeakerAssignment = serde_json::from_str(&b)?;
                a.current = Some(a.source_revision.get()) == r;
                Ok(a)
            })
            .collect()
    }
    pub fn speaker_assignment_history(
        &self,
        session: Uuid,
        segment: Uuid,
    ) -> Result<Vec<SpeakerAssignment>> {
        let (_, _, current) = capture_context(&self.connection, session, segment)?;
        let mut q=self.connection.prepare("SELECT body_json FROM speaker_assignment_revisions WHERE segment_id=?1 ORDER BY revision")?;
        let rows = q
            .query_map([segment.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| {
                let mut a: SpeakerAssignment = serde_json::from_str(&b)?;
                a.current = Some(a.source_revision.get()) == current;
                Ok(a)
            })
            .collect()
    }
}
fn peer(c: &Connection, owner: Uuid, session: Uuid) -> Result<PeerSessionState> {
    let row:Option<(String,u64,Option<u64>,bool)>=c.query_row("SELECT body_json,cursor,last_sync_at_ms,stale FROM peer_sessions WHERE session_id=?1 AND owner_device_id=?2",params![session.to_string(),owner.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (b, cursor, last_sync_at_ms, stale) = row.ok_or(StoreError::NotFound("peer_session"))?;
    Ok(PeerSessionState {
        session: serde_json::from_str(&b)?,
        cursor,
        last_sync_at_ms,
        stale,
    })
}
fn alias(owner: Uuid, session: Uuid, public: Uuid) -> Uuid {
    let bytes =
        Sha256::digest(format!("forum-peer-public-v1/{owner}/{session}/{public}").as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&bytes[..16]);
    id[6] = (id[6] & 15) | 0x50;
    id[8] = (id[8] & 63) | 0x80;
    Uuid::from_bytes(id)
}
fn validate_public(a: &PublicArtifact, cursor: u64) -> Result<()> {
    non_nil(a.public_id, "public_id")?;
    if a.publication_seq == 0
        || a.publication_seq > cursor
        || a.title.trim().is_empty()
        || a.title.len() > 2048
        || a.text.trim().is_empty()
        || a.text.len() > 1024 * 1024
        || a.evidence.is_empty()
        || a.evidence.len() > 1000
        || matches!(
            a.kind,
            AnalysisKind::SuggestedQuestions | AnalysisKind::RedactionReview
        )
    {
        return Err(invalid("peer_public_projection"));
    }
    let mut seen = HashSet::new();
    for e in &a.evidence {
        if e.public_evidence_id.is_nil()
            || !seen.insert(e.public_evidence_id)
            || e.text.trim().is_empty()
            || e.text.len() > 64 * 1024
        {
            return Err(invalid("peer_public_evidence"));
        }
    }
    Ok(())
}
fn peer_artifacts(c: &Connection, session: Uuid) -> Result<Vec<PublicArtifact>> {
    let mut q = c.prepare(
        "SELECT body_json FROM peer_publications WHERE session_id=?1 ORDER BY public_id",
    )?;
    let rows = q
        .query_map([session.to_string()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|b| Ok(serde_json::from_str(&b)?))
        .collect()
}
fn semantic(a: &PublicArtifact) -> Result<String> {
    let mut a = a.clone();
    a.publication_seq = 0;
    json(&a)
}
fn record_peer_version(c: &Connection, s: &PeerSessionState, a: &PublicArtifact) -> Result<()> {
    let id = alias(s.session.owner_device_id, s.session.session_id, a.public_id);
    let local: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM analysis_artifacts WHERE id=?1)",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if local {
        return Err(StoreError::EntityConflict);
    }
    let watermark: Option<(u64, bool)> = c
        .query_row(
            "SELECT publication_seq,withdrawn FROM peer_publication_watermarks WHERE alias_id=?1",
            [id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if watermark.is_some_and(|(seq, withdrawn)| {
        a.publication_seq < seq || (withdrawn && a.publication_seq <= seq)
    }) {
        return Err(invalid("peer_publication_sequence_regressed"));
    }

    let old: Option<String> = c
        .query_row(
            "SELECT body_json FROM peer_publication_history WHERE alias_id=?1 AND revision=?2",
            params![id.to_string(), a.revision.get()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(old) = old {
        if semantic(&serde_json::from_str(&old)?)? != semantic(a)? {
            return Err(invalid("peer_revision_content_changed"));
        }
    }
    let maxrev: Option<u32> = c.query_row(
        "SELECT MAX(revision) FROM peer_publication_history WHERE alias_id=?1",
        [id.to_string()],
        |r| r.get(0),
    )?;
    if maxrev.is_some_and(|v| v > a.revision.get()) {
        return Err(invalid("peer_revision_regressed"));
    }
    c.execute(
        "INSERT OR IGNORE INTO peer_publication_history VALUES(?1,?2,?3)",
        params![id.to_string(), a.revision.get(), json(a)?],
    )?;
    Ok(())
}
fn replace_peer(
    c: &Connection,
    s: &PeerSessionState,
    cursor: u64,
    artifacts: &[PublicArtifact],
) -> Result<()> {
    if cursor > 9_007_199_254_740_991
        || artifacts.len() > 1000
        || json(&artifacts)?.len() > 16 * 1024 * 1024
    {
        return Err(invalid("peer_snapshot_size"));
    }
    let mut seen = HashSet::new();
    for a in artifacts {
        validate_public(a, cursor)?;
        if !seen.insert(a.public_id) {
            return Err(invalid("peer_duplicate_public_id"));
        }
    }
    let mut sorted = artifacts.to_vec();
    sorted.sort_by_key(|a| a.public_id);
    let hash = sha(&json(&sorted)?);
    let prior: Option<String> = c.query_row(
        "SELECT snapshot_sha256 FROM peer_sessions WHERE session_id=?1",
        [s.session.session_id.to_string()],
        |r| r.get(0),
    )?;
    if cursor < s.cursor {
        return Err(StoreError::InvalidCursor);
    }
    if cursor == s.cursor && prior.as_ref().is_some_and(|h| *h != hash) {
        return Err(StoreError::EventIdConflict);
    }
    let before = peer_artifacts(c, s.session.session_id)?;
    for a in &sorted {
        record_peer_version(c, s, a)?;
    }
    for old in before {
        if !sorted.iter().any(|new| new.public_id == old.public_id) {
            c.execute("INSERT INTO peer_publication_watermarks VALUES(?1,?2,1) ON CONFLICT(alias_id) DO UPDATE SET publication_seq=excluded.publication_seq,withdrawn=1",params![alias(s.session.owner_device_id,s.session.session_id,old.public_id).to_string(),cursor])?;
        }

        if !sorted
            .iter()
            .any(|new| new.public_id == old.public_id && semantic(new).ok() == semantic(&old).ok())
        {
            super::analysis::invalidate_dependents(
                c,
                alias(
                    s.session.owner_device_id,
                    s.session.session_id,
                    old.public_id,
                ),
            )?;
        }
    }
    c.execute(
        "DELETE FROM peer_publications WHERE session_id=?1",
        [s.session.session_id.to_string()],
    )?;
    for a in &sorted {
        c.execute("INSERT INTO peer_publication_watermarks VALUES(?1,?2,0) ON CONFLICT(alias_id) DO UPDATE SET publication_seq=excluded.publication_seq,withdrawn=0",params![alias(s.session.owner_device_id,s.session.session_id,a.public_id).to_string(),a.publication_seq])?;
        c.execute(
            "INSERT INTO peer_publications VALUES(?1,?2,?3,?4)",
            params![
                alias(s.session.owner_device_id, s.session.session_id, a.public_id).to_string(),
                s.session.session_id.to_string(),
                a.public_id.to_string(),
                json(a)?
            ],
        )?;
    }
    c.execute("UPDATE peer_sessions SET cursor=?2,snapshot_sha256=?3,last_sync_at_ms=?4,stale=0,last_batch_sha256=NULL WHERE session_id=?1",params![s.session.session_id.to_string(),cursor,hash,now()])?;
    Ok(())
}
impl Store {
    /// Only call after authenticated pairing and explicit confirmation of public metadata.
    pub fn register_peer_session(&mut self, s: &PeerSession) -> Result<PeerSessionState> {
        for id in [s.session_id, s.event_id, s.owner_device_id] {
            non_nil(id, "peer_scope")?;
        }
        if s.title.trim().is_empty()
            || s.title.len() > 2048
            || s.room_name.trim().is_empty()
            || s.room_name.len() > 256
        {
            return Err(invalid("peer_public_metadata"));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let local: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
            [s.session_id.to_string()],
            |r| r.get(0),
        )?;
        if local {
            return Err(StoreError::ScopeMismatch);
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT body_json FROM peer_sessions WHERE session_id=?1",
                [s.session_id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = existing {
            let old: PeerSession = serde_json::from_str(&old)?;
            if old != *s {
                return Err(StoreError::ScopeMismatch);
            }
        } else {
            let count: u64 =
                tx.query_row("SELECT COUNT(*) FROM peer_sessions", [], |r| r.get(0))?;
            if count >= 1000 {
                return Err(invalid("peer_session_limit"));
            }
            tx.execute("INSERT INTO peer_sessions(session_id,owner_device_id,event_id,body_json) VALUES(?1,?2,?3,?4)",params![s.session_id.to_string(),s.owner_device_id.to_string(),s.event_id.to_string(),json(s)?])?;
        }
        let state = peer(&tx, s.owner_device_id, s.session_id)?;
        tx.commit()?;
        Ok(state)
    }
    pub fn peer_sessions(&self, event: Uuid) -> Result<Vec<PeerSessionState>> {
        let mut q=self.connection.prepare("SELECT owner_device_id,session_id FROM peer_sessions WHERE event_id=?1 ORDER BY session_id LIMIT 1000")?;
        let rows = q
            .query_map([event.to_string()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(o, s)| peer(&self.connection, parse_uuid(&o)?, parse_uuid(&s)?))
            .collect()
    }
    pub fn apply_peer_snapshot(
        &mut self,
        snapshot: &PeerPublicationSnapshot,
    ) -> Result<PeerSessionState> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = peer(&tx, snapshot.owner_device_id, snapshot.session_id)?;
        if s.session.event_id != snapshot.event_id {
            return Err(StoreError::ScopeMismatch);
        }
        replace_peer(&tx, &s, snapshot.cursor, &snapshot.artifacts)?;
        let state = peer(&tx, snapshot.owner_device_id, snapshot.session_id)?;
        tx.commit()?;
        Ok(state)
    }
    pub fn apply_peer_changes(&mut self, batch: &PeerPublicationBatch) -> Result<PeerSessionState> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let s = peer(&tx, batch.owner_device_id, batch.session_id)?;
        if s.session.event_id != batch.event_id {
            return Err(StoreError::ScopeMismatch);
        }
        let batch_hash = sha(&json(batch)?);
        let prior_hash: Option<String> = tx.query_row(
            "SELECT last_batch_sha256 FROM peer_sessions WHERE session_id=?1",
            [batch.session_id.to_string()],
            |r| r.get(0),
        )?;
        if batch.cursor == s.cursor && prior_hash.as_ref() == Some(&batch_hash) {
            tx.execute(
                "UPDATE peer_sessions SET stale=0,last_sync_at_ms=?2 WHERE session_id=?1",
                params![batch.session_id.to_string(), now()],
            )?;
            let fresh = peer(&tx, batch.owner_device_id, batch.session_id)?;
            tx.commit()?;
            return Ok(fresh);
        }
        if batch.after_cursor != s.cursor || batch.cursor < batch.after_cursor {
            return Err(StoreError::InvalidCursor);
        }
        if batch.changes.len() > 1000 {
            return Err(invalid("peer_change_limit"));
        }
        let mut list = peer_artifacts(&tx, batch.session_id)?;
        let mut previous = batch.after_cursor;
        for change in &batch.changes {
            if change.publication_seq <= previous || change.publication_seq > batch.cursor {
                return Err(StoreError::InvalidCursor);
            }
            previous = change.publication_seq;
            non_nil(change.public_id, "peer_public_id")?;
            if change.withdrawn {
                if change.artifact.is_some() {
                    return Err(invalid("peer_tombstone_has_content"));
                }
                tx.execute("INSERT INTO peer_publication_watermarks VALUES(?1,?2,1) ON CONFLICT(alias_id) DO UPDATE SET publication_seq=MAX(publication_seq,excluded.publication_seq),withdrawn=1",params![alias(batch.owner_device_id,batch.session_id,change.public_id).to_string(),change.publication_seq])?;
                list.retain(|a| a.public_id != change.public_id);
            } else {
                let a = change
                    .artifact
                    .as_ref()
                    .ok_or(invalid("peer_change_missing_content"))?;
                validate_public(a, batch.cursor)?;
                if a.public_id != change.public_id || a.publication_seq != change.publication_seq {
                    return Err(invalid("peer_change_identity"));
                }
                record_peer_version(&tx, &s, a)?;
                list.retain(|p| p.public_id != a.public_id);
                list.push(a.clone());
            }
        }
        replace_peer(&tx, &s, batch.cursor, &list)?;
        tx.execute(
            "UPDATE peer_sessions SET last_batch_sha256=?2 WHERE session_id=?1",
            params![batch.session_id.to_string(), batch_hash],
        )?;
        let state = peer(&tx, batch.owner_device_id, batch.session_id)?;
        tx.commit()?;
        Ok(state)
    }
    pub fn mark_peer_disconnected(
        &mut self,
        owner: Uuid,
        session: Uuid,
    ) -> Result<PeerSessionState> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        peer(&tx, owner, session)?;
        tx.execute(
            "UPDATE peer_sessions SET stale=1 WHERE session_id=?1",
            [session.to_string()],
        )?;
        for a in peer_artifacts(&tx, session)? {
            super::analysis::invalidate_dependents(&tx, alias(owner, session, a.public_id))?;
        }
        let s = peer(&tx, owner, session)?;
        tx.commit()?;
        Ok(s)
    }
    pub fn peer_public_snapshot(
        &self,
        owner: Uuid,
        session: Uuid,
    ) -> Result<PeerPublicationSnapshot> {
        let tx = self.connection.unchecked_transaction()?;
        let s = peer(&tx, owner, session)?;
        let artifacts = peer_artifacts(&tx, session)?;
        tx.commit()?;
        Ok(PeerPublicationSnapshot {
            owner_device_id: owner,
            event_id: s.session.event_id,
            session_id: session,
            cursor: s.cursor,
            artifacts,
        })
    }
}
// Analysis keeps peer provenance in a host-owned alias lookup. Worker receives only
// immutable reviewed text, never peer credentials, URLs, internal IDs or raw audio.
pub(super) fn analysis_session_event(c: &Connection, session: Uuid) -> Result<Uuid> {
    let remote: Option<String> = c
        .query_row(
            "SELECT event_id FROM peer_sessions WHERE session_id=?1",
            [session.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(event) = remote {
        parse_uuid(&event)
    } else {
        Ok(require_session(c, session)?.event_id)
    }
}
pub(super) fn analysis_peer_inputs(
    c: &Connection,
    session: Uuid,
) -> Result<Vec<AnalysisPublishedInput>> {
    let row: Option<(String, bool)> = c
        .query_row(
            "SELECT owner_device_id,stale FROM peer_sessions WHERE session_id=?1",
            [session.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((owner, stale)) = row else {
        return Ok(vec![]);
    };
    if stale {
        return Err(invalid("peer_input_stale"));
    }
    let owner = parse_uuid(&owner)?;
    peer_artifacts(c, session)?
        .into_iter()
        .map(|a| {
            Ok(AnalysisPublishedInput {
                artifact_id: alias(owner, session, a.public_id),
                revision: a.revision,
                session_ids: vec![session],
                kind: a.kind,
                title: a.title,
                text: a.text,
            })
        })
        .collect()
}
pub(super) fn peer_input_current(
    c: &Connection,
    input: &AnalysisPublishedInput,
) -> Result<Option<bool>> {
    let row:Option<(String,bool)>=c.query_row("SELECT p.body_json,s.stale FROM peer_publications p JOIN peer_sessions s ON s.session_id=p.session_id WHERE p.alias_id=?1",[input.artifact_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((b, stale)) = row {
        let a: PublicArtifact = serde_json::from_str(&b)?;
        return Ok(Some(
            !stale
                && a.revision == input.revision
                && a.title == input.title
                && a.text == input.text,
        ));
    }
    let known: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM peer_publication_history WHERE alias_id=?1)",
        [input.artifact_id.to_string()],
        |r| r.get(0),
    )?;
    Ok(known.then_some(false))
}
pub(super) fn peer_evidence(
    c: &Connection,
    id: Uuid,
    revision: Revision,
    start: usize,
    end: usize,
    quote: &str,
) -> Result<Option<AnalysisEvidenceView>> {
    let old: Option<String> = c
        .query_row(
            "SELECT body_json FROM peer_publication_history WHERE alias_id=?1 AND revision=?2",
            params![id.to_string(), revision.get()],
            |r| r.get(0),
        )
        .optional()?;
    let Some(old) = old else { return Ok(None) };
    let a: PublicArtifact = serde_json::from_str(&old)?;
    if start >= end || a.text.get(start..end) != Some(quote) {
        return Err(ValidationError::InvalidSpan.into());
    }
    let input = AnalysisPublishedInput {
        artifact_id: id,
        revision,
        session_ids: vec![],
        kind: a.kind,
        title: a.title,
        text: a.text.clone(),
    };
    Ok(Some(AnalysisEvidenceView {
        text: a.text,
        quote: quote.into(),
        current: peer_input_current(c, &input)? == Some(true),
    }))
}
impl Store {
    /// Safe public-only activity projection. Empty selection means all rooms.
    /// Local private meeting titles are deliberately not exposed by this API.
    pub fn public_sessions(
        &self,
        event: Uuid,
        selected: &[Uuid],
    ) -> Result<Vec<PublicSessionView>> {
        if selected.len() > 100 || selected.iter().collect::<HashSet<_>>().len() != selected.len() {
            return Err(invalid("public_session_selection"));
        }
        let mut out = vec![];
        let mut q=self.connection.prepare("SELECT spec_json FROM sessions WHERE event_id=?1 AND (?2='[]' OR id IN (SELECT value FROM json_each(?2))) ORDER BY id LIMIT 101")?;
        let rows = q
            .query_map(params![event.to_string(), json(&selected)?], |r| {
                r.get::<_, String>(0)
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(q);
        for body in rows {
            let spec: SessionSpec = serde_json::from_str(&body)?;
            if !selected.is_empty() && !selected.contains(&spec.session_id) {
                continue;
            }
            let p = self.public_snapshot(spec.session_id)?;
            out.push(PublicSessionView {
                owner_device_id: spec.owner_device_id,
                event_id: event,
                session_id: spec.session_id,
                title: "公开会场".into(),
                room_name: "本机会场".into(),
                local: true,
                stale: false,
                last_sync_at_ms: None,
                cursor: p.cursor,
                artifacts: p.artifacts,
            });
        }
        for state in self.peer_sessions(event)? {
            let spec = state.session;
            if !selected.is_empty() && !selected.contains(&spec.session_id) {
                continue;
            }
            let p = self.peer_public_snapshot(spec.owner_device_id, spec.session_id)?;
            out.push(PublicSessionView {
                owner_device_id: spec.owner_device_id,
                event_id: event,
                session_id: spec.session_id,
                title: spec.title,
                room_name: spec.room_name,
                local: false,
                stale: state.stale,
                last_sync_at_ms: state.last_sync_at_ms,
                cursor: p.cursor,
                artifacts: p.artifacts,
            });
        }
        if out.len() > 100 {
            return Err(invalid("public_session_limit"));
        }
        if !selected.is_empty()
            && selected
                .iter()
                .any(|id| !out.iter().any(|s| s.session_id == *id))
        {
            return Err(StoreError::ScopeMismatch);
        }
        Ok(out)
    }
    /// Literal case-insensitive search over reviewed public copies only. Returned
    /// match offsets are UTF-8 byte offsets into the returned bounded snippet.
    pub fn search_public(
        &self,
        event: Uuid,
        selected: &[Uuid],
        query: &str,
        limit: u32,
    ) -> Result<Vec<PublicSearchHit>> {
        let query = query.trim();
        if query.is_empty() || query.len() > 256 || limit == 0 || limit > 100 {
            return Err(invalid("public_search_query"));
        }
        let needle = query.to_lowercase();
        let mut hits = vec![];
        for s in self.public_sessions(event, selected)? {
            for a in s.artifacts {
                let hay = format!(
                    "{}\n{}\n{}",
                    a.title,
                    a.text,
                    a.evidence
                        .iter()
                        .map(|e| e.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                );
                let mut lower = String::new();
                let mut mapping = vec![];
                for (pos, ch) in hay.char_indices() {
                    for lc in ch.to_lowercase() {
                        let before = lower.len();
                        lower.push(lc);
                        for _ in before..lower.len() {
                            mapping.push((pos, pos + ch.len_utf8()));
                        }
                    }
                }
                let Some(index) = lower.find(&needle) else {
                    continue;
                };
                let start = mapping[index].0;
                let end = mapping[index + needle.len() - 1].1;
                let mut left = start.saturating_sub(240);
                while !hay.is_char_boundary(left) {
                    left += 1
                }
                let mut right = (end + 240).min(hay.len());
                while !hay.is_char_boundary(right) {
                    right -= 1
                }
                hits.push(PublicSearchHit {
                    owner_device_id: s.owner_device_id,
                    session_id: s.session_id,
                    public_id: a.public_id,
                    revision: a.revision,
                    title: a.title,
                    text: hay[left..right].into(),
                    match_start_utf8: start - left,
                    match_end_utf8: end - left,
                    stale: s.stale,
                    last_sync_at_ms: s.last_sync_at_ms,
                });
                if hits.len() >= limit as usize {
                    return Ok(hits);
                }
            }
        }
        Ok(hits)
    }
}
pub(super) fn analysis_speaker(
    c: &Connection,
    session: Uuid,
    segment: Uuid,
    fallback: Option<Uuid>,
) -> Result<Option<Uuid>> {
    Ok(match assignment(c, session, segment)? {
        Some(a) if a.current => match a.label {
            SpeakerLabel::Anonymous { speaker_id } => Some(speaker_id),
            _ => None,
        },
        Some(_) => None,
        None => fallback,
    })
}
pub(super) fn selection_alias(c: &Connection, s: &PublicSelection) -> Result<Uuid> {
    let remote: Option<String> = c
        .query_row(
            "SELECT owner_device_id FROM peer_sessions WHERE session_id=?1",
            [s.session_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(owner) = remote {
        if owner != s.owner_device_id.to_string() {
            return Err(StoreError::ScopeMismatch);
        }
        return Ok(alias(s.owner_device_id, s.session_id, s.public_id));
    }
    if require_session(c, s.session_id)?.owner_device_id != s.owner_device_id {
        return Err(StoreError::ScopeMismatch);
    }
    let id: Option<String> = c
        .query_row(
            "SELECT artifact_id FROM publication_projection WHERE public_id=?1 AND active=1",
            [s.public_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    parse_uuid(&id.ok_or(StoreError::NotFound("selected_public_artifact"))?)
}
impl Store {
    pub fn all_peer_sessions(&self) -> Result<Vec<PeerSessionState>> {
        let mut q=self.connection.prepare("SELECT owner_device_id,session_id FROM peer_sessions ORDER BY event_id,session_id LIMIT 1000")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(o, s)| peer(&self.connection, parse_uuid(&o)?, parse_uuid(&s)?))
            .collect()
    }
    /// Call on host startup: persisted publication copies outlive network leases.
    pub fn disconnect_all_peers(&mut self) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut q =
            tx.prepare("SELECT owner_device_id,session_id FROM peer_sessions WHERE stale=0")?;
        let rows = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(q);
        for (owner, session) in rows {
            let owner = parse_uuid(&owner)?;
            let session = parse_uuid(&session)?;
            for a in peer_artifacts(&tx, session)? {
                super::analysis::invalidate_dependents(&tx, alias(owner, session, a.public_id))?;
            }
        }
        tx.execute("UPDATE peer_sessions SET stale=1", [])?;
        tx.commit()?;
        Ok(())
    }
}
pub(super) fn save_peer_provenance(c: &Connection, snapshot: &AnalysisSnapshot) -> Result<()> {
    for input in &snapshot.published_artifacts {
        let row:Option<(String,String,String,u64,String)>=c.query_row("SELECT s.owner_device_id,s.event_id,s.session_id,s.cursor,p.public_id FROM peer_publications p JOIN peer_sessions s ON s.session_id=p.session_id WHERE p.alias_id=?1",[input.artifact_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        if let Some((owner, event, session, cursor, public)) = row {
            let provenance = PeerAnalysisProvenance {
                snapshot_id: snapshot.snapshot_id,
                owner_device_id: parse_uuid(&owner)?,
                event_id: parse_uuid(&event)?,
                session_id: parse_uuid(&session)?,
                source_cursor: cursor,
                public_id: parse_uuid(&public)?,
                revision: input.revision,
                artifact_alias_id: input.artifact_id,
            };
            c.execute(
                "INSERT OR IGNORE INTO analysis_peer_provenance VALUES(?1,?2,?3)",
                params![
                    snapshot.snapshot_id.to_string(),
                    input.artifact_id.to_string(),
                    json(&provenance)?
                ],
            )?;
        }
    }
    Ok(())
}
impl Store {
    pub fn analysis_peer_provenance(&self, job: Uuid) -> Result<Vec<PeerAnalysisProvenance>> {
        let snapshot = self.analysis_job(job)?.snapshot_id;
        let mut q = self.connection.prepare(
            "SELECT body_json FROM analysis_peer_provenance WHERE snapshot_id=?1 ORDER BY alias_id",
        )?;
        let rows = q
            .query_map([snapshot.to_string()], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|b| Ok(serde_json::from_str(&b)?))
            .collect()
    }
}
