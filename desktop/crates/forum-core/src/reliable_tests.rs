use super::*;
use std::time::{Duration, Instant};

fn event<T>(f: &Fixture, kind: EventType, payload: T) -> Event<T> {
    Event {
        schema_version: SCHEMA_VERSION,
        message_id: Uuid::new_v4(),
        event_type: kind,
        event_id: f.session.event_id,
        room_id: f.session.room_id,
        session_id: f.session.session_id,
        producer: Producer {
            name: "test-host".into(),
            run_id: Uuid::new_v4(),
            seq: 1,
        },
        payload,
    }
}
fn transition(
    store: &mut Store,
    f: &Fixture,
    expected: SessionState,
    next: SessionState,
) -> Receipt {
    store
        .transition_session(&event(
            f,
            EventType::SessionChanged,
            SessionTransition {
                expected_state: expected,
                next_state: next,
                reason: "test lifecycle".into(),
            },
        ))
        .unwrap()
}
fn real_setup(store: &mut Store, f: &Fixture) {
    store.create_session(&f.session).unwrap();
    store.create_track(&f.track).unwrap();
    transition(store, f, SessionState::Created, SessionState::Preparing);
    transition(store, f, SessionState::Preparing, SessionState::Ready);
    transition(store, f, SessionState::Ready, SessionState::Recording);
}
fn stop(store: &mut Store, f: &Fixture) -> CaptureStopped {
    let state = store.session_status(f.session.session_id).unwrap().state;
    if state == SessionState::Recording {
        transition(store, f, state, SessionState::Stopping);
    }
    let tracks = vec![TrackSeal {
        track_id: f.track.track_id,
        final_sample: 80000,
        segment_ids: vec![f.capture.payload.segment_id],
    }];
    let payload = CaptureStopped {
        manifest_sha256: capture_manifest_sha256(&tracks).unwrap(),
        tracks,
    };
    store
        .capture_stopped(&event(f, EventType::CaptureStopped, payload.clone()))
        .unwrap();
    payload
}
fn requested(f: &Fixture, start: usize, end: usize) -> Event<TranslationRequested> {
    let text = &f.final_event.payload.text;
    let quote = text[start..end].to_string();
    event(
        f,
        EventType::TranslationRequested,
        TranslationRequested {
            translation_id: Uuid::new_v4(),
            revision: Revision::FIRST,
            attempt: 1,
            target_language: "en".into(),
            direction_epoch: 1,
            source_spans: vec![SourceSpan {
                segment_id: f.capture.payload.segment_id,
                segment_revision: Revision::FIRST,
                start_utf8: start,
                end_utf8: end,
                quote: quote.clone(),
            }],
            input_text: quote,
            normalization_version: "identity-v1".into(),
            backend: "test-translation".into(),
            model_manifest_id: "test-model".into(),
        },
    )
}
fn finished(f: &Fixture, request: &Event<TranslationRequested>) -> Event<TranslationFinal> {
    let p = &request.payload;
    event(
        f,
        EventType::TranslationFinal,
        TranslationFinal {
            translation_id: p.translation_id,
            revision: p.revision,
            attempt: p.attempt,
            direction_epoch: p.direction_epoch,
            text: "translation".into(),
            status: TranslationStatus::Final,
            backend: p.backend.clone(),
            model_manifest_id: p.model_manifest_id.clone(),
            elapsed_ms: 1,
        },
    )
}
fn source(store: &mut Store, f: &Fixture) {
    f.setup(store);
    store.register_capture(&f.capture).unwrap();
    store.ingest_final(&f.final_event).unwrap();
}

#[test]
fn lifecycle_rejects_capture_before_ready_and_seals_only_reconciled_terminals() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    store.create_session(&f.session).unwrap();
    store.create_track(&f.track).unwrap();
    assert!(matches!(
        store.register_capture(&f.capture),
        Err(StoreError::InvalidState)
    ));
    assert!(store.outbox_after(0, 100).unwrap().is_empty());
    real_setup(&mut store, &f);
    store.register_capture(&f.capture).unwrap();
    let capture = stop(&mut store, &f);
    let seal = event(
        &f,
        EventType::TranscriptSealed,
        TranscriptSeal {
            capture_manifest_sha256: capture.manifest_sha256,
        },
    );
    assert!(matches!(
        store.seal_transcript(&seal),
        Err(StoreError::DependencyNotReady)
    ));
    store.ingest_final(&f.final_event).unwrap();
    assert!(matches!(
        store.seal_transcript(&seal),
        Err(StoreError::DependencyNotReady)
    ));
    let mut producer = event(
        &f,
        EventType::ProducerSealed,
        ProducerSeal {
            producer_run_id: f.final_event.producer.run_id,
            final_seq: 1,
            segment_ids: vec![f.capture.payload.segment_id],
        },
    );
    producer.producer = f.final_event.producer.clone();
    producer.producer.seq = 2;
    store.producer_sealed(&producer).unwrap();
    store.seal_transcript(&seal).unwrap();
    let status = store.session_status(f.session.session_id).unwrap();
    assert_eq!(status.state, SessionState::Completed);
    assert!(status.capture_stopped && status.transcript_sealed && !status.incomplete);
    assert!(store.ingest_final(&f.final_event).unwrap().duplicate);
    let mut late = f.final_event.clone();
    late.message_id = Uuid::new_v4();
    late.producer.seq = 3;
    assert!(matches!(
        store.ingest_final(&late),
        Err(StoreError::LateResult)
    ));
    assert!(store.register_capture(&f.capture).unwrap().duplicate);
    let mut capture = f.capture.clone();
    capture.message_id = Uuid::new_v4();
    capture.producer.seq = 2;
    capture.payload.segment_id = Uuid::new_v4();
    assert!(matches!(
        store.register_capture(&capture),
        Err(StoreError::InvalidState)
    ));
}

