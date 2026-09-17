use super::*;
use std::collections::HashSet;
fn config() -> AnalysisConfig {
    let mut c = AnalysisConfig {
        model_profile: "summary".into(),
        model_manifest_id: format!("sha256:{}", "a".repeat(64)),
        prompt_version: "v1".into(),
        prompt_sha256: "b".repeat(64),
        profile_id: "vision".into(),
        profile_version: "v1".into(),
        profile_sha256: "d".repeat(64),
        effective_config_hash: String::new(),
        projection_policy_hash: "c".repeat(64),
        generation: serde_json::json!({"temperature":0.0,"max_output_tokens":1024,"safety_tokens":128,"max_retries":1,"context_limit":8192}),
    };
    c.effective_config_hash = c.computed_hash();
    c
}
fn setup(store: &mut Store) -> Fixture {
    let f = Fixture::new();
    f.setup(store);
    store.register_capture(&f.capture).unwrap();
    store.ingest_final(&f.final_event).unwrap();
    f
}
fn request(f: &Fixture, kind: AnalysisKind) -> CreateAnalysisJob {
    CreateAnalysisJob {
        request_id: Uuid::new_v4(),
        session_ids: vec![f.session.session_id],
        kind,
        config: config(),
        budget_ms: 60_000,
        max_attempts: 3,
        automatic: false,
    }
}
fn result(store: &Store, j: &AnalysisJob) -> AnalysisResult {
    let s = store.analysis_snapshot(j.job_id).unwrap().snapshot;
    let (e, target, text) = if let Some(src) = s
        .segments
        .iter()
        .find(|i| i.status == Some(TranscriptStatus::Success))
    {
        (
            AnalysisEvidence::Source {
                session_id: src.session_id,
                span: SourceSpan {
                    segment_id: src.segment_id,
                    segment_revision: src.revision.unwrap(),
                    start_utf8: 0,
                    end_utf8: src.text.len(),
                    quote: src.text.clone(),
                },
            },
            CoverageTarget::Source {
                segment_id: src.segment_id,
                segment_revision: src.revision,
            },
            src.text.clone(),
        )
    } else {
        let a = &s.published_artifacts[0];
        (
            AnalysisEvidence::Artifact {
                artifact_id: a.artifact_id,
                revision: a.revision,
                start_utf8: 0,
                end_utf8: a.text.len(),
                quote: a.text.clone(),
            },
            CoverageTarget::Artifact {
                artifact_id: a.artifact_id,
                revision: a.revision,
            },
            a.text.clone(),
        )
    };
    AnalysisResult {
        schema_version: 1,
        job_id: j.job_id,
        attempt: j.attempt,
        snapshot_id: j.snapshot_id,
        snapshot_sha256: j.snapshot_sha256.clone(),
        effective_config_hash: j.config.effective_config_hash.clone(),
        content: ArtifactContent {
            title: "纪要 <script>alert(1)</script>".into(),
            sections: vec![AnalysisSection {
                heading: "已确认".into(),
                claims: vec![AnalysisClaim {
                    claim_id: Uuid::new_v4(),
                    kind: ClaimKind::Fact,
                    text: "讨论了字幕测试".into(),
                    evidence: vec![e],
                    grounding: GroundingStatus::Cited,
                    assignee: None,
                    due: None,
                }],
            }],
        },
        coverage: AnalysisCoverage {
            units: vec![CoverageUnit {
                target,
                start_utf8: 0,
                end_utf8: text.len(),
                status: CoverageStatus::Processed,
                reason: None,
            }],
        },
    }
}
fn finish(store: &mut Store, f: &Fixture) -> ArtifactRecord {
    let j = store
        .create_analysis_job(&request(f, AnalysisKind::Insight))
        .unwrap();
    store.claim_analysis_job(j.job_id, j.attempt).unwrap();
    store.finish_analysis_job(&result(store, &j)).unwrap()
}
fn publish(store: &mut Store, a: &ArtifactRecord) -> PublicArtifact {
    store
        .review_artifact(&ArtifactReviewCommand {
            artifact_id: a.artifact_id,
            expected_revision: a.revision,
            review: ArtifactReview::Approved,
            operator_id: "operator".into(),
            reason: "核对了原文".into(),
        })
        .unwrap();
    store
        .publish_artifact(&ArtifactPublishCommand {
            artifact_id: a.artifact_id,
            expected_revision: a.revision,
            operator_id: "operator".into(),
            reason: "匿名公开版本".into(),
            policy_hash: a.config.projection_policy_hash.clone(),
            reviewed_title: "测试讨论".into(),
            reviewed_text: "讨论了字幕测试".into(),
            evidence: vec![PublicEvidenceInput {
                evidence: a.content.sections[0].claims[0].evidence[0].clone(),
                reviewed_text: "已核对匿名证据".into(),
            }],
        })
        .unwrap()
}
#[test]
fn create_idempotence_hash_scope_and_stable_snapshot() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let r = request(&f, AnalysisKind::Insight);
    let a = s.create_analysis_job(&r).unwrap();
    assert_eq!(a, s.create_analysis_job(&r).unwrap());
    let mut changed = r.clone();
    changed.budget_ms += 1;
    assert!(matches!(
        s.create_analysis_job(&changed),
        Err(StoreError::EventIdConflict)
    ));
    let b = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    assert_eq!(a.snapshot_id, b.snapshot_id);
    let data = s.analysis_snapshot(a.job_id).unwrap();
    assert_eq!(
        data.sha256,
        format!("{:x}", Sha256::digest(data.compact_json.as_bytes()))
    );
    let mut cross = request(&f, AnalysisKind::EventReport);
    let other = setup(&mut s);
    cross.session_ids.push(other.session.session_id);
    assert!(matches!(
        s.create_analysis_job(&cross),
        Err(StoreError::ScopeMismatch)
    ));
    let mut bad = request(&f, AnalysisKind::Insight);
    bad.config.generation["max_output_tokens"] = 2048.into();
    assert!(s.create_analysis_job(&bad).is_err());
}
#[test]
fn global_claim_cancel_attempt_fence_and_bounded_retry() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    let k = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    assert!(matches!(
        s.claim_analysis_job(k.job_id, 1),
        Err(StoreError::QueueFull)
    ));
    s.cancel_analysis_job(j.job_id, 1).unwrap();
    assert!(s.finish_analysis_job(&result(&s, &j)).is_err());
    assert!(s.claim_analysis_job(k.job_id, 1).is_err());
    s.complete_analysis_cancel(j.job_id, 1).unwrap();
    let retry = s.retry_analysis_job(j.job_id, 1).unwrap();
    assert_eq!(retry.attempt, 2);
    assert!(matches!(
        s.cancel_analysis_job(j.job_id, 1),
        Err(StoreError::LateResult)
    ));
    s.claim_analysis_job(j.job_id, 2).unwrap();
    s.fail_analysis_job(j.job_id, 2, "model failed".into())
        .unwrap();
    s.retry_analysis_job(j.job_id, 2).unwrap();
    s.cancel_analysis_job(j.job_id, 3).unwrap();
    assert!(s.retry_analysis_job(j.job_id, 3).is_err());
    s.claim_analysis_job(k.job_id, 1).unwrap();
}
#[test]
fn source_revision_fences_commit_and_invalidates_public_recursively() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    publish(&mut s, &a);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::EventReport))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let report = s.finish_analysis_job(&result(&s, &j)).unwrap();
    publish(&mut s, &report);
    let pending = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(pending.job_id, 1).unwrap();
    let output = result(&s, &pending);
    s.revise_transcript(&f.revision(Revision::FIRST, "修改后的来源"))
        .unwrap();
    assert!(matches!(
        s.finish_analysis_job(&output),
        Err(StoreError::LateResult)
    ));
    assert_eq!(
        s.artifact(a.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
    assert_eq!(
        s.artifact(report.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
    assert!(s
        .public_snapshot(f.session.session_id)
        .unwrap()
        .artifacts
        .is_empty());
    assert!(s
        .public_changes(f.session.session_id, 0, 100)
        .unwrap()
        .iter()
        .all(|c| c.artifact.is_none() && c.withdrawn));
}
#[test]
fn report_has_only_explicit_published_input_and_hidden_never_replays() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    assert!(s
        .create_analysis_job(&request(&f, AnalysisKind::EventReport))
        .is_err());
    publish(&mut s, &a);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::EventReport))
        .unwrap();
    let snap = s.analysis_snapshot(j.job_id).unwrap().snapshot;
    assert!(snap.segments.is_empty());
    assert_eq!(snap.published_artifacts.len(), 1);
    assert_eq!(snap.published_artifacts[0].text, "讨论了字幕测试");
    s.set_artifact_visibility(&ArtifactVisibilityCommand {
        artifact_id: a.artifact_id,
        expected_revision: a.revision,
        publication: ArtifactPublication::Hidden,
        operator_id: "operator".into(),
        reason: "撤下".into(),
    })
    .unwrap();
    assert!(s
        .create_analysis_job(&request(&f, AnalysisKind::EventReport))
        .is_err());
    assert!(s
        .public_snapshot(f.session.session_id)
        .unwrap()
        .artifacts
        .is_empty());
    assert!(s
        .public_changes(f.session.session_id, 0, 100)
        .unwrap()
        .iter()
        .all(|c| c.artifact.is_none()));
}
#[test]
fn coverage_tail_missing_and_unsupported_claim_remain_internal() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let mut r = result(&s, &j);
    r.coverage.units[0].end_utf8 = 3;
    let a = s.finish_analysis_job(&r).unwrap();
    assert!(!a.coverage_complete);
    assert_eq!(
        s.analysis_job(j.job_id).unwrap().state,
        AnalysisJobState::SucceededPartial
    );
    s.review_artifact(&ArtifactReviewCommand {
        artifact_id: a.artifact_id,
        expected_revision: a.revision,
        review: ArtifactReview::Approved,
        operator_id: "operator".into(),
        reason: "事实有据但部分输入".into(),
    })
    .unwrap();
    let err = s.publish_artifact(&ArtifactPublishCommand {
        artifact_id: a.artifact_id,
        expected_revision: a.revision,
        operator_id: "operator".into(),
        reason: "should reject".into(),
        policy_hash: a.config.projection_policy_hash.clone(),
        reviewed_title: "a".into(),
        reviewed_text: "a".into(),
        evidence: vec![],
    });
    assert!(err.is_err());
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let mut r = result(&s, &j);
    r.content.sections[0].claims[0].grounding = GroundingStatus::Unsupported;
    r.content.sections[0].claims[0].evidence.clear();
    let a = s.finish_analysis_job(&r).unwrap();
    assert_eq!(a.validation, ArtifactValidation::NeedsReview);
    assert!(s
        .review_artifact(&ArtifactReviewCommand {
            artifact_id: a.artifact_id,
            expected_revision: a.revision,
            review: ArtifactReview::Approved,
            operator_id: "operator".into(),
            reason: "cannot override missing evidence".into()
        })
        .is_err());
}
#[test]
fn exact_utf8_evidence_foreign_scope_and_failed_transaction_roll_back() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let r = result(&s, &j);
    let mut bad = r.clone();
    if let AnalysisEvidence::Source { span, .. } =
        &mut bad.content.sections[0].claims[0].evidence[0]
    {
        span.start_utf8 = 1;
    }
    assert!(s.finish_analysis_job(&bad).is_err());
    let mut bad = r.clone();
    if let AnalysisEvidence::Source { session_id, .. } =
        &mut bad.content.sections[0].claims[0].evidence[0]
    {
        *session_id = Uuid::new_v4();
    }
    assert!(matches!(
        s.finish_analysis_job(&bad),
        Err(StoreError::ScopeMismatch)
    ));
    s.connection.execute_batch("CREATE TRIGGER fail_analysis_outbox BEFORE INSERT ON analysis_changes WHEN NEW.entity_kind='artifact' BEGIN SELECT RAISE(FAIL,'injected'); END;").unwrap();
    assert!(s.finish_analysis_job(&r).is_err());
    assert_eq!(
        s.analysis_job(j.job_id).unwrap().state,
        AnalysisJobState::Running
    );
    assert!(s
        .list_artifacts(f.session.session_id, None, 100)
        .unwrap()
        .items
        .is_empty());
    s.connection
        .execute_batch("DROP TRIGGER fail_analysis_outbox")
        .unwrap();
    let a = s.finish_analysis_job(&r).unwrap();
    assert_eq!(s.finish_analysis_job(&r).unwrap(), a);
    let mut conflict = r.clone();
    conflict.content.title = "another".into();
    assert!(matches!(
        s.finish_analysis_job(&conflict),
        Err(StoreError::EventIdConflict)
    ));
}
#[test]
fn human_review_not_overwritten_and_html_is_escaped() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    publish(&mut s, &a);
    let generated = finish(&mut s, &f);
    assert_ne!(a.artifact_id, generated.artifact_id);
    assert_eq!(
        s.artifact(a.artifact_id).unwrap().review,
        ArtifactReview::Approved
    );
    let mut content = a.content.clone();
    content.title = "人工修订".into();
    let edited = s
        .edit_artifact(&ArtifactEdit {
            artifact_id: a.artifact_id,
            expected_revision: a.revision,
            content,
            operator_id: "operator".into(),
            reason: "人工调整".into(),
        })
        .unwrap();
    assert_eq!(edited.revision.get(), 2);
    assert_eq!(edited.review, ArtifactReview::Draft);
    assert!(s
        .public_snapshot(f.session.session_id)
        .unwrap()
        .artifacts
        .is_empty());
    assert_eq!(
        s.artifact_revision(a.artifact_id, Revision::FIRST)
            .unwrap()
            .review,
        ArtifactReview::Approved
    );
    let html = s
        .export_artifact(generated.artifact_id, Revision::FIRST, "html")
        .unwrap();
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;"));
}
#[test]
fn fixed_cursor_pages_do_not_skip_mutating_jobs_or_include_new_rows() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let mut expected = vec![];
    for _ in 0..4 {
        expected.push(
            s.create_analysis_job(&request(&f, AnalysisKind::Insight))
                .unwrap()
                .job_id,
        )
    }
    let first = s.list_analysis_jobs(f.session.session_id, None, 2).unwrap();
    let ids = first.items.iter().map(|j| j.job_id).collect::<Vec<_>>();
    for id in &expected {
        s.cancel_analysis_job(*id, 1).unwrap();
    }
    s.create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    let next = s
        .list_analysis_jobs(f.session.session_id, first.next_after, 2)
        .unwrap();
    assert_eq!(next.cursor, first.cursor);
    assert!(next
        .items
        .iter()
        .all(|j| j.state == AnalysisJobState::Queued));
    let all = ids
        .into_iter()
        .chain(next.items.into_iter().map(|j| j.job_id))
        .collect::<HashSet<_>>();
    assert_eq!(all, expected.into_iter().collect());
    assert!(next.next_after.is_none());
}
#[test]
fn restart_interrupts_active_job_preserves_confirmed_checkpoints() {
    let db = TempDatabase::new();
    let (job, checkpoint) = {
        let mut s = Store::open(db.path()).unwrap();
        let f = setup(&mut s);
        let j = s
            .create_analysis_job(&request(&f, AnalysisKind::Insight))
            .unwrap();
        s.claim_analysis_job(j.job_id, 1).unwrap();
        let result = serde_json::json!({"covered":"unit 1"});
        let p = AnalysisCheckpoint {
            job_id: j.job_id,
            attempt: 1,
            step_index: 0,
            snapshot_sha256: j.snapshot_sha256.clone(),
            input_sha256: "e".repeat(64),
            effective_config_hash: j.config.effective_config_hash.clone(),
            result_sha256: format!("{:x}", Sha256::digest(canonical_json(&result))),
            result,
        };
        s.confirm_analysis_checkpoint(&p).unwrap();
        let mut bad = p.clone();
        bad.result["covered"] = "changed".into();
        assert!(s.confirm_analysis_checkpoint(&bad).is_err());
        (j, p)
    };
    let mut s = Store::open(db.path()).unwrap();
    let interrupted = s.analysis_job(job.job_id).unwrap();
    assert_eq!(interrupted.state, AnalysisJobState::Interrupted);
    assert_eq!(interrupted.deadline_at_ms, job.deadline_at_ms);
    assert_eq!(
        s.analysis_checkpoints(job.job_id).unwrap(),
        vec![checkpoint]
    );
    let retry = s.retry_analysis_job(job.job_id, 1).unwrap();
    assert_eq!(retry.attempt, 2);
}
#[test]
fn auto_insight_coalesces_only_pending_and_queue_budget_is_real() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let mut r = request(&f, AnalysisKind::Insight);
    r.automatic = true;
    let a = s.create_analysis_job(&r).unwrap();
    r.request_id = Uuid::new_v4();
    assert_eq!(s.create_analysis_job(&r).unwrap().job_id, a.job_id);
    s.revise_transcript(&f.revision(Revision::FIRST, "新输入"))
        .unwrap();
    r.request_id = Uuid::new_v4();
    let b = s.create_analysis_job(&r).unwrap();
    assert_eq!(
        s.analysis_job(a.job_id).unwrap().state,
        AnalysisJobState::Cancelled
    );
    s.claim_analysis_job(b.job_id, 1).unwrap();
    r.request_id = Uuid::new_v4();
    assert_eq!(s.create_analysis_job(&r).unwrap().job_id, b.job_id);
    s.revise_transcript(&f.revision(Revision::FIRST.next().unwrap(), "更新的输入"))
        .unwrap();
    r.request_id = Uuid::new_v4();
    let c = s.create_analysis_job(&r).unwrap();
    assert_eq!(
        s.analysis_job(b.job_id).unwrap().state,
        AnalysisJobState::Running
    );
    s.fail_analysis_job(b.job_id, 1, "done".into()).unwrap();
    let mut expired = s.analysis_job(c.job_id).unwrap();
    expired.deadline_at_ms = 0;
    s.connection
        .execute(
            "UPDATE analysis_jobs SET body_json=?2 WHERE id=?1",
            params![
                c.job_id.to_string(),
                serde_json::to_string(&expired).unwrap()
            ],
        )
        .unwrap();
    let expired = s.claim_analysis_job(c.job_id, 1).unwrap();
    assert_eq!(expired.state, AnalysisJobState::Failed);
    assert_eq!(expired.error.as_deref(), Some("budget_exhausted_in_queue"));
}

