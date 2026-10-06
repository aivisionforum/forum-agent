// Included by analysis.rs: shares transaction helpers, not a second publication path.
fn insight_settings(c: &Connection, session: Uuid) -> Result<InsightSettings> {
    require_session(c, session)?;
    let mode: Option<String> = c
        .query_row(
            "SELECT mode FROM insight_settings WHERE session_id=?1",
            [session.to_string()],
            |r| r.get(0),
        )
        .optional()?;
    Ok(InsightSettings {
        mode: if mode.as_deref() == Some("automatic") {
            InsightApprovalMode::Automatic
        } else {
            InsightApprovalMode::Gated
        },
    })
}

impl Store {
    pub fn insight_settings(&self, session: Uuid) -> Result<InsightSettings> {
        insight_settings(&self.connection, session)
    }
    pub fn set_insight_settings(&mut self, cmd: &SetInsightSettings) -> Result<InsightSettings> {
        operator(&cmd.operator_id, &cmd.reason)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_session(&tx, cmd.session_id)?;
        let mode = match cmd.mode {
            InsightApprovalMode::Gated => "gated",
            InsightApprovalMode::Automatic => "automatic",
        };
        tx.execute("INSERT INTO insight_settings(session_id,mode) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET mode=excluded.mode", params![cmd.session_id.to_string(),mode])?;
        tx.execute("INSERT INTO insight_settings_audit(session_id,changed_at_ms,command_json) VALUES(?1,?2,?3)", params![cmd.session_id.to_string(),now_ms(),json(cmd)?])?;
        tx.commit()?;
        Ok(InsightSettings { mode: cmd.mode })
    }
    /// Called by the capture scheduler on start and at each three-minute tick.
    pub fn advance_insight_schedule(&mut self, session: Uuid) -> Result<()> {
        require_session(&self.connection, session)?;
        self.connection.execute("INSERT INTO insight_schedule(session_id,next_update_at_ms) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET next_update_at_ms=excluded.next_update_at_ms,blocked=0", params![session.to_string(), now_ms() + INSIGHT_INTERVAL_MS])?;
        Ok(())
    }
    pub fn mark_insight_schedule_blocked(&mut self, session: Uuid) -> Result<()> {
        self.connection.execute(
            "UPDATE insight_schedule SET blocked=1 WHERE session_id=?1",
            [session.to_string()],
        )?;
        Ok(())
    }
}