#[test]
fn capture_manifest_missing_extra_and_wrong_hash_fail_atomically() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    real_setup(&mut store, &f);
    store.register_capture(&f.capture).unwrap();
    transition(
        &mut store,
        &f,
        SessionState::Recording,
        SessionState::Stopping,
    );
    let cursor = store.session_snapshot(f.session.session_id).unwrap().cursor;
    for (segments, final_sample) in [
        (vec![], 80000),
        (vec![f.capture.payload.segment_id, Uuid::new_v4()], 80000),
        (vec![f.capture.payload.segment_id], 70000),
    ] {
        let tracks = vec![TrackSeal {
            track_id: f.track.track_id,
            final_sample,
            segment_ids: segments,
        }];
        let p = CaptureStopped {
            manifest_sha256: capture_manifest_sha256(&tracks).unwrap(),
            tracks,
        };
        assert!(store
            .capture_stopped(&event(&f, EventType::CaptureStopped, p))
            .is_err());
        assert_eq!(
            store.session_snapshot(f.session.session_id).unwrap().cursor,
            cursor
        );
    }
}

#[test]
fn crash_recovery_needs_trusted_evidence_and_never_forges_old_normal_seal() {
    let db = TempDatabase::new();
    let f = Fixture::new();
    {
        let mut s = Store::open(db.path()).unwrap();
        real_setup(&mut s, &f);
        s.register_capture(&f.capture).unwrap();
        s.ingest_final(&f.final_event).unwrap();
    }
    let mut s = Store::open(db.path()).unwrap();
    assert_eq!(
        s.session_status(f.session.session_id).unwrap().state,
        SessionState::Interrupted
    );
    let capture = stop(&mut s, &f);
    assert_eq!(
        s.unsealed_producer_runs(f.session.session_id).unwrap(),
        vec![f.final_event.producer.run_id]
    );
    let mut evidence = ProducerRecoveryEvidence {
        capture_manifest_sha256: capture.manifest_sha256.clone(),
        owned_process_exited: false,
        outbox_replayed: true,
        reason: "owned child reaped; durable journal replayed".into(),
    };
    assert!(matches!(
        s.reconcile_abandoned_producer(
            f.session.session_id,
            f.final_event.producer.run_id,
            evidence.clone()
        ),
        Err(StoreError::DependencyNotReady)
    ));
    evidence.owned_process_exited = true;
    let receipt = s
        .reconcile_abandoned_producer(
            f.session.session_id,
            f.final_event.producer.run_id,
            evidence.clone(),
        )
        .unwrap();
    assert!(
        s.reconcile_abandoned_producer(
            f.session.session_id,
            f.final_event.producer.run_id,
            evidence
        )
        .unwrap()
        .duplicate
    );
    let message = s.outbox_after(receipt.store_seq - 1, 1).unwrap().remove(0);
    assert_eq!(message.event_type, "session.producer_reconciled");
    assert_ne!(
        message.body["producer"]["run_id"],
        f.final_event.producer.run_id.to_string()
    );
    assert!(matches!(
        s.ingest_json(message.body),
        Err(StoreError::InvalidState)
    ));
    assert!(s
        .unsealed_producer_runs(f.session.session_id)
        .unwrap()
        .is_empty());
    s.seal_transcript(&event(
        &f,
        EventType::TranscriptSealed,
        TranscriptSeal {
            capture_manifest_sha256: capture.manifest_sha256,
        },
    ))
    .unwrap();
    assert!(s.ingest_final(&f.final_event).unwrap().duplicate);
    let mut late = f.final_event.clone();
    late.message_id = Uuid::new_v4();
    late.producer.seq = 2;
    assert!(matches!(s.ingest_final(&late), Err(StoreError::LateResult)));
}

#[test]
fn failed_asr_can_be_recovered_at_next_revision_without_overwriting_success() {
    let mut s = Store::in_memory().unwrap();
    let mut f = Fixture::new();
    f.setup(&mut s);
    s.register_capture(&f.capture).unwrap();
    f.final_event.payload.status = TranscriptStatus::Failed;
    f.final_event.payload.text.clear();
    f.final_event.payload.reason = Some("worker exited".into());
    s.ingest_final(&f.final_event).unwrap();
    assert!(s.session_status(f.session.session_id).unwrap().incomplete);
    let next = s.recovery_segments(f.session.session_id, 1).unwrap();
    assert_eq!(next[0].next_revision.get(), 2);
    let mut recovered = f.final_event.clone();
    recovered.message_id = Uuid::new_v4();
    recovered.producer.run_id = Uuid::new_v4();
    recovered.payload.revision = next[0].next_revision;
    recovered.payload.status = TranscriptStatus::Success;
    recovered.payload.text = "恢复原文".into();
    recovered.payload.reason = None;
    s.ingest_final(&recovered).unwrap();
    assert!(s
        .recovery_segments(f.session.session_id, 1)
        .unwrap()
        .is_empty());
    assert!(!s.session_status(f.session.session_id).unwrap().incomplete);
    recovered.message_id = Uuid::new_v4();
    recovered.producer.seq = 2;
    recovered.payload.revision = 3.try_into().unwrap();
    assert!(matches!(
        s.ingest_final(&recovered),
        Err(StoreError::RevisionConflict { .. })
    ));
}

#[test]
fn persistent_gaps_keep_completed_session_incomplete() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    s.register_gap(&event(
        &f,
        EventType::AudioGap,
        AudioGap {
            gap_id: Uuid::new_v4(),
            track_id: f.track.track_id,
            audio: f.capture.payload.audio.clone(),
            reason: "capture overflow".into(),
            recoverable: false,
        },
    ))
    .unwrap();
    let capture = stop(&mut s, &f);
    s.reconcile_abandoned_producer(
        f.session.session_id,
        f.final_event.producer.run_id,
        ProducerRecoveryEvidence {
            capture_manifest_sha256: capture.manifest_sha256.clone(),
            owned_process_exited: true,
            outbox_replayed: true,
            reason: "test host recovery".into(),
        },
    )
    .unwrap();
    s.seal_transcript(&event(
        &f,
        EventType::TranscriptSealed,
        TranscriptSeal {
            capture_manifest_sha256: capture.manifest_sha256,
        },
    ))
    .unwrap();
    assert!(s.session_status(f.session.session_id).unwrap().incomplete);
    assert_eq!(s.audio_gaps(f.session.session_id).unwrap().len(), 1);
}

