use super::*;
fn origin() -> SpeakerAssignmentOrigin {
    SpeakerAssignmentOrigin::Automatic {
        model_manifest_id: format!("sha256:{}", "f".repeat(64)),
        model_version: "ecapa-v1".into(),
    }
}
fn assign(f: &Fixture, label: SpeakerLabel) -> SpeakerAssignmentCommand {
    SpeakerAssignmentCommand {
        request_id: Uuid::new_v4(),
        session_id: f.session.session_id,
        segment_id: f.capture.payload.segment_id,
        source_revision: Revision::FIRST,
        expected_revision: None,
        label,
        origin: origin(),
    }
}
fn peer_fixture(event: Uuid) -> PeerSession {
    PeerSession {
        owner_device_id: Uuid::new_v4(),
        event_id: event,
        session_id: Uuid::new_v4(),
        title: "公开 B 会场".into(),
        room_name: "B".into(),
    }
}
fn public() -> PublicArtifact {
    PublicArtifact {
        public_id: Uuid::new_v4(),
        revision: Revision::FIRST,
        kind: AnalysisKind::Minutes,
        title: "已审核纪要".into(),
        text: "Budget 120000 元，没有批准全面发布。📝".into(),
        evidence: vec![PublicEvidence {
            public_evidence_id: Uuid::new_v4(),
            revision: Revision::FIRST,
            text: "审核后公开证据：先做两个试点。".into(),
        }],
        publication_seq: 1,
    }
}
fn snapshot(
    p: &PeerSession,
    cursor: u64,
    artifacts: Vec<PublicArtifact>,
) -> PeerPublicationSnapshot {
    PeerPublicationSnapshot {
        owner_device_id: p.owner_device_id,
        event_id: p.event_id,
        session_id: p.session_id,
        cursor,
        artifacts,
    }
}
fn embedding(f: &Fixture) -> SpeakerEmbeddingCommand {
    let mut e = vec![0.0; 192];
    e[0] = 1.0;
    SpeakerEmbeddingCommand {
        request_id: Uuid::new_v4(),
        session_id: f.session.session_id,
        segment_id: f.capture.payload.segment_id,
        source_revision: Revision::FIRST,
        expected_revision: None,
        model_manifest_id: format!("sha256:{}", "f".repeat(64)),
        model_version: "ecapa-v1".into(),
        pcm_sha256: "a".repeat(64),
        embedding: e,
    }
}
fn another_segment(s: &mut Store, f: &Fixture) -> Fixture {
    let mut g = Fixture::new();
    g.session = f.session.clone();
    g.track = f.track.clone();
    g.capture = f.capture.clone();
    g.capture.message_id = Uuid::new_v4();
    g.capture.producer.seq += 1;
    g.capture.payload.segment_id = Uuid::new_v4();
    g.capture.payload.audio.start_sample += 80000;
    g.capture.payload.audio.end_sample += 80000;
    g.capture.payload.audio.start_ms += 5000;
    g.capture.payload.audio.end_ms += 5000;
    g.final_event = f.final_event.clone();
    g.final_event.message_id = Uuid::new_v4();
    g.final_event.producer.seq += 1;
    g.final_event.payload.segment_id = g.capture.payload.segment_id;
    g.final_event.payload.audio = g.capture.payload.audio.clone();
    s.register_capture(&g.capture).unwrap();
    s.ingest_final(&g.final_event).unwrap();
    g
}
#[test]
fn speaker_versions_preserve_source_and_lock_human_edits() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let original = s.session_snapshot(f.session.session_id).unwrap();
    let cmd = assign(&f, SpeakerLabel::Unknown);
    let a = s.apply_speaker_assignment(&cmd).unwrap();
    assert_eq!(a, s.apply_speaker_assignment(&cmd).unwrap());
    let mut conflict = cmd.clone();
    conflict.label = SpeakerLabel::Overlap;
    assert!(matches!(
        s.apply_speaker_assignment(&conflict),
        Err(StoreError::EventIdConflict)
    ));
    let mut human = assign(
        &f,
        SpeakerLabel::Anonymous {
            speaker_id: Uuid::new_v4(),
        },
    );
    human.expected_revision = Some(a.revision);
    human.origin = SpeakerAssignmentOrigin::Human {
        operator_id: "operator".into(),
        reason: "人工确认匿名标签".into(),
    };
    let h = s.apply_speaker_assignment(&human).unwrap();
    assert_eq!(h.revision.get(), 2);
    let mut late = assign(&f, SpeakerLabel::Overlap);
    late.expected_revision = Some(h.revision);
    assert!(s.apply_speaker_assignment(&late).is_err());
    assert_eq!(original, s.session_snapshot(f.session.session_id).unwrap());
    assert_eq!(
        s.speaker_assignment_history(f.session.session_id, f.capture.payload.segment_id)
            .unwrap()
            .len(),
        2
    );
    s.revise_transcript(&f.revision(Revision::FIRST, "修订原文"))
        .unwrap();
    assert!(
        !s.speaker_assignment(f.session.session_id, f.capture.payload.segment_id)
            .unwrap()
            .unwrap()
            .current
    );
    human.request_id = Uuid::new_v4();
    human.expected_revision = Some(h.revision);
    assert!(matches!(
        s.apply_speaker_assignment(&human),
        Err(StoreError::LateResult)
    ));
}
#[test]
fn speaker_clusters_are_session_model_scoped_and_ambiguous_remains_unknown() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let cmd = embedding(&f);
    let first = s.apply_speaker_embedding(&cmd).unwrap();
    assert!(matches!(first.label, SpeakerLabel::Anonymous { .. }));
    assert_eq!(first, s.apply_speaker_embedding(&cmd).unwrap());
    let second = another_segment(&mut s, &f);
    let second_a = s.apply_speaker_embedding(&embedding(&second)).unwrap();
    assert_eq!(first.label, second_a.label);
    let third = another_segment(&mut s, &second);
    let mut uncertain = embedding(&third);
    uncertain.embedding[0] = 0.7;
    uncertain.embedding[1] = (1.0f32 - 0.49).sqrt();
    assert_eq!(
        s.apply_speaker_embedding(&uncertain).unwrap().label,
        SpeakerLabel::Unknown
    );
    let other = setup(&mut s);
    let different = s.apply_speaker_embedding(&embedding(&other)).unwrap();
    assert_ne!(first.label, different.label);
    let mut invalid = assign(&other, first.label);
    invalid.expected_revision = Some(different.revision);
    invalid.origin = SpeakerAssignmentOrigin::Human {
        operator_id: "op".into(),
        reason: "cross session must fail".into(),
    };
    assert!(matches!(
        s.apply_speaker_assignment(&invalid),
        Err(StoreError::ScopeMismatch)
    ));
    let mut bad = embedding(&third);
    bad.embedding[0] = f32::NAN;
    assert!(s.apply_speaker_embedding(&bad).is_err());
    bad.embedding = vec![1.0; 192];
    assert!(s.apply_speaker_embedding(&bad).is_err());
}
#[test]
fn speaker_assignment_rolls_back_cluster_and_request_on_failure() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let mut cmd = embedding(&f);
    cmd.source_revision = Revision::try_from(2).unwrap();
    assert!(s.apply_speaker_embedding(&cmd).is_err());
    assert_eq!(
        s.connection
            .query_row("SELECT COUNT(*) FROM speaker_clusters", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    s.connection.execute_batch("CREATE TRIGGER fail_speaker BEFORE INSERT ON speaker_assignment_revisions BEGIN SELECT RAISE(ABORT,'failure'); END").unwrap();
    assert!(s.apply_speaker_embedding(&embedding(&f)).is_err());
    assert_eq!(
        s.connection
            .query_row("SELECT COUNT(*) FROM speaker_assignments", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        s.connection
            .query_row("SELECT COUNT(*) FROM speaker_clusters", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
}
#[test]
fn peer_snapshot_scope_idempotence_and_strict_public_dto() {
    let mut s = Store::in_memory().unwrap();
    let p = peer_fixture(Uuid::new_v4());
    s.register_peer_session(&p).unwrap();
    let a = public();
    let snap = snapshot(&p, 1, vec![a.clone()]);
    let state = s.apply_peer_snapshot(&snap).unwrap();
    assert!(!state.stale);
    assert_eq!(state.cursor, 1);
    s.apply_peer_snapshot(&snap).unwrap();
    let mut bad = snap.clone();
    bad.owner_device_id = Uuid::new_v4();
    assert!(s.apply_peer_snapshot(&bad).is_err());
    bad = snap.clone();
    bad.artifacts[0].text = "changed same version".into();
    assert!(s.apply_peer_snapshot(&bad).is_err());
    bad.cursor = 2;
    bad.artifacts[0].publication_seq = 2;
    assert!(s.apply_peer_snapshot(&bad).is_err());
    bad = snapshot(&p, 0, vec![]);
    assert!(matches!(
        s.apply_peer_snapshot(&bad),
        Err(StoreError::InvalidCursor)
    ));
    let mut wire = serde_json::to_value(&snap).unwrap();
    wire["artifacts"][0]["source_transcript"] = "PRIVATE".into();
    assert!(serde_json::from_value::<PeerPublicationSnapshot>(wire).is_err());
    let mut collision = p.clone();
    collision.owner_device_id = Uuid::new_v4();
    assert!(s.register_peer_session(&collision).is_err());
    let local = SessionSpec {
        session_id: p.session_id,
        event_id: p.event_id,
        room_id: Uuid::new_v4(),
        owner_device_id: p.owner_device_id,
        title: "private".into(),
    };
    assert!(s.create_session(&local).is_err());
    assert_eq!(
        s.peer_public_snapshot(p.owner_device_id, p.session_id)
            .unwrap()
            .artifacts,
        vec![a]
    );
}
#[test]
fn peer_delta_gap_rollback_withdrawal_and_snapshot_recovery() {
    let mut s = Store::in_memory().unwrap();
    let p = peer_fixture(Uuid::new_v4());
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![a.clone()]))
        .unwrap();
    let mut batch = PeerPublicationBatch {
        owner_device_id: p.owner_device_id,
        event_id: p.event_id,
        session_id: p.session_id,
        after_cursor: 0,
        cursor: 2,
        changes: vec![PublicationChange {
            publication_seq: 2,
            public_id: a.public_id,
            withdrawn: true,
            artifact: None,
        }],
    };
    assert!(matches!(
        s.apply_peer_changes(&batch),
        Err(StoreError::InvalidCursor)
    ));
    batch.after_cursor = 1;
    batch.changes[0].artifact = Some(a.clone());
    assert!(s.apply_peer_changes(&batch).is_err());
    assert_eq!(
        s.peer_public_snapshot(p.owner_device_id, p.session_id)
            .unwrap()
            .artifacts
            .len(),
        1
    );
    batch.changes[0].artifact = None;
    s.apply_peer_changes(&batch).unwrap();
    assert!(s
        .peer_public_snapshot(p.owner_device_id, p.session_id)
        .unwrap()
        .artifacts
        .is_empty());
    assert!(s.apply_peer_changes(&batch).is_ok());
    let stale = s
        .mark_peer_disconnected(p.owner_device_id, p.session_id)
        .unwrap();
    assert!(stale.stale);
    assert!(stale.last_sync_at_ms.is_some());
    let mut newer = a.clone();
    newer.revision = Revision::try_from(2).unwrap();
    newer.publication_seq = 5;
    newer.text = "new reviewed content".into();
    s.apply_peer_snapshot(&snapshot(&p, 5, vec![newer.clone()]))
        .unwrap();
    assert!(!s.peer_sessions(p.event_id).unwrap()[0].stale);
    s.connection.execute_batch("CREATE TRIGGER fail_peer BEFORE INSERT ON peer_publications BEGIN SELECT RAISE(ABORT,'failure'); END").unwrap();
    newer.revision = Revision::try_from(3).unwrap();
    newer.publication_seq = 6;
    assert!(s
        .apply_peer_snapshot(&snapshot(&p, 6, vec![newer]))
        .is_err());
    assert_eq!(
        s.peer_public_snapshot(p.owner_device_id, p.session_id)
            .unwrap()
            .cursor,
        5
    );
}
#[test]
fn cross_room_report_uses_only_selected_public_copies_and_withdraws_dependents() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    publish(&mut s, &a);
    let p = peer_fixture(f.session.event_id);
    s.register_peer_session(&p).unwrap();
    let remote = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![remote.clone()]))
        .unwrap();
    let mut req = request(&f, AnalysisKind::EventReport);
    req.session_ids.push(p.session_id);
    let j = s.create_analysis_job(&req).unwrap();
    let input = s.analysis_snapshot(j.job_id).unwrap().snapshot;
    assert!(input.segments.is_empty());
    assert_eq!(input.published_artifacts.len(), 2);
    assert!(input
        .published_artifacts
        .iter()
        .all(|p| !p.text.contains("先测试")));
    let remote_input = input
        .published_artifacts
        .iter()
        .find(|v| v.session_ids == vec![p.session_id])
        .unwrap();
    assert_ne!(remote_input.artifact_id, remote.public_id);
    let mut result = result(&s, &j);
    result.coverage.units = input
        .published_artifacts
        .iter()
        .map(|a| CoverageUnit {
            target: CoverageTarget::Artifact {
                artifact_id: a.artifact_id,
                revision: a.revision,
            },
            start_utf8: 0,
            end_utf8: a.text.len(),
            status: CoverageStatus::Processed,
            reason: None,
        })
        .collect();
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let report = s.finish_analysis_job(&result).unwrap();
    assert!(report.coverage_complete);
    publish(&mut s, &report);
    s.apply_peer_snapshot(&snapshot(&p, 2, vec![])).unwrap();
    assert_eq!(
        s.artifact(report.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
    assert!(!s
        .public_snapshot(f.session.session_id)
        .unwrap()
        .artifacts
        .iter()
        .any(|v| v.kind == AnalysisKind::EventReport));
    let evidence = AnalysisEvidence::Artifact {
        artifact_id: remote_input.artifact_id,
        revision: remote_input.revision,
        start_utf8: 0,
        end_utf8: remote_input.text.len(),
        quote: remote_input.text.clone(),
    };
    assert!(!s.resolve_analysis_evidence(&evidence).unwrap().current);
}
#[test]
fn closing_brief_requires_published_input_review_and_live_peer_fence() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    assert!(s
        .create_analysis_job(&request(&f, AnalysisKind::ClosingBrief))
        .is_err());
    let p = peer_fixture(f.session.event_id);
    s.register_peer_session(&p).unwrap();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![public()]))
        .unwrap();
    let mut req = request(&f, AnalysisKind::ClosingBrief);
    req.session_ids = vec![p.session_id];
    let j = s.create_analysis_job(&req).unwrap();
    assert!(s
        .analysis_snapshot(j.job_id)
        .unwrap()
        .snapshot
        .segments
        .is_empty());
    s.claim_analysis_job(j.job_id, 1).unwrap();
    let a = s.finish_analysis_job(&result(&s, &j)).unwrap();
    let public = publish(&mut s, &a);
    assert_eq!(public.kind, AnalysisKind::ClosingBrief);
    s.mark_peer_disconnected(p.owner_device_id, p.session_id)
        .unwrap();
    assert_eq!(
        s.artifact(a.artifact_id).unwrap().publication,
        ArtifactPublication::Withdrawn
    );
    req.request_id = Uuid::new_v4();
    assert!(s.create_analysis_job(&req).is_err());
    s.apply_peer_snapshot(&snapshot(
        &p,
        1,
        s.peer_public_snapshot(p.owner_device_id, p.session_id)
            .unwrap()
            .artifacts,
    ))
    .unwrap();
    req.request_id = Uuid::new_v4();
    assert!(s.create_analysis_job(&req).is_ok());
    assert_eq!(
        s.artifact(a.artifact_id).unwrap().validation,
        ArtifactValidation::Stale
    );
}
#[test]
fn public_search_unicode_offsets_public_evidence_and_stale_are_safe() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let draft = finish(&mut s, &f);
    let p = peer_fixture(f.session.event_id);
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![a])).unwrap();
    assert!(s
        .search_public(f.session.event_id, &[], "先测试", 10)
        .unwrap()
        .is_empty());
    assert!(s
        .search_public(f.session.event_id, &[], "测试会议", 10)
        .unwrap()
        .is_empty());
    for query in ["BUDGET", "两个试点", "📝"] {
        let hits = s.search_public(f.session.event_id, &[], query, 10).unwrap();
        assert_eq!(hits.len(), 1);
        let hit = &hits[0];
        assert_eq!(
            hit.text[hit.match_start_utf8..hit.match_end_utf8].to_lowercase(),
            query.to_lowercase()
        );
    }
    s.mark_peer_disconnected(p.owner_device_id, p.session_id)
        .unwrap();
    assert!(
        s.search_public(f.session.event_id, &[], "Budget", 10)
            .unwrap()[0]
            .stale
    );
    assert!(s
        .search_public(f.session.event_id, &[Uuid::new_v4()], "Budget", 10)
        .is_err());
    assert_eq!(
        s.artifact(draft.artifact_id).unwrap().publication,
        ArtifactPublication::Private
    );
}
#[test]
fn migration_five_to_six_preserves_source_and_makes_backup() {
    let db = TempDatabase::new();
    let mut s = Store::open(db.path()).unwrap();
    let f = setup(&mut s);
    let expected = s.session_snapshot(f.session.session_id).unwrap();
    s.connection.execute_batch("DROP TABLE speaker_assignment_requests; DROP TABLE speaker_assignment_revisions; DROP TABLE speaker_assignments; DROP TABLE speaker_clusters; DROP TABLE analysis_peer_provenance; DROP TABLE peer_publication_watermarks; DROP TABLE peer_publication_history; DROP TABLE peer_publications; DROP TABLE peer_sessions; PRAGMA user_version=5;").unwrap();
    drop(s);
    let mut s = Store::open(db.path()).unwrap();
    assert_eq!(
        expected.transcript,
        s.session_snapshot(f.session.session_id).unwrap().transcript
    );
    assert!(s.migration_backup.as_ref().unwrap().is_file());
    assert_eq!(
        s.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        6
    );
}
#[test]
fn selected_public_versions_are_exact_scoped_and_request_idempotent() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    let first = publish(&mut s, &a);
    let b = finish(&mut s, &f);
    let second = publish(&mut s, &b);
    let req = request(&f, AnalysisKind::ClosingBrief);
    let selection = PublicSelection {
        owner_device_id: f.session.owner_device_id,
        session_id: f.session.session_id,
        public_id: first.public_id,
        revision: first.revision,
    };
    let job = s
        .create_selected_analysis_job(&req, &[selection.clone()])
        .unwrap();
    assert_eq!(
        job,
        s.create_selected_analysis_job(&req, &[selection.clone()])
            .unwrap()
    );
    let input = s.analysis_snapshot(job.job_id).unwrap().snapshot;
    assert_eq!(input.published_artifacts.len(), 1);
    assert_eq!(input.published_artifacts[0].artifact_id, a.artifact_id);
    let mut different = selection.clone();
    different.public_id = second.public_id;
    assert!(matches!(
        s.create_selected_analysis_job(&req, &[different]),
        Err(StoreError::EventIdConflict)
    ));
    let mut req2 = req.clone();
    req2.request_id = Uuid::new_v4();
    let mut invalid = selection.clone();
    invalid.revision = Revision::try_from(2).unwrap();
    assert!(matches!(
        s.create_selected_analysis_job(&req2, &[invalid]),
        Err(StoreError::LateResult)
    ));
    invalid = selection.clone();
    invalid.owner_device_id = Uuid::new_v4();
    assert!(matches!(
        s.create_selected_analysis_job(&req2, &[invalid]),
        Err(StoreError::ScopeMismatch)
    ));
    assert!(s.create_selected_analysis_job(&req2, &[]).is_err());
    assert!(s
        .create_selected_analysis_job(&req2, &[selection.clone(), selection])
        .is_err());
}
#[test]
fn peer_provenance_is_frozen_and_foreign_event_selection_is_rejected() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let p = peer_fixture(f.session.event_id);
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![a.clone()]))
        .unwrap();
    let selection = PublicSelection {
        owner_device_id: p.owner_device_id,
        session_id: p.session_id,
        public_id: a.public_id,
        revision: a.revision,
    };
    let mut req = request(&f, AnalysisKind::ClosingBrief);
    req.session_ids = vec![p.session_id];
    let job = s.create_selected_analysis_job(&req, &[selection]).unwrap();
    let provenance = s.analysis_peer_provenance(job.job_id).unwrap();
    assert_eq!(provenance.len(), 1);
    assert_eq!(provenance[0].owner_device_id, p.owner_device_id);
    assert_eq!(provenance[0].source_cursor, 1);
    assert_eq!(provenance[0].public_id, a.public_id);
    s.apply_peer_snapshot(&snapshot(&p, 5, vec![a])).unwrap();
    assert_eq!(s.analysis_peer_provenance(job.job_id).unwrap(), provenance);
    let other = peer_fixture(Uuid::new_v4());
    s.register_peer_session(&other).unwrap();
    s.apply_peer_snapshot(&snapshot(&other, 1, vec![public()]))
        .unwrap();
    req.request_id = Uuid::new_v4();
    req.session_ids.push(other.session_id);
    assert!(matches!(
        s.create_analysis_job(&req),
        Err(StoreError::ScopeMismatch)
    ));
    assert_eq!(s.all_peer_sessions().unwrap().len(), 2);
    s.disconnect_all_peers().unwrap();
    assert!(s.all_peer_sessions().unwrap().iter().all(|p| p.stale));
}
#[test]
fn withdrawn_publication_cannot_resurrect_with_old_sequence() {
    let mut s = Store::in_memory().unwrap();
    let p = peer_fixture(Uuid::new_v4());
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![a.clone()]))
        .unwrap();
    s.apply_peer_snapshot(&snapshot(&p, 2, vec![])).unwrap();
    assert!(s
        .apply_peer_snapshot(&snapshot(&p, 3, vec![a.clone()]))
        .is_err());
    assert!(s
        .peer_public_snapshot(p.owner_device_id, p.session_id)
        .unwrap()
        .artifacts
        .is_empty());
    let mut republished = a;
    republished.publication_seq = 3;
    s.apply_peer_snapshot(&snapshot(&p, 3, vec![republished]))
        .unwrap();
}
#[test]
fn delayed_peer_withdrawal_fences_result_and_speaker_change_fences_source_job() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let p = peer_fixture(f.session.event_id);
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![a])).unwrap();
    let mut req = request(&f, AnalysisKind::ClosingBrief);
    req.session_ids = vec![p.session_id];
    let job = s.create_analysis_job(&req).unwrap();
    s.claim_analysis_job(job.job_id, 1).unwrap();
    let output = result(&s, &job);
    s.apply_peer_snapshot(&snapshot(&p, 2, vec![])).unwrap();
    assert!(matches!(
        s.finish_analysis_job(&output),
        Err(StoreError::LateResult)
    ));
    s.fail_analysis_job(job.job_id, 1, "peer withdrawn".into())
        .unwrap();
    let job = s
        .create_analysis_job(&request(&f, AnalysisKind::Insight))
        .unwrap();
    s.claim_analysis_job(job.job_id, 1).unwrap();
    let output = result(&s, &job);
    s.apply_speaker_embedding(&embedding(&f)).unwrap();
    assert!(matches!(
        s.finish_analysis_job(&output),
        Err(StoreError::LateResult)
    ));
}
#[test]
fn closing_narration_reads_reviewed_public_text_and_stops_after_withdrawal() {
    let mut s = Store::in_memory().unwrap();
    let f = setup(&mut s);
    let a = finish(&mut s, &f);
    publish(&mut s, &a);
    let job = s
        .create_analysis_job(&request(&f, AnalysisKind::ClosingBrief))
        .unwrap();
    s.claim_analysis_job(job.job_id, 1).unwrap();
    let closing = s.finish_analysis_job(&result(&s, &job)).unwrap();
    assert!(s
        .public_artifact_for_revision(closing.artifact_id, closing.revision)
        .is_err());
    let public = publish(&mut s, &closing);
    assert_eq!(
        s.public_artifact_for_revision(closing.artifact_id, closing.revision)
            .unwrap(),
        public
    );
    assert_ne!(public.text, closing.content.title);
    s.set_artifact_visibility(&ArtifactVisibilityCommand {
        artifact_id: closing.artifact_id,
        expected_revision: closing.revision,
        publication: ArtifactPublication::Hidden,
        operator_id: "op".into(),
        reason: "stop narration".into(),
    })
    .unwrap();
    assert!(s
        .public_artifact_for_revision(closing.artifact_id, closing.revision)
        .is_err());
    assert!(s
        .public_artifact_for_revision(a.artifact_id, a.revision)
        .is_err());
}
#[test]
fn unseen_tombstone_is_remembered_and_replayed_delta_refreshes_peer() {
    let mut s = Store::in_memory().unwrap();
    let p = peer_fixture(Uuid::new_v4());
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![])).unwrap();
    let b = PeerPublicationBatch {
        owner_device_id: p.owner_device_id,
        event_id: p.event_id,
        session_id: p.session_id,
        after_cursor: 1,
        cursor: 2,
        changes: vec![PublicationChange {
            publication_seq: 2,
            public_id: a.public_id,
            withdrawn: true,
            artifact: None,
        }],
    };
    s.apply_peer_changes(&b).unwrap();
    s.mark_peer_disconnected(p.owner_device_id, p.session_id)
        .unwrap();
    assert!(!s.apply_peer_changes(&b).unwrap().stale);
    assert!(s.apply_peer_snapshot(&snapshot(&p, 3, vec![a])).is_err());
}
#[test]
fn one_delta_batch_cannot_hide_intermediate_revision_regression() {
    let mut s = Store::in_memory().unwrap();
    let p = peer_fixture(Uuid::new_v4());
    s.register_peer_session(&p).unwrap();
    let a = public();
    s.apply_peer_snapshot(&snapshot(&p, 1, vec![a.clone()]))
        .unwrap();
    let mut newer = a.clone();
    newer.revision = Revision::try_from(2).unwrap();
    newer.publication_seq = 2;
    newer.text = "new reviewed revision".into();
    let mut older = a.clone();
    older.publication_seq = 3;
    let b = PeerPublicationBatch {
        owner_device_id: p.owner_device_id,
        event_id: p.event_id,
        session_id: p.session_id,
        after_cursor: 1,
        cursor: 3,
        changes: vec![
            PublicationChange {
                publication_seq: 2,
                public_id: a.public_id,
                withdrawn: false,
                artifact: Some(newer),
            },
            PublicationChange {
                publication_seq: 3,
                public_id: a.public_id,
                withdrawn: false,
                artifact: Some(older),
            },
        ],
    };
    assert!(s.apply_peer_changes(&b).is_err());
    let after = s
        .peer_public_snapshot(p.owner_device_id, p.session_id)
        .unwrap();
    assert_eq!(after.cursor, 1);
    assert_eq!(after.artifacts, vec![a]);
}
