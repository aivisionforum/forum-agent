use super::*;

/// Capture ownership is fixed at registration, independent of delayed ASR.
pub(crate) fn capture_direction_at(
    connection: &Connection,
    session_id: Uuid,
    track_id: Uuid,
    audio: &AudioRange,
) -> Result<(u64, Option<String>, Option<String>)> {
    let crossing:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM direction_boundaries WHERE session_id=?1 AND track_id=?2 AND start_sample>?3 AND start_sample<?4)",params![session_id.to_string(),track_id.to_string(),audio.start_sample,audio.end_sample],|r|r.get(0))?;
    if crossing {
        return Err(StoreError::ScopeMismatch);
    }
    let (base_epoch, base_targets): (u64, Option<String>) = connection.query_row(
        "SELECT retroactive_epoch,retroactive_targets_json FROM sessions WHERE id=?1",
        [session_id.to_string()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let boundary:Option<(u64,String,String)>=connection.query_row("SELECT direction_epoch,configured_source_language,targets_json FROM direction_boundaries WHERE session_id=?1 AND track_id=?2 AND start_sample<=?3 AND direction_epoch>?4 ORDER BY direction_epoch DESC LIMIT 1",params![session_id.to_string(),track_id.to_string(),audio.start_sample,base_epoch],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    Ok(match boundary {
        Some((epoch, source, targets)) => (epoch, Some(source), Some(targets)),
        None => (base_epoch, None, base_targets),
    })
}

pub(crate) fn validate_final_direction(
    connection: &Connection,
    session_id: Uuid,
    p: &TranscriptFinal,
) -> Result<()> {
    let (epoch,configured,targets):(u64,Option<String>,Option<String>)=connection.query_row("SELECT direction_epoch,configured_source_language,targets_json FROM capture_segments WHERE segment_id=?1",[p.segment_id.to_string()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    let retroactive: u64 = connection.query_row(
        "SELECT retroactive_epoch FROM sessions WHERE id=?1",
        [session_id.to_string()],
        |r| r.get(0),
    )?;
    // Explicit retroactive translation preserves original ASR provenance while
    // creating coverage in the new direction. Live boundaries never do this.
    if p.direction_epoch != epoch && !(p.direction_epoch < epoch && epoch == retroactive) {
        return Err(StoreError::LateResult);
    }
    if let Some(source) = configured {
        if source != p.configured_source_language
            || targets
                .as_deref()
                .map(serde_json::from_str::<Vec<String>>)
                .transpose()?
                .as_ref()
                != Some(&p.target_languages)
        {
            return Err(StoreError::ScopeMismatch);
        }
    }
    Ok(())
}

pub(crate) fn apply_direction_boundary(
    tx: &Transaction<'_>,
    event: &Event<DirectionChanged>,
    seq: u64,
) -> Result<bool> {
    let Some(boundary) = &event.payload.boundary else {
        return Ok(false);
    };
    non_nil(boundary.track_id, "track_id")?;
    non_blank(
        &boundary.configured_source_language,
        "configured_source_language",
    )?;
    if boundary.start_sample > 9_007_199_254_740_991 {
        return Err(ValidationError::Invalid("start_sample").into());
    }
    let status = load_session_status(tx, event.session_id)?;
    if status.capture_stopped
        || !matches!(
            status.state,
            SessionState::Recording
                | SessionState::Ready
                | SessionState::Preparing
                | SessionState::Interrupted
        )
    {
        return Err(StoreError::InvalidState);
    }
    let owner: Option<String> = tx
        .query_row(
            "SELECT session_id FROM tracks WHERE id=?1",
            [boundary.track_id.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    if owner.as_deref() != Some(event.session_id.to_string().as_str()) {
        return Err(StoreError::ScopeMismatch);
    }
    let beyond:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM capture_segments c JOIN segments s ON s.id=c.segment_id WHERE s.track_id=?1 AND json_extract(c.audio_json,'$.end_sample')>?2) OR EXISTS(SELECT 1 FROM direction_boundaries WHERE track_id=?1 AND start_sample>?2)",params![boundary.track_id.to_string(),boundary.start_sample],|r|r.get(0))?;
    if beyond {
        return Err(StoreError::SealMismatch);
    }
    tx.execute("INSERT INTO direction_boundaries(session_id,direction_epoch,track_id,start_sample,configured_source_language,targets_json,created_seq) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![event.session_id.to_string(),event.payload.direction_epoch,boundary.track_id.to_string(),boundary.start_sample,boundary.configured_source_language,serde_json::to_string(&event.payload.target_languages)?,seq])?;
    Ok(true)
}