#[test]
fn process_lock_alias_and_shutdown_release_are_real() {
    let db = TempDatabase::new();
    let h = CoreHandle::open(db.path(), 2).unwrap();
    assert!(matches!(
        Store::open(db.path()),
        Err(StoreError::AlreadyOwned)
    ));
    #[cfg(unix)]
    {
        let alias = db.0.join("alias.sqlite");
        std::os::unix::fs::symlink(db.path(), &alias).unwrap();
        assert!(matches!(Store::open(alias), Err(StoreError::AlreadyOwned)));
    }
    h.shutdown().unwrap();
    let s = Store::open(db.path()).unwrap();
    drop(s);
    #[cfg(unix)]
    {
        let alias = db.0.join("hardlink.sqlite");
        std::fs::hard_link(db.path(), &alias).unwrap();
        assert!(matches!(Store::open(alias), Err(StoreError::Validation(_))));
    }
}

#[test]
fn ownership_is_enforced_between_processes() {
    if let Some(path) = std::env::var_os("FORUM_CORE_TEST_LOCK_PATH") {
        assert!(matches!(Store::open(path), Err(StoreError::AlreadyOwned)));
        return;
    }
    let db = TempDatabase::new();
    let _s = Store::open(db.path()).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::reliable_tests::ownership_is_enforced_between_processes",
        ])
        .env("FORUM_CORE_TEST_LOCK_PATH", db.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn actor_queue_is_bounded_and_timeout_is_ack_unknown_then_replay_is_idempotent() {
    let db = TempDatabase::new();
    let f = Fixture::new();
    let h = CoreHandle::open(db.path(), 1).unwrap();
    let setup = f.session.clone();
    let track = f.track.clone();
    let sid = setup.session_id;
    h.call(move |s| {
        s.create_session(&setup)?;
        s.create_track(&track)?;
        s.connection.execute(
            "UPDATE sessions SET state='recording' WHERE id=?1",
            [sid.to_string()],
        )?;
        Ok(())
    })
    .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let hold = h
        .try_call(move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            Ok(())
        })
        .unwrap();
    started_rx.recv().unwrap();
    let body = serde_json::to_value(&f.capture).unwrap();
    let queued = h.try_ingest_json(body.clone()).unwrap();
    assert!(matches!(h.try_call(|_| Ok(())), Err(StoreError::QueueFull)));
    assert!(matches!(h.shutdown(), Err(StoreError::QueueFull)));
    let start = Instant::now();
    assert!(matches!(
        queued.wait_timeout(Duration::from_millis(5)),
        Err(StoreError::AckUnknown)
    ));
    assert!(start.elapsed() < Duration::from_millis(100));
    release_tx.send(()).unwrap();
    hold.wait().unwrap();
    // The hold ACK does not promise that the next queued command has already
    // been dequeued. Retry admission under real backpressure; FIFO then ensures
    // the original event is persisted before this replay executes.
    let deadline = Instant::now() + Duration::from_secs(4);
    let replay = loop {
        match h.try_ingest_json(body.clone()) {
            Ok(ticket) => break ticket.wait().unwrap(),
            Err(StoreError::QueueFull) => {
                assert!(Instant::now() < deadline, "actor queue did not drain");
                std::thread::yield_now();
            }
            Err(error) => panic!("unexpected replay admission error: {error}"),
        }
    };
    assert!(replay.duplicate);
    h.shutdown().unwrap();
}

#[test]
fn migration_keeps_queryable_pre_migration_backup() {
    let db = TempDatabase::new();
    let c = Connection::open(db.path()).unwrap();
    c.execute_batch(include_str!("../migrations/001_initial.sql"))
        .unwrap();
    drop(c);
    let s = Store::open(db.path()).unwrap();
    let backup = s.migration_backup.clone().unwrap();
    let c = Connection::open(&backup).unwrap();
    assert_eq!(
        c.pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        s.connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, u32>(0))
            .unwrap(),
        DATABASE_VERSION
    );
    assert!(c.prepare("SELECT * FROM sessions").is_ok());
}

#[test]
fn snapshot_cursor_keeps_revisions_and_new_segments_out_of_later_pages() {
    let mut s = Store::in_memory().unwrap();
    let a = Fixture::new();
    source(&mut s, &a);
    let mut b = Fixture::new();
    b.session = a.session.clone();
    b.track.session_id = a.session.session_id;
    b.capture.event_id = a.session.event_id;
    b.capture.room_id = a.session.room_id;
    b.capture.session_id = a.session.session_id;
    b.final_event.event_id = a.session.event_id;
    b.final_event.room_id = a.session.room_id;
    b.final_event.session_id = a.session.session_id;
    b.capture.payload.audio.start_ms = 6000;
    b.final_event.payload.audio.start_ms = 6000;
    b.capture.payload.audio.end_ms = 9000;
    b.final_event.payload.audio.end_ms = 9000;
    source(&mut s, &b);
    let first = s
        .snapshot_page(a.session.session_id, None, None, 1)
        .unwrap();
    assert_eq!(first.items[0].segment_id, a.capture.payload.segment_id);
    s.revise_transcript(&b.revision(Revision::FIRST, "修改后的文本"))
        .unwrap();
    let second = s
        .snapshot_page(
            a.session.session_id,
            Some(first.cursor),
            first.next_after,
            1,
        )
        .unwrap();
    assert_eq!(
        second.items[0]
            .transcript
            .as_ref()
            .unwrap()
            .payload
            .revision,
        Revision::FIRST
    );
    assert!(s
        .snapshot_page(a.session.session_id, Some(first.cursor + 100), None, 1)
        .is_err());
    assert!(s
        .snapshot_page(
            a.session.session_id,
            None,
            Some(PageKey {
                start_ms: u64::MAX,
                track_id: a.track.track_id,
                segment_id: a.capture.payload.segment_id
            }),
            1
        )
        .is_err());
}