#[test]
fn legacy_import_is_atomic_idempotent_and_never_claims_recording_completeness() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    let text="{\"t_start\":0.25,\"t_end\":1.75,\"speaker_id\":\"S1\",\"lang\":\"zh\",\"text\":\"测试旧数据📝\"}\n".to_owned();
    let first = s
        .import_legacy_transcript(f.session.clone(), text.clone())
        .unwrap();
    assert_eq!(first.segment_count, 1);
    assert!(first.session.title.starts_with("[旧数据]"));
    assert!(first.status.incomplete);
    let mut again = f.session.clone();
    again.session_id = Uuid::new_v4();
    assert_eq!(
        s.import_legacy_transcript(again, text.clone())
            .unwrap()
            .session
            .session_id,
        first.session.session_id
    );
    let snapshot = s
        .snapshot_page(first.session.session_id, None, None, 100)
        .unwrap();
    let row = &snapshot.items[0];
    assert!(row.recording_ref.is_none());
    assert_eq!(row.audio.start_ms, 250);
    assert_eq!(row.audio.end_ms, 1750);
    assert!(
        matches!(&row.transcript.as_ref().unwrap().origin,RevisionOrigin::LegacyImport{line:1,speaker_label:Some(label),..}if label=="S1")
    );
    assert_eq!(
        s.connection
            .query_row::<u64, _, _>("SELECT COUNT(*) FROM capture_seals", [], |r| r.get(0))
            .unwrap(),
        0
    );
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Minutes))
        .unwrap();
    assert!(
        !s.analysis_snapshot(j.job_id)
            .unwrap()
            .snapshot
            .input_complete
    );
    let bad =
        format!("{text}{{\"speaker_id\":\"S2\",\"lang\":\"en\",\"text\":\"missing time\"}}\n");
    let fresh = Fixture::new();
    assert!(s
        .import_legacy_transcript(fresh.session.clone(), bad)
        .is_err());
    assert!(s.session_status(fresh.session.session_id).is_err());
}
#[test]
fn legacy_import_real_sql_failure_rolls_back_all_rows() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    let text = "{\"t_start\":0,\"t_end\":1,\"speaker_id\":1,\"lang\":\"en\",\"text\":\"hello\"}\n"
        .to_owned();
    s.connection.execute_batch("CREATE TRIGGER fail_import BEFORE INSERT ON legacy_import_rows BEGIN SELECT RAISE(FAIL,'injected'); END").unwrap();
    assert!(s
        .import_legacy_transcript(f.session.clone(), text.clone())
        .is_err());
    assert!(s.list_sessions(100).unwrap().is_empty());
    assert!(s.outbox_after(0, 100).unwrap().is_empty());
    s.connection
        .execute_batch("DROP TRIGGER fail_import")
        .unwrap();
    assert_eq!(
        s.import_legacy_transcript(f.session, text)
            .unwrap()
            .segment_count,
        1
    );
}
#[test]
fn recovered_asr_revision_invalidates_partial_artifact_and_pending_commit() {
    let mut s = Store::in_memory().unwrap();
    let mut f = Fixture::new();
    f.setup(&mut s);
    s.register_capture(&f.capture).unwrap();
    f.final_event.payload.text.clear();
    f.final_event.payload.status = TranscriptStatus::Failed;
    f.final_event.payload.reason = Some("ASR unavailable".into());
    s.ingest_final(&f.final_event).unwrap();
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let output = AnalysisResult {
        schema_version: 1,
        job_id: j.job_id,
        attempt: 1,
        snapshot_id: j.snapshot_id,
        snapshot_sha256: j.snapshot_sha256.clone(),
        effective_config_hash: j.config.effective_config_hash.clone(),
        content: ArtifactContent {
            title: "输入尚缺".into(),
            sections: vec![AnalysisSection {
                heading: "限制".into(),
                claims: vec![AnalysisClaim {
                    claim_id: Uuid::new_v4(),
                    kind: ClaimKind::Uncertainty,
                    text: "来源识别失败".into(),
                    evidence: vec![],
                    grounding: GroundingStatus::Unsupported,
                    assignee: None,
                    due: None,
                }],
            }],
        },
        coverage: AnalysisCoverage {
            units: vec![CoverageUnit {
                target: CoverageTarget::Source {
                    segment_id: f.capture.payload.segment_id,
                    segment_revision: Some(Revision::FIRST),
                },
                start_utf8: 0,
                end_utf8: 0,
                status: CoverageStatus::Failed,
                reason: Some("ASR unavailable".into()),
            }],
        },
    };
    let a = s.finish_analysis_job(&output).unwrap();
    assert!(!a.coverage_complete);
    let pending = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(pending.job_id, 1).unwrap();
    let mut repaired = f.final_event.clone();
    repaired.message_id = Uuid::new_v4();
    repaired.producer.run_id = Uuid::new_v4();
    repaired.payload.revision = Revision::FIRST.next().unwrap();
    repaired.payload.text = "恢复后的原文".into();
    repaired.payload.status = TranscriptStatus::Success;
    repaired.payload.reason = None;
    s.ingest_final(&repaired).unwrap();
    assert_eq!(
        s.artifact(a.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
    let mut late = output;
    late.job_id = pending.job_id;
    late.snapshot_id = pending.snapshot_id;
    late.snapshot_sha256 = pending.snapshot_sha256;
    assert!(matches!(
        s.finish_analysis_job(&late),
        Err(StoreError::LateResult)
    ));
}
#[test]
fn publication_projects_only_reviewed_strings_and_republish_is_idempotent() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    let p = publish(&mut s, &a);
    let public = serde_json::to_string(&s.public_snapshot(f.session.session_id).unwrap()).unwrap();
    assert!(!public.contains(&f.session.session_id.to_string()));
    assert!(!public.contains(&f.capture.payload.segment_id.to_string()));
    assert!(!public.contains(&f.final_event.payload.text));
    assert!(!public.contains("script"));
    let command = ArtifactPublishCommand {
        artifact_id: a.artifact_id,
        expected_revision: a.revision,
        operator_id: "operator".into(),
        reason: "匿名公开版本".into(),
        policy_hash: a.config.projection_policy_hash.clone(),
        reviewed_title: "测试讨论".into(),
        reviewed_text: "讨论了字幕测试".into(),
        evidence: vec![PublicEvidenceInput {
            evidence: a.content.sections[0].claims[0].evidence[0].clone(),
            reviewed_text: "已核对匿名证据".into(),
        }],
    };
    assert_eq!(s.publish_artifact(&command).unwrap(), p);
    let report = s
        .create_analysis_job(&request(&f, AnalysisKind::EventReport))
        .unwrap();
    s.claim_analysis_job(report.job_id, 1).unwrap();
    let r = s.finish_analysis_job(&result(&s, &report)).unwrap();
    let mut changed = command;
    changed.reviewed_text = "重新审校的公开表述".into();
    assert!(s.publish_artifact(&changed).is_err());
    let edited = s
        .edit_artifact(&ArtifactEdit {
            artifact_id: a.artifact_id,
            expected_revision: a.revision,
            content: a.content.clone(),
            operator_id: "operator".into(),
            reason: "new public redaction revision".into(),
        })
        .unwrap();
    publish(&mut s, &edited);
    assert_eq!(
        s.artifact(r.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
}
#[test]
fn insight_window_is_full_segments_and_does_not_claim_gap_free_audio() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut s);
    s.register_capture(&f.capture).unwrap();
    let mut capture = f.capture.clone();
    capture.message_id = Uuid::new_v4();
    capture.producer.run_id = Uuid::new_v4();
    capture.payload.segment_id = Uuid::new_v4();
    capture.payload.audio = AudioRange {
        start_sample: 16_000_000,
        end_sample: 16_080_000,
        sample_rate: 16_000,
        start_ms: 1_000_000,
        end_ms: 1_005_000,
    };
    s.register_capture(&capture).unwrap();
    let mut final_event = f.final_event.clone();
    final_event.message_id = Uuid::new_v4();
    final_event.payload.segment_id = capture.payload.segment_id;
    final_event.payload.audio = capture.payload.audio.clone();
    s.ingest_final(&final_event).unwrap();
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    let snapshot = s.analysis_snapshot(j.job_id).unwrap().snapshot;
    assert_eq!(snapshot.segments.len(), 1);
    assert_eq!(snapshot.segments[0].segment_id, capture.payload.segment_id);
    assert!(snapshot.input_complete);
    let gap = Event {
        schema_version: 1,
        message_id: Uuid::new_v4(),
        event_type: EventType::AudioGap,
        event_id: f.session.event_id,
        room_id: f.session.room_id,
        session_id: f.session.session_id,
        producer: Producer {
            name: "capture".into(),
            run_id: Uuid::new_v4(),
            seq: 1,
        },
        payload: AudioGap {
            gap_id: Uuid::new_v4(),
            track_id: f.track.track_id,
            audio: capture.payload.audio,
            reason: "overflow".into(),
            recoverable: false,
        },
    };
    s.register_gap(&gap).unwrap();
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    assert!(
        !s.analysis_snapshot(j.job_id)
            .unwrap()
            .snapshot
            .input_complete
    );
}
#[test]
fn session_catalog_pages_exclude_new_rows_without_losing_old_sessions() {
    let mut s = Store::in_memory().unwrap();
    let mut original = HashSet::new();
    for _ in 0..5 {
        let f = Fixture::new();
        s.create_session(&f.session).unwrap();
        original.insert(f.session.session_id);
    }
    let (mut first, next) = s.list_sessions_page(None, 2).unwrap();
    let new = Fixture::new();
    s.create_session(&new.session).unwrap();
    let (second, next) = s.list_sessions_page(next, 2).unwrap();
    first.extend(second);
    let (third, next) = s.list_sessions_page(next, 2).unwrap();
    first.extend(third);
    assert!(next.is_none());
    assert_eq!(
        first
            .into_iter()
            .map(|s| s.session.session_id)
            .collect::<HashSet<_>>(),
        original
    );
}
#[test]
fn confirmed_chunk_reuse_is_scoped_and_keeps_original_provenance() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Minutes))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let body = serde_json::json!({"claims":[]});
    let checkpoint = AnalysisCheckpoint {
        job_id: j.job_id,
        attempt: 1,
        step_index: 0,
        snapshot_sha256: j.snapshot_sha256.clone(),
        input_sha256: "e".repeat(64),
        effective_config_hash: j.config.effective_config_hash.clone(),
        result_sha256: format!("{:x}", Sha256::digest(canonical_json(&body))),
        result: body,
    };
    s.confirm_analysis_checkpoint(&checkpoint).unwrap();
    s.fail_analysis_job(j.job_id, 1, "stop".into()).unwrap();
    s.revise_transcript(&f.revision(Revision::FIRST, "new source gives new snapshot"))
        .unwrap();
    let next = s
        .create_analysis_job(&request(&f, AnalysisKind::Minutes))
        .unwrap();
    assert_ne!(next.snapshot_id, j.snapshot_id);
    assert_eq!(
        s.reusable_analysis_checkpoints(next.job_id).unwrap(),
        vec![checkpoint]
    );
    let otherkind = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    assert!(s
        .reusable_analysis_checkpoints(otherkind.job_id)
        .unwrap()
        .is_empty());
    let other = setup(&mut s);
    let otherjob = s
        .create_analysis_job(&request(&other, AnalysisKind::Minutes))
        .unwrap();
    assert!(s
        .reusable_analysis_checkpoints(otherjob.job_id)
        .unwrap()
        .is_empty());
    let mut changed = request(&f, AnalysisKind::Minutes);
    changed.config.generation["context_limit"] = 4096.into();
    changed.config.effective_config_hash = changed.config.computed_hash();
    let configjob = s.create_analysis_job(&changed).unwrap();
    assert!(s
        .reusable_analysis_checkpoints(configjob.job_id)
        .unwrap()
        .is_empty());
}
#[test]
fn late_audio_gap_fences_finish_and_withdraws_previously_complete_artifact() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    publish(&mut s, &a);
    let j = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let output = result(&s, &j);
    let gap = Event {
        schema_version: 1,
        message_id: Uuid::new_v4(),
        event_type: EventType::AudioGap,
        event_id: f.session.event_id,
        room_id: f.session.room_id,
        session_id: f.session.session_id,
        producer: Producer {
            name: "capture".into(),
            run_id: Uuid::new_v4(),
            seq: 1,
        },
        payload: AudioGap {
            gap_id: Uuid::new_v4(),
            track_id: f.track.track_id,
            audio: f.capture.payload.audio.clone(),
            reason: "late overflow".into(),
            recoverable: false,
        },
    };
    s.register_gap(&gap).unwrap();
    assert!(matches!(
        s.finish_analysis_job(&output),
        Err(StoreError::LateResult)
    ));
    assert_eq!(
        s.artifact(a.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
    assert!(s
        .public_snapshot(f.session.session_id)
        .unwrap()
        .artifacts
        .is_empty());
}

#[test]
fn analysis_stop_intent_survives_restart_without_model_or_job() {
    let db = TempDatabase::new();
    let f = Fixture::new();
    let marker = {
        let mut s = Store::open(db.path()).unwrap();
        s.create_session(&f.session).unwrap();
        s.record_analysis_stop_intent(f.session.session_id).unwrap();
        assert!(s.next_analysis_jobs(100).unwrap().is_empty());
        s.pending_analysis_stop_intents(10).unwrap()[0].1
    };
    let mut s = Store::open(db.path()).unwrap();
    assert_eq!(
        s.pending_analysis_stop_intents(10).unwrap(),
        vec![(f.session.session_id, marker)]
    );
    s.ack_analysis_stop_intent(f.session.session_id, marker)
        .unwrap();
    s.ack_analysis_stop_intent(f.session.session_id, marker)
        .unwrap();
    assert!(s.pending_analysis_stop_intents(10).unwrap().is_empty());
    s.record_analysis_stop_intent(f.session.session_id).unwrap();
    assert!(s.pending_analysis_stop_intents(10).unwrap()[0].1 > marker);
}

#[test]
fn old_stop_ack_cannot_delete_new_stop_or_another_session() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    let other = Fixture::new();
    s.create_session(&f.session).unwrap();
    s.create_session(&other.session).unwrap();
    s.record_analysis_stop_intent(f.session.session_id).unwrap();
    let first = s.pending_analysis_stop_intents(10).unwrap()[0].1;
    s.record_analysis_stop_intent(other.session.session_id)
        .unwrap();
    s.record_analysis_stop_intent(f.session.session_id).unwrap();
    let pending = s.pending_analysis_stop_intents(10).unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].0, other.session.session_id);
    assert_eq!(
        s.pending_analysis_stop_intents(1).unwrap(),
        vec![pending[0]]
    );
    assert!(pending[1].1 > first);
    s.ack_analysis_stop_intent(f.session.session_id, first)
        .unwrap();
    s.ack_analysis_stop_intent(other.session.session_id, pending[1].1)
        .unwrap();
    assert_eq!(s.pending_analysis_stop_intents(10).unwrap(), pending);
    assert!(s.record_analysis_stop_intent(Uuid::new_v4()).is_err());
    assert!(s.pending_analysis_stop_intents(0).is_err());
    assert!(s.pending_analysis_stop_intents(1001).is_err());
    assert!(s.ack_analysis_stop_intent(f.session.session_id, 0).is_err());
}