fn wall_status(c: &Connection, session: Uuid) -> Result<PublicWallStatus> {
    let session_state: String = c.query_row(
        "SELECT state FROM sessions WHERE id=?1",
        [session.to_string()],
        |r| r.get(0),
    )?;
    let latest: Option<String> = c.query_row("SELECT state FROM analysis_jobs WHERE json_extract(body_json,'$.kind')='insight' AND EXISTS(SELECT 1 FROM json_each(body_json,'$.session_ids') WHERE value=?1) ORDER BY (state IN ('queued','waiting','running','cancel_requested')) DESC,created_at_ms DESC,rowid DESC LIMIT 1", [session.to_string()], |r| r.get(0)).optional()?;
    let ended = matches!(
        session_state.as_str(),
        "completed" | "interrupted" | "stopping" | "draining"
    );
    let blocked: bool = c
        .query_row(
            "SELECT blocked FROM insight_schedule WHERE session_id=?1",
            [session.to_string()],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    let phase = match latest.as_deref() {
        Some("running" | "queued") => WallPhase::Working,
        Some("waiting" | "cancel_requested") => WallPhase::Paused,
        _ if ended => WallPhase::Finished,
        _ if blocked => WallPhase::Delayed,
        Some("failed" | "interrupted" | "succeeded_partial") => WallPhase::Delayed,
        _ => WallPhase::Listening,
    };
    let next_update_at_ms = if ended {
        None
    } else {
        c.query_row(
            "SELECT next_update_at_ms FROM insight_schedule WHERE session_id=?1",
            [session.to_string()],
            |r| r.get(0),
        )
        .optional()?
    };
    Ok(PublicWallStatus {
        phase,
        next_update_at_ms,
        server_time_ms: now_ms(),
    })
}

fn anonymous_label(c: &Connection, session: Uuid, speaker: Uuid) -> Result<String> {
    c.execute("INSERT OR IGNORE INTO public_speaker_labels(session_id,speaker_id,ordinal) SELECT ?1,?2,COALESCE(MAX(ordinal),0)+1 FROM public_speaker_labels WHERE session_id=?1", params![session.to_string(),speaker.to_string()])?;
    let mut ordinal: u32 = c.query_row(
        "SELECT ordinal FROM public_speaker_labels WHERE session_id=?1 AND speaker_id=?2",
        params![session.to_string(), speaker.to_string()],
        |r| r.get(0),
    )?;
    let mut suffix = Vec::new();
    while ordinal > 0 {
        ordinal -= 1;
        suffix.push((b'A' + (ordinal % 26) as u8) as char);
        ordinal /= 26;
    }
    Ok(format!(
        "发言人{}",
        suffix.into_iter().rev().collect::<String>()
    ))
}

fn auto_publish_insight(
    c: &Connection,
    a: &mut ArtifactRecord,
    snapshot: &AnalysisSnapshot,
) -> Result<()> {
    if a.kind != AnalysisKind::Insight
        || a.session_ids.len() != 1
        || a.validation != ArtifactValidation::Valid
        || !publication_coverage_ready(a)
        || insight_settings(c, a.session_ids[0])?.mode != InsightApprovalMode::Automatic
    {
        return Ok(());
    }
    let Some((text, evidence)) = insight_public_copy(c, a, snapshot)? else { return Ok(()); };
    let reason = "操作员已为本场启用自动批准；引用检查通过，现场可纠错或隐藏".to_string();
    a.review = ArtifactReview::Approved;
    a.operator_id = Some("session-auto-approval".into());
    a.reason = Some(reason.clone());
    save_artifact(c, a)?;
    publish_in_transaction(
        c,
        &ArtifactPublishCommand {
            artifact_id: a.artifact_id,
            expected_revision: a.revision,
            operator_id: "session-auto-approval".into(),
            reason,
            policy_hash: a.config.projection_policy_hash.clone(),
            reviewed_title: "讨论要点".into(),
            reviewed_text: text,
            evidence,
        },
    )?;
    *a = load_artifact(c, a.artifact_id)?;
    Ok(())
}

// A live insight is a set of cited points, not a claim to cover the whole
// meeting. Preserve its partial-coverage flag while allowing those points to
// be reviewed. Minutes, reports and closing briefs still require full coverage.
fn publication_coverage_ready(a: &ArtifactRecord) -> bool {
    if a.coverage_complete { return true; }
    let claims: Vec<_> = a.content.sections.iter().flat_map(|s| &s.claims).collect();
    a.kind == AnalysisKind::Insight && a.session_ids.len() == 1
        && !claims.is_empty()
        && claims.iter().all(|c| c.grounding == GroundingStatus::Cited && !c.evidence.is_empty())
}

// Shared anonymous proposal for automatic publication and the operator's review
// form. Preparing it never approves or exposes an artifact.
fn insight_public_copy(c: &Connection, a: &ArtifactRecord, snapshot: &AnalysisSnapshot)
    -> Result<Option<(String, Vec<PublicEvidenceInput>)>> {
    let mut lines = Vec::new();
    let mut evidence = Vec::new();
    for claim in a.content.sections.iter().flat_map(|s| &s.claims) {
        if claim.grounding != GroundingStatus::Cited || claim.evidence.is_empty() {
            return Ok(None);
        }
        let speakers: HashSet<Uuid> = claim
            .evidence
            .iter()
            .filter_map(|e| match e {
                AnalysisEvidence::Source { span, .. } => snapshot
                    .segments
                    .iter()
                    .find(|s| s.segment_id == span.segment_id)
                    .and_then(|s| s.speaker_id),
                _ => None,
            })
            .collect();
        // Multiple/unknown speakers do not imply a single person or a consensus.
        let all_known = claim.evidence.iter().all(|e| match e {
            AnalysisEvidence::Source { span, .. } => snapshot
                .segments
                .iter()
                .any(|s| s.segment_id == span.segment_id && s.speaker_id.is_some()),
            _ => false,
        });
        let label = if all_known && speakers.len() == 1 {
            Some(anonymous_label(
                c,
                a.session_ids[0],
                *speakers.iter().next().unwrap(),
            )?)
        } else {
            None
        };
        let mut text = claim.text.clone();
        // Assignee metadata is private. Attribution is derived from cited source,
        // never from a guessed model identity. Free-text anonymity also relies on
        // the insight prompt and the operator's live corrections in automatic mode.
        if let Some(name) = claim.assignee.as_ref().filter(|s| !s.trim().is_empty()) {
            text = text.replace(name, "相关发言人");
        }
        for source in &snapshot.segments {
            if let Some(id) = source.speaker_id {
                if text.contains(&id.to_string()) {
                    text =
                        text.replace(&id.to_string(), &anonymous_label(c, a.session_ids[0], id)?);
                }
            }
        }
        lines.push(match &label {
            Some(label) => format!("{label}：{text}"),
            None => text,
        });
        for source in &claim.evidence {
            if !evidence
                .iter()
                .any(|e: &PublicEvidenceInput| &e.evidence == source)
            {
                evidence.push(PublicEvidenceInput {
                    evidence: source.clone(),
                    // Raw quotes may contain introductions/names. Keep the exact
                    // quote private; this public description is not a quotation.
                    reviewed_text: label
                        .as_ref()
                        .map(|label| format!("依据{label}的本场发言整理。"))
                        .unwrap_or_else(|| "依据本场讨论整理。".into()),
                });
            }
        }
    }
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some((lines.join("\n\n"), evidence)))
}