#[test]
fn exact_utf8_partial_coverage_can_split_and_resume_without_loss() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let req = requested(&f, 9, 13);
    assert_eq!(req.payload.input_text, "📝");
    let receipt = s.request_translation(&req).unwrap();
    assert!(s.request_translation(&req).unwrap().duplicate);
    let pending = s
        .pending_translation_work(f.session.session_id, 100)
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].source_span.quote, "先测试");
    assert_eq!(pending[1].source_span.quote, "字幕");
    let final_event = finished(&f, &req);
    s.finish_translation(&final_event).unwrap();
    assert!(s.finish_translation(&final_event).unwrap().duplicate);
    let records = s
        .translation_records_for_segments(f.session.session_id, vec![f.capture.payload.segment_id])
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].created_seq, receipt.store_seq);
    assert_eq!(records[0].result.as_ref().unwrap().text, "translation");
    assert_eq!(
        s.coverage(f.session.session_id)
            .unwrap()
            .iter()
            .filter(|c| c.state == "final")
            .count(),
        1
    );
}

#[test]
fn translation_claim_conflict_and_injected_sql_failure_roll_back_every_projection() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let req = requested(&f, 0, f.final_event.payload.text.len());
    s.connection.execute_batch("CREATE TEMP TRIGGER translation_failure BEFORE INSERT ON outbox WHEN NEW.event_type='translation.requested' BEGIN SELECT RAISE(ABORT,'injected'); END").unwrap();
    let before = s.session_snapshot(f.session.session_id).unwrap().cursor;
    assert!(s.request_translation(&req).is_err());
    assert!(s
        .translation_requests(f.session.session_id, 100)
        .unwrap()
        .is_empty());
    assert_eq!(
        s.pending_translation_work(f.session.session_id, 100)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        s.session_snapshot(f.session.session_id).unwrap().cursor,
        before
    );
    s.connection
        .execute_batch("DROP TRIGGER translation_failure")
        .unwrap();
    s.request_translation(&req).unwrap();
    let conflict = requested(&f, 9, 13);
    assert!(matches!(
        s.request_translation(&conflict),
        Err(StoreError::CoverageConflict)
    ));
    assert_eq!(
        s.translation_requests(f.session.session_id, 100)
            .unwrap()
            .len(),
        1
    );
    let mut collision = req.clone();
    collision.payload.backend = "different".into();
    assert!(matches!(
        s.request_translation(&collision),
        Err(StoreError::EventIdConflict)
    ));
}

#[test]
fn translation_attempt_source_and_direction_fences_reject_late_results() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let req = requested(&f, 0, f.final_event.payload.text.len());
    s.request_translation(&req).unwrap();
    let old_final = finished(&f, &req);
    let mut retry = req.clone();
    retry.message_id = Uuid::new_v4();
    retry.producer.seq = 2;
    retry.payload.attempt = 2;
    s.request_translation(&retry).unwrap();
    assert!(matches!(
        s.finish_translation(&old_final),
        Err(StoreError::LateResult)
    ));
    let current_final = finished(&f, &retry);
    s.revise_transcript(&f.revision(Revision::FIRST, "新原文"))
        .unwrap();
    assert!(matches!(
        s.finish_translation(&current_final),
        Err(StoreError::LateResult)
    ));
    assert_eq!(
        s.translation_requests(f.session.session_id, 100).unwrap()[0].state,
        "stale"
    );
    let work = s
        .pending_translation_work(f.session.session_id, 100)
        .unwrap()
        .remove(0);
    let mut next = req.clone();
    next.message_id = Uuid::new_v4();
    next.producer.seq = 3;
    next.payload.revision = 2.try_into().unwrap();
    next.payload.source_spans = vec![work.source_span.clone()];
    next.payload.input_text = work.source_span.quote;
    s.request_translation(&next).unwrap();
    let final_event = finished(&f, &next);
    s.change_direction(&event(
        &f,
        EventType::DirectionChanged,
        DirectionChanged {
            expected_epoch: 1,
            direction_epoch: 2,
            target_languages: vec!["ja".into()],
            boundary: None,
        },
    ))
    .unwrap();
    assert!(matches!(
        s.finish_translation(&final_event),
        Err(StoreError::LateResult)
    ));
    let pending = s
        .pending_translation_work(f.session.session_id, 100)
        .unwrap();
    assert_eq!(pending[0].direction_epoch, 2);
    assert_eq!(pending[0].target_language, "ja");
}

#[test]
fn translation_reopen_restores_failed_requested_and_pending_tail() {
    let db = TempDatabase::new();
    let f = Fixture::new();
    let req = requested(&f, 9, 13);
    {
        let mut s = Store::open(db.path()).unwrap();
        source(&mut s, &f);
        s.request_translation(&req).unwrap();
        s.fail_translation(&event(
            &f,
            EventType::TranslationFailed,
            TranslationFailed {
                translation_id: req.payload.translation_id,
                revision: Revision::FIRST,
                attempt: 1,
                direction_epoch: 1,
                code: "CANCELLED".into(),
                reason: "stop".into(),
            },
        ))
        .unwrap();
    }
    let mut s = Store::open(db.path()).unwrap();
    assert_eq!(
        s.pending_translation_work(f.session.session_id, 100)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        s.unresolved_translation_requests(f.session.session_id, 1)
            .unwrap()[0]
            .state,
        "failed"
    );
    let mut retry = req.clone();
    retry.message_id = Uuid::new_v4();
    retry.producer.seq = 2;
    retry.payload.attempt = 2;
    s.request_translation(&retry).unwrap();
    s.finish_translation(&finished(&f, &retry)).unwrap();
    assert!(s
        .unresolved_translation_requests(f.session.session_id, 100)
        .unwrap()
        .is_empty());
}