#[test]
fn stop_intent_replacement_sql_failure_preserves_previous_marker() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    s.create_session(&f.session).unwrap();
    s.record_analysis_stop_intent(f.session.session_id).unwrap();
    let prior = s.pending_analysis_stop_intents(10).unwrap();
    s.connection.execute_batch("CREATE TRIGGER fail_stop_intent BEFORE INSERT ON analysis_stop_intents BEGIN SELECT RAISE(FAIL,'injected'); END").unwrap();
    assert!(s.record_analysis_stop_intent(f.session.session_id).is_err());
    assert_eq!(s.pending_analysis_stop_intents(10).unwrap(), prior);
    s.connection
        .execute_batch("DROP TRIGGER fail_stop_intent")
        .unwrap();
    s.record_analysis_stop_intent(f.session.session_id).unwrap();
    assert!(s.pending_analysis_stop_intents(10).unwrap()[0].1 > prior[0].1);
}

#[test]
fn successful_finish_closes_progress_but_partial_keeps_reported_counters() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    for (partial, reported_total) in [(false, 1), (false, 0), (true, 1)] {
        let job = s
            .create_analysis_job(&request(&f, AnalysisKind::Insight))
            .unwrap();
        s.claim_analysis_job(job.job_id, 1).unwrap();
        s.update_analysis_progress(
            job.job_id,
            1,
            AnalysisProgress {
                phase: "map".into(),
                completed_units: 0,
                total_units: reported_total,
                wait_reason: None,
            },
        )
        .unwrap();
        let mut output = result(&s, &job);
        if partial {
            output.coverage.units[0].end_utf8 = 3;
        }
        s.finish_analysis_job(&output).unwrap();
        let finished = s.analysis_job(job.job_id).unwrap();
        assert_eq!(finished.progress.phase, "finished");
        assert_eq!(finished.progress.total_units, 1);
        if partial {
            assert_eq!(finished.state, AnalysisJobState::SucceededPartial);
            assert_eq!(finished.progress.completed_units, 0);
        } else {
            assert_eq!(finished.state, AnalysisJobState::Succeeded);
            assert_eq!(finished.progress.completed_units, 1);
        }
    }
}

#[path = "forum_tests.rs"]
mod forum_tests;