impl Store {
    pub fn prepare_insight_publication(&mut self, id: Uuid, revision: Revision) -> Result<ArtifactPublishCommand> {
        let tx = self.connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let artifact = load_artifact(&tx, id)?;
        expected_revision(&artifact, revision)?;
        if artifact.kind != AnalysisKind::Insight || artifact.session_ids.len() != 1
            || artifact.validation != ArtifactValidation::Valid || !publication_coverage_ready(&artifact) {
            return Err(invalid("artifact_publication_policy"));
        }
        let snapshot = load_snapshot(&tx, artifact.snapshot_id)?.snapshot;
        if !snapshot_current(&tx, &snapshot)? { return Err(StoreError::LateResult); }
        // Keep the exact reviewed copy when viewing or restoring a publication.
        // A changed copy needs a new artifact revision, just as in publishing.
        let previous: Option<String> = tx.query_row(
            "SELECT r.command_json FROM publication_projection p JOIN publication_reviews r ON r.publication_seq=json_extract(p.body_json,'$.publication_seq') WHERE p.artifact_id=?1 AND json_extract(p.body_json,'$.revision')=?2",
            params![id.to_string(), revision.get()], |r| r.get(0),
        ).optional()?;
        if let Some(body) = previous {
            let mut proposal: ArtifactPublishCommand = serde_json::from_str(&body)?;
            proposal.operator_id = "local-operator".into();
            proposal.reason = "操作员核对公开正文与匿名依据后批准上墙".into();
            tx.commit()?;
            return Ok(proposal);
        }
        let (text, evidence) = insight_public_copy(&tx, &artifact, &snapshot)?
            .ok_or_else(|| invalid("insight_requires_reviewed_public_copy"))?;
        let proposal = ArtifactPublishCommand {
            artifact_id: id, expected_revision: revision,
            operator_id: "local-operator".into(), reason: "操作员核对公开正文与匿名依据后批准上墙".into(),
            policy_hash: artifact.config.projection_policy_hash,
            reviewed_title: "讨论要点".into(), reviewed_text: text, evidence,
        };
        tx.commit()?;
        Ok(proposal)
    }
}