#[test]
fn unresolved_filter_precedes_limit_and_translation_pages_freeze_state() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let req1 = requested(&f, 0, 9);
    let req2 = requested(&f, 9, 13);
    let req3 = requested(&f, 13, f.final_event.payload.text.len());
    for req in [&req1, &req2, &req3] {
        s.request_translation(req).unwrap();
    }
    s.finish_translation(&finished(&f, &req1)).unwrap();
    s.finish_translation(&finished(&f, &req2)).unwrap();
    assert_eq!(
        s.unresolved_translation_requests(f.session.session_id, 1)
            .unwrap()[0]
            .request
            .translation_id,
        req3.payload.translation_id
    );
    let page = s
        .translation_page(f.session.session_id, None, None, 2)
        .unwrap();
    s.finish_translation(&finished(&f, &req3)).unwrap();
    let tail = s
        .translation_page(f.session.session_id, Some(page.cursor), page.next_after, 2)
        .unwrap();
    assert_eq!(tail.items.len(), 1);
    assert_eq!(tail.items[0].state, "requested");
    let export = s.export_session(f.session.session_id).unwrap();
    assert_eq!(export.translations.len(), 3);
    assert!(export.translations.iter().all(|r| r.state == "final"));
}

#[test]
fn cross_session_and_missing_revision_translation_sources_are_rejected() {
    let mut s = Store::in_memory().unwrap();
    let a = Fixture::new();
    let b = Fixture::new();
    source(&mut s, &a);
    source(&mut s, &b);
    let mut req = requested(&a, 0, 9);
    req.payload.source_spans[0].segment_id = b.capture.payload.segment_id;
    assert!(matches!(
        s.request_translation(&req),
        Err(StoreError::ScopeMismatch)
    ));
    req.payload.source_spans[0].segment_id = Uuid::new_v4();
    assert!(matches!(
        s.request_translation(&req),
        Err(StoreError::DependencyNotReady)
    ));
    req.payload.source_spans[0].segment_id = a.capture.payload.segment_id;
    req.payload.source_spans[0].end_utf8 = 10;
    req.payload.source_spans[0].quote = "先测试x".into();
    req.payload.input_text = "先测试x".into();
    assert!(matches!(
        s.request_translation(&req),
        Err(StoreError::Validation(_))
    ));
}

#[test]
fn multi_source_invalidation_releases_unchanged_source_coverage() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let mut capture = f.capture.clone();
    capture.message_id = Uuid::new_v4();
    capture.producer.seq = 2;
    capture.payload.segment_id = Uuid::new_v4();
    s.register_capture(&capture).unwrap();
    let mut final2 = f.final_event.clone();
    final2.message_id = Uuid::new_v4();
    final2.producer.seq = 2;
    final2.payload.segment_id = capture.payload.segment_id;
    final2.payload.text = "第二段 ".into();
    s.ingest_final(&final2).unwrap();
    let mut req = requested(&f, 0, f.final_event.payload.text.len());
    req.payload.source_spans.push(SourceSpan {
        segment_id: capture.payload.segment_id,
        segment_revision: Revision::FIRST,
        start_utf8: 0,
        end_utf8: final2.payload.text.len(),
        quote: final2.payload.text.clone(),
    });
    req.payload.normalization_version = "join-space-v1".into();
    req.payload.input_text = format!("{} {}", f.final_event.payload.text, final2.payload.text);
    s.request_translation(&req).unwrap();
    let done = finished(&f, &req);
    s.finish_translation(&done).unwrap();
    let cursor = s.session_snapshot(f.session.session_id).unwrap().cursor;
    s.revise_transcript(&f.revision(Revision::FIRST, "第一段改正"))
        .unwrap();
    let pending = s
        .pending_translation_work(f.session.session_id, 100)
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending
        .iter()
        .any(|p| p.source_span.segment_id == capture.payload.segment_id
            && p.source_span.quote == final2.payload.text));
    assert_eq!(
        s.translation_requests(f.session.session_id, 100).unwrap()[0].state,
        "stale"
    );
    let historical = s
        .translation_records_for_segments_at(
            f.session.session_id,
            vec![capture.payload.segment_id],
            cursor,
        )
        .unwrap();
    assert_eq!(historical[0].state, "final");
}

#[test]
fn unresolved_paging_retry_uses_fixed_first_creation_key() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let first = requested(&f, 0, 9);
    let second = requested(&f, 9, 13);
    let third = requested(&f, 13, f.final_event.payload.text.len());
    for r in [&first, &second, &third] {
        s.request_translation(r).unwrap();
    }
    let page = s
        .unresolved_translation_page(f.session.session_id, None, 1)
        .unwrap();
    let key = page.next_after.clone();
    let first_seq = page.items[0].created_seq;
    let mut retry = first.clone();
    retry.message_id = Uuid::new_v4();
    retry.producer.seq = 2;
    retry.payload.attempt = 2;
    s.request_translation(&retry).unwrap();
    assert_eq!(
        s.unresolved_translation_page(f.session.session_id, None, 1)
            .unwrap()
            .items[0]
            .created_seq,
        first_seq
    );
    let page = s
        .unresolved_translation_page(f.session.session_id, key, 2)
        .unwrap();
    assert_eq!(page.items.len(), 2);
    assert!(page
        .items
        .iter()
        .all(|r| r.request.translation_id != first.payload.translation_id));
}

#[test]
fn replay_terminal_query_does_not_hide_failures_after_first_thousand() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut s);
    for index in 1..=1002 {
        let mut capture = f.capture.clone();
        capture.message_id = Uuid::new_v4();
        capture.producer.seq = index;
        capture.payload.segment_id = Uuid::new_v4();
        s.register_capture(&capture).unwrap();
        let mut final_event = f.final_event.clone();
        final_event.message_id = Uuid::new_v4();
        final_event.producer.seq = index;
        final_event.payload.segment_id = capture.payload.segment_id;
        if index <= 1001 {
            final_event.payload.status = TranscriptStatus::Failed;
            final_event.payload.text.clear();
            final_event.payload.reason = Some("fixture failure".into());
        }
        s.ingest_final(&final_event).unwrap();
    }
    assert_eq!(
        s.recovery_segments(f.session.session_id, 1000)
            .unwrap()
            .len(),
        1000
    );
    assert_eq!(
        s.terminal_segment_ids(f.session.session_id).unwrap().len(),
        1002
    );
    assert_eq!(
        s.terminal_segment_ids_for_replay(f.session.session_id)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn translation_final_outbox_failure_rolls_back_completion_and_replays_once() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let request = requested(&f, 0, f.final_event.payload.text.len());
    s.request_translation(&request).unwrap();
    let final_event = finished(&f, &request);
    s.connection.execute_batch("CREATE TEMP TRIGGER fail_translation_final BEFORE INSERT ON outbox WHEN NEW.event_type='translation.final' BEGIN SELECT RAISE(ABORT,'disk failure fixture'); END").unwrap();
    assert!(s.finish_translation(&final_event).is_err());
    assert_eq!(
        s.translation_requests(f.session.session_id, 1).unwrap()[0].state,
        "requested"
    );
    assert_eq!(
        s.coverage(f.session.session_id).unwrap()[0].state,
        "requested"
    );
    s.connection
        .execute_batch("DROP TRIGGER fail_translation_final")
        .unwrap();
    s.finish_translation(&final_event).unwrap();
    assert!(s.finish_translation(&final_event).unwrap().duplicate);
}

#[test]
fn prospective_boundary_preserves_late_old_asr_and_pending_translation_across_restart() {
    let db = TempDatabase::new();
    let f = Fixture::new();
    let direction = event(
        &f,
        EventType::DirectionChanged,
        DirectionChanged {
            expected_epoch: 1,
            direction_epoch: 2,
            target_languages: vec!["ja".into()],
            boundary: Some(DirectionBoundary {
                track_id: f.track.track_id,
                start_sample: 80000,
                configured_source_language: "en".into(),
            }),
        },
    );
    {
        let mut s = Store::open(db.path()).unwrap();
        real_setup(&mut s, &f);
        s.register_capture(&f.capture).unwrap();
        s.change_direction(&direction).unwrap();
    }
    let mut s = Store::open(db.path()).unwrap();
    s.ingest_final(&f.final_event).unwrap();
    let work = s
        .pending_translation_work(f.session.session_id, 10)
        .unwrap();
    assert_eq!(work[0].direction_epoch, 1);
    assert_eq!(work[0].target_language, "en");
    let old = requested(&f, 0, f.final_event.payload.text.len());
    s.request_translation(&old).unwrap();
    let mut capture = f.capture.clone();
    capture.message_id = Uuid::new_v4();
    capture.producer.seq = 2;
    capture.payload.segment_id = Uuid::new_v4();
    capture.payload.audio = AudioRange {
        start_sample: 80000,
        end_sample: 96000,
        sample_rate: 16000,
        start_ms: 5000,
        end_ms: 6000,
    };
    s.register_capture(&capture).unwrap();
    let mut final2 = f.final_event.clone();
    final2.message_id = Uuid::new_v4();
    final2.producer.seq = 2;
    final2.payload.segment_id = capture.payload.segment_id;
    final2.payload.audio = capture.payload.audio.clone();
    assert!(matches!(
        s.ingest_final(&final2),
        Err(StoreError::LateResult)
    ));
    final2.payload.direction_epoch = 2;
    final2.payload.configured_source_language = "en".into();
    final2.payload.target_languages = vec!["ja".into()];
    final2.payload.text = "new source".into();
    s.ingest_final(&final2).unwrap();
    let work = s
        .pending_translation_work(f.session.session_id, 10)
        .unwrap();
    assert_eq!(work.len(), 1);
    assert_eq!(work[0].direction_epoch, 2);
    assert_eq!(work[0].target_language, "ja");
    s.finish_translation(&finished(&f, &old)).unwrap();
    assert_eq!(
        s.translation_requests(f.session.session_id, 10).unwrap()[0].state,
        "final"
    );
}

#[test]
fn prospective_boundary_rejects_retroactive_or_crossing_capture_ranges() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let mut direction = event(
        &f,
        EventType::DirectionChanged,
        DirectionChanged {
            expected_epoch: 1,
            direction_epoch: 2,
            target_languages: vec!["ja".into()],
            boundary: Some(DirectionBoundary {
                track_id: f.track.track_id,
                start_sample: 70000,
                configured_source_language: "en".into(),
            }),
        },
    );
    assert!(matches!(
        s.change_direction(&direction),
        Err(StoreError::SealMismatch)
    ));
    assert_eq!(
        s.session_status(f.session.session_id)
            .unwrap()
            .direction_epoch,
        1
    );
    direction.payload.boundary.as_mut().unwrap().start_sample = 80000;
    s.change_direction(&direction).unwrap();
    let mut capture = f.capture.clone();
    capture.message_id = Uuid::new_v4();
    capture.producer.seq = 2;
    capture.payload.segment_id = Uuid::new_v4();
    capture.payload.audio.start_sample = 75000;
    capture.payload.audio.end_sample = 85000;
    assert!(matches!(
        s.register_capture(&capture),
        Err(StoreError::ScopeMismatch)
    ));
    let old = requested(&f, 0, f.final_event.payload.text.len());
    s.request_translation(&old).unwrap();
    s.finish_translation(&finished(&f, &old)).unwrap();
}

#[test]
fn explicit_retroactive_direction_changes_coverage_for_delayed_old_asr() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut s);
    s.register_capture(&f.capture).unwrap();
    let direction = event(
        &f,
        EventType::DirectionChanged,
        DirectionChanged {
            expected_epoch: 1,
            direction_epoch: 2,
            target_languages: vec!["ja".into()],
            boundary: None,
        },
    );
    assert!(serde_json::to_value(&direction).unwrap()["payload"]
        .get("boundary")
        .is_none());
    s.change_direction(&direction).unwrap();
    s.ingest_final(&f.final_event).unwrap();
    let work = s
        .pending_translation_work(f.session.session_id, 10)
        .unwrap();
    assert_eq!(work[0].direction_epoch, 2);
    assert_eq!(work[0].target_language, "ja");
    assert_eq!(
        s.transcript_revision(
            f.session.session_id,
            f.capture.payload.segment_id,
            Revision::FIRST
        )
        .unwrap()
        .payload
        .direction_epoch,
        1
    );
}

#[test]
fn unacknowledged_prospective_boundary_replays_after_core_restart() {
    let db = TempDatabase::new();
    let f = Fixture::new();
    let direction = event(
        &f,
        EventType::DirectionChanged,
        DirectionChanged {
            expected_epoch: 1,
            direction_epoch: 2,
            target_languages: vec!["ja".into()],
            boundary: Some(DirectionBoundary {
                track_id: f.track.track_id,
                start_sample: 80000,
                configured_source_language: "en".into(),
            }),
        },
    );
    {
        let mut s = Store::open(db.path()).unwrap();
        real_setup(&mut s, &f);
        s.register_capture(&f.capture).unwrap();
    }
    let mut s = Store::open(db.path()).unwrap();
    assert_eq!(
        s.session_status(f.session.session_id).unwrap().state,
        SessionState::Interrupted
    );
    s.change_direction(&direction).unwrap();
    assert!(s.change_direction(&direction).unwrap().duplicate);
    let mut capture = f.capture.clone();
    capture.message_id = Uuid::new_v4();
    capture.producer.seq = 2;
    capture.payload.segment_id = Uuid::new_v4();
    capture.payload.audio.start_sample = 80000;
    capture.payload.audio.end_sample = 96000;
    s.register_capture(&capture).unwrap();
    let epoch: u64 = s
        .connection
        .query_row(
            "SELECT direction_epoch FROM capture_segments WHERE segment_id=?1",
            [capture.payload.segment_id.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(epoch, 2);
}

#[test]
fn snapshot_tail_is_bounded_ordered_and_keeps_empty_failed_missing() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut s);
    for i in 1..=4 {
        let mut capture = f.capture.clone();
        capture.message_id = Uuid::new_v4();
        capture.producer.seq = i;
        capture.payload.segment_id = Uuid::new_v4();
        capture.payload.audio.start_ms = i * 1000;
        capture.payload.audio.end_ms = i * 1000 + 1000;
        s.register_capture(&capture).unwrap();
        if i < 4 {
            let mut final_event = f.final_event.clone();
            final_event.message_id = Uuid::new_v4();
            final_event.producer.seq = i;
            final_event.payload.segment_id = capture.payload.segment_id;
            final_event.payload.audio = capture.payload.audio;
            if i > 1 {
                final_event.payload.status = if i == 2 {
                    TranscriptStatus::Empty
                } else {
                    TranscriptStatus::Failed
                };
                final_event.payload.text.clear();
                final_event.payload.reason = Some("terminal fixture".into());
            }
            s.ingest_final(&final_event).unwrap();
        }
    }
    let tail = s.snapshot_tail(f.session.session_id, 3).unwrap();
    assert_eq!(tail.items.len(), 3);
    assert!(tail.next_after.is_none());
    assert_eq!(
        tail.items[0].transcript.as_ref().unwrap().payload.status,
        TranscriptStatus::Empty
    );
    assert_eq!(
        tail.items[1].transcript.as_ref().unwrap().payload.status,
        TranscriptStatus::Failed
    );
    assert!(tail.items[2].transcript.is_none());
    let export = s.export_session(f.session.session_id).unwrap();
    assert_eq!(export.sources.len(), 4);
    assert_eq!(export.snapshot.transcript.len(), 1);
}

#[test]
fn exact_recovery_revisions_never_infer_missing_tail_revision() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut s);
    let mut ids = Vec::new();
    for index in 1..=1002 {
        let mut capture = f.capture.clone();
        capture.message_id = Uuid::new_v4();
        capture.producer.seq = index;
        capture.payload.segment_id = Uuid::new_v4();
        s.register_capture(&capture).unwrap();
        ids.push(capture.payload.segment_id);
        let mut final_event = f.final_event.clone();
        final_event.message_id = Uuid::new_v4();
        final_event.producer.seq = index;
        final_event.payload.segment_id = capture.payload.segment_id;
        final_event.payload.status = TranscriptStatus::Failed;
        final_event.payload.text.clear();
        final_event.payload.reason = Some("failed".into());
        s.ingest_final(&final_event).unwrap();
    }
    let rows = s
        .recovery_revisions(f.session.session_id, ids[1000..].to_vec())
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|row| !row.terminal && row.next_revision.unwrap().get() == 2));
    assert!(s
        .recovery_revisions(f.session.session_id, ids[..257].to_vec())
        .is_err());
}

#[test]
fn exact_recovery_revisions_distinguish_terminal_missing_and_invalid_scope() {
    let mut s = Store::in_memory().unwrap();
    let f = Fixture::new();
    source(&mut s, &f);
    let mut missing = f.capture.clone();
    missing.message_id = Uuid::new_v4();
    missing.producer.seq = 2;
    missing.payload.segment_id = Uuid::new_v4();
    s.register_capture(&missing).unwrap();
    let rows = s
        .recovery_revisions(
            f.session.session_id,
            vec![missing.payload.segment_id, f.capture.payload.segment_id],
        )
        .unwrap();
    assert_eq!(rows[0].next_revision, Some(Revision::FIRST));
    assert!(!rows[0].terminal);
    assert!(rows[1].terminal);
    assert_eq!(rows[1].next_revision, None);
    let foreign = Fixture::new();
    source(&mut s, &foreign);
    assert!(matches!(
        s.recovery_revisions(
            f.session.session_id,
            vec![
                missing.payload.segment_id,
                foreign.capture.payload.segment_id
            ]
        ),
        Err(StoreError::ScopeMismatch)
    ));
    assert!(s
        .recovery_revisions(f.session.session_id, vec![Uuid::new_v4()])
        .is_err());
    assert!(s
        .recovery_revisions(
            f.session.session_id,
            vec![missing.payload.segment_id, missing.payload.segment_id]
        )
        .is_err());
}

#[test]
fn duplicated_descriptor_cannot_extend_closed_store_ownership() {
    let db = TempDatabase::new();
    let store = Store::open(db.path()).unwrap();
    let duplicate = store._ownership.as_ref().unwrap().file.try_clone().unwrap();
    assert!(matches!(
        Store::open(db.path()),
        Err(StoreError::AlreadyOwned)
    ));
    drop(store);
    // `duplicate` intentionally remains open and shares the kernel lock owner.
    // This failed with close-only cleanup, just as a fork-before-exec copy does.
    let reopened = Store::open(db.path()).unwrap();
    assert!(matches!(
        Store::open(db.path()),
        Err(StoreError::AlreadyOwned)
    ));
    drop(duplicate);
    assert!(matches!(
        Store::open(db.path()),
        Err(StoreError::AlreadyOwned)
    ));
    drop(reopened);
    Store::open(db.path()).unwrap();
}

#[cfg(unix)]
mod inherited_ownership {
    use super::*;
    use std::os::raw::{c_int, c_void};
    unsafe extern "C" {
        fn fork() -> c_int;
        fn pipe(fds: *mut c_int) -> c_int;
        fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
        fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
        fn close(fd: c_int) -> c_int;
        fn waitpid(pid: c_int, status: *mut c_int, flags: c_int) -> c_int;
        fn _exit(code: c_int) -> !;
    }
    struct ForkHold {
        pid: c_int,
        release: c_int,
    }
    impl ForkHold {
        fn start(drop_in_child: Option<OwnershipLock>) -> Self {
            let mut release = [0; 2];
            assert_eq!(unsafe { pipe(release.as_mut_ptr()) }, 0);
            let mut ready = [0; 2];
            assert_eq!(unsafe { pipe(ready.as_mut_ptr()) }, 0);
            let pid = unsafe { fork() };
            assert!(pid >= 0);
            if pid == 0 {
                // Child side uses only stack operations and async-signal-safe
                // FD syscalls. It never touches SQLite/actor locks or allocates.
                unsafe {
                    close(release[1]);
                    close(ready[0]);
                }
                drop(drop_in_child); // PID-guarded unlock + close only
                let mut byte = 1u8;
                unsafe {
                    write(ready[1], (&byte as *const u8).cast(), 1);
                    close(ready[1]);
                    read(release[0], (&mut byte as *mut u8).cast(), 1);
                    close(release[0]);
                    _exit(0);
                }
            }
            unsafe {
                close(release[0]);
                close(ready[1]);
            }
            let guard = Self {
                pid,
                release: release[1],
            };
            let mut byte = 0u8;
            assert_eq!(
                unsafe { read(ready[0], (&mut byte as *mut u8).cast(), 1) },
                1
            );
            unsafe {
                close(ready[0]);
            }
            // In the parent, this is merely an extra descriptor. Avoid invoking
            // a second owner unlock while the original Store is still active.
            if let Some(mut owner) = drop_in_child {
                owner.owner_pid = 0; // no live process has PID zero
                drop(owner);
            }
            guard
        }
    }
    impl Drop for ForkHold {
        fn drop(&mut self) {
            let byte = 1u8;
            unsafe {
                write(self.release, (&byte as *const u8).cast(), 1);
                close(self.release);
            }
            let mut status = 0;
            loop {
                let result = unsafe { waitpid(self.pid, &mut status, 0) };
                if result >= 0
                    || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                {
                    break;
                }
            }
        }
    }
    #[test]
    fn dropping_fork_copy_does_not_unlock_active_parent() {
        let db = TempDatabase::new();
        let store = Store::open(db.path()).unwrap();
        let duplicate = store._ownership.as_ref().unwrap().file.try_clone().unwrap();
        let child = ForkHold::start(Some(OwnershipLock::new(duplicate)));
        // The child already dropped its copied RAII owner and acknowledged it.
        // Its destructor must not issue LOCK_UN against the parent's shared fd.
        assert!(matches!(
            Store::open(db.path()),
            Err(StoreError::AlreadyOwned)
        ));
        drop(child);
        assert!(matches!(
            Store::open(db.path()),
            Err(StoreError::AlreadyOwned)
        ));
        drop(store);
        Store::open(db.path()).unwrap();
    }
    #[test]
    fn shutdown_ack_releases_lock_while_fork_child_still_holds_inherited_fd() {
        let db = TempDatabase::new();
        let core = CoreHandle::open(db.path(), 4).unwrap();
        let child = ForkHold::start(None);
        assert!(matches!(
            Store::open(db.path()),
            Err(StoreError::AlreadyOwned)
        ));
        core.shutdown().unwrap();
        // No sleep/retry: child remains blocked on the release pipe here.
        let reopened = Store::open(db.path()).unwrap();
        assert!(matches!(
            Store::open(db.path()),
            Err(StoreError::AlreadyOwned)
        ));
        drop(child);
        assert!(matches!(
            Store::open(db.path()),
            Err(StoreError::AlreadyOwned)
        ));
        drop(reopened);
        Store::open(db.path()).unwrap();
    }
}
