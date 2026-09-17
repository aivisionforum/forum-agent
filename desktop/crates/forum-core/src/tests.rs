use super::*;
use std::path::PathBuf;

struct Fixture {
    session: SessionSpec,
    track: TrackSpec,
    capture: Event<CaptureSegmentClosed>,
    final_event: Event<TranscriptFinal>,
}

impl Fixture {
    fn new() -> Self {
        let session = SessionSpec {
            session_id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            room_id: Uuid::new_v4(),
            owner_device_id: Uuid::new_v4(),
            title: "测试会议".into(),
        };
        let track = TrackSpec {
            track_id: Uuid::new_v4(),
            session_id: session.session_id,
            kind: TrackKind::RoomMix,
            sample_rate: 16000,
        };
        let audio = AudioRange {
            start_sample: 32000,
            end_sample: 80000,
            sample_rate: 16000,
            start_ms: 2000,
            end_ms: 5000,
        };
        let segment_id = Uuid::new_v4();
        let capture = Event {
            schema_version: 1,
            message_id: Uuid::new_v4(),
            event_type: EventType::AudioSegmentClosed,
            event_id: session.event_id,
            room_id: session.room_id,
            session_id: session.session_id,
            producer: Producer {
                name: "capture".into(),
                run_id: Uuid::new_v4(),
                seq: 1,
            },
            payload: CaptureSegmentClosed {
                track_id: track.track_id,
                segment_id,
                audio: audio.clone(),
                recording_ref: None,
            },
        };
        let final_event = Event {
            schema_version: 1,
            message_id: Uuid::new_v4(),
            event_type: EventType::TranscriptFinal,
            event_id: session.event_id,
            room_id: session.room_id,
            session_id: session.session_id,
            producer: Producer {
                name: "asr".into(),
                run_id: Uuid::new_v4(),
                seq: 1,
            },
            payload: TranscriptFinal {
                track_id: track.track_id,
                segment_id,
                revision: Revision::FIRST,
                audio,
                text: "先测试📝字幕".into(),
                configured_source_language: "auto".into(),
                detected_language: Some("zh".into()),
                target_languages: vec!["en".into()],
                direction_epoch: 1,
                speaker_id: None,
                status: TranscriptStatus::Success,
                reason: None,
                backend: "test-asr".into(),
                model_manifest_id: "test-v1".into(),
            },
        };
        Self {
            session,
            track,
            capture,
            final_event,
        }
    }

    fn setup(&self, store: &mut Store) {
        store.create_session(&self.session).unwrap();
        store.create_track(&self.track).unwrap();
        // These transaction tests isolate ingestion. Lifecycle has dedicated
        // end-to-end tests using versioned transitions in reliable_tests.
        store
            .connection
            .execute(
                "UPDATE sessions SET state='recording' WHERE id=?1",
                [self.session.session_id.to_string()],
            )
            .unwrap();
    }

    fn revision(&self, expected: Revision, text: &str) -> Event<TranscriptRevision> {
        Event {
            schema_version: 1,
            message_id: Uuid::new_v4(),
            event_type: EventType::TranscriptRevised,
            event_id: self.session.event_id,
            room_id: self.session.room_id,
            session_id: self.session.session_id,
            producer: Producer {
                name: "operator".into(),
                run_id: Uuid::new_v4(),
                seq: 1,
            },
            payload: TranscriptRevision {
                segment_id: self.final_event.payload.segment_id,
                expected_revision: expected,
                text: text.into(),
                reason: "纠正术语".into(),
                operator_id: "local-operator".into(),
            },
        }
    }
}

struct TempDatabase(PathBuf);
impl TempDatabase {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("forum-store-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("forum.sqlite")
    }
}
impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn final_persists_before_translation_and_creates_atomic_coverage_and_outbox() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    let capture = store.register_capture(&f.capture).unwrap();
    let pending = store.session_snapshot(f.session.session_id).unwrap();
    assert_eq!(pending.cursor, capture.store_seq);
    assert!(pending.transcript.is_empty());
    assert_eq!(pending.incomplete.len(), 1);
    assert_eq!(pending.incomplete[0].status, None);
    let receipt = store.ingest_final(&f.final_event).unwrap();
    let snapshot = store.session_snapshot(f.session.session_id).unwrap();
    assert_eq!(snapshot.cursor, receipt.store_seq);
    assert_eq!(
        snapshot.transcript[0].payload.text,
        f.final_event.payload.text
    );
    assert!(snapshot.incomplete.is_empty());
    let coverage = store.coverage(f.session.session_id).unwrap();
    assert_eq!(coverage.len(), 1);
    assert_eq!(coverage[0].state, "pending");
    assert_eq!(coverage[0].start_utf8, 0);
    assert_eq!(coverage[0].end_utf8, f.final_event.payload.text.len());
    assert_eq!(coverage[0].target_language, "en");
    let events = store.outbox_after(capture.store_seq, 100).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].message_id, f.final_event.message_id);
    assert_eq!(
        events[0].body["payload"]["text"],
        f.final_event.payload.text
    );
}

#[test]
fn exact_replay_returns_original_receipt_without_reapplying() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    let capture = store.register_capture(&f.capture).unwrap();
    assert_eq!(
        store.register_capture(&f.capture).unwrap().store_seq,
        capture.store_seq
    );
    let final_receipt = store.ingest_final(&f.final_event).unwrap();
    let replay = store.ingest_final(&f.final_event).unwrap();
    assert!(replay.duplicate);
    assert_eq!(replay.store_seq, final_receipt.store_seq);
    assert_eq!(store.outbox_after(0, 100).unwrap().len(), 2);
    assert_eq!(store.coverage(f.session.session_id).unwrap().len(), 1);
}

#[test]
fn reused_message_and_producer_sequence_cannot_change_body_or_identity() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    store.register_capture(&f.capture).unwrap();
    store.ingest_final(&f.final_event).unwrap();
    let mut changed = f.final_event.clone();
    changed.payload.text = "不同原文".into();
    assert!(matches!(
        store.ingest_final(&changed),
        Err(StoreError::EventIdConflict)
    ));
    changed = f.final_event.clone();
    changed.message_id = Uuid::new_v4();
    assert!(matches!(
        store.ingest_final(&changed),
        Err(StoreError::EventIdConflict)
    ));
    changed = f.final_event.clone();
    changed.producer.run_id = Uuid::new_v4();
    assert!(matches!(
        store.ingest_final(&changed),
        Err(StoreError::EventIdConflict)
    ));
    assert_eq!(
        store
            .session_snapshot(f.session.session_id)
            .unwrap()
            .transcript[0]
            .payload
            .text,
        f.final_event.payload.text
    );
    assert_eq!(store.outbox_after(0, 100).unwrap().len(), 2);
}

#[test]
fn final_requires_capture_and_exact_scope_audio_and_first_revision() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    assert!(matches!(
        store.ingest_final(&f.final_event),
        Err(StoreError::DependencyNotReady)
    ));
    assert!(store.outbox_after(0, 100).unwrap().is_empty());
    store.register_capture(&f.capture).unwrap();
    let mut wrong = f.final_event.clone();
    wrong.payload.audio.end_sample += 1;
    assert!(matches!(
        store.ingest_final(&wrong),
        Err(StoreError::ScopeMismatch)
    ));
    wrong = f.final_event.clone();
    wrong.room_id = Uuid::new_v4();
    assert!(matches!(
        store.ingest_final(&wrong),
        Err(StoreError::ScopeMismatch)
    ));
    wrong = f.final_event.clone();
    wrong.payload.revision = Revision::try_from(2).unwrap();
    assert!(matches!(
        store.ingest_final(&wrong),
        Err(StoreError::RevisionConflict { .. })
    ));
    // Failed attempts did not reserve this message ID or producer sequence.
    let receipt = store.ingest_final(&f.final_event).unwrap();
    assert_eq!(receipt.store_seq, 2);
}

#[test]
fn cross_session_capture_final_revision_and_evidence_are_rejected() {
    let mut store = Store::in_memory().unwrap();
    let a = Fixture::new();
    a.setup(&mut store);
    let b = Fixture::new();
    b.setup(&mut store);
    let mut wrong_capture = a.capture.clone();
    wrong_capture.payload.track_id = b.track.track_id;
    assert!(matches!(
        store.register_capture(&wrong_capture),
        Err(StoreError::ScopeMismatch)
    ));
    store.register_capture(&a.capture).unwrap();
    let mut wrong_final = a.final_event.clone();
    wrong_final.session_id = b.session.session_id;
    wrong_final.event_id = b.session.event_id;
    wrong_final.room_id = b.session.room_id;
    assert!(matches!(
        store.ingest_final(&wrong_final),
        Err(StoreError::ScopeMismatch)
    ));
    store.ingest_final(&a.final_event).unwrap();
    let mut wrong_revision = a.revision(Revision::FIRST, "修订");
    wrong_revision.session_id = b.session.session_id;
    wrong_revision.event_id = b.session.event_id;
    wrong_revision.room_id = b.session.room_id;
    assert!(matches!(
        store.revise_transcript(&wrong_revision),
        Err(StoreError::ScopeMismatch)
    ));
    let span = SourceSpan {
        segment_id: a.final_event.payload.segment_id,
        segment_revision: Revision::FIRST,
        start_utf8: 9,
        end_utf8: 13,
        quote: "📝".into(),
    };
    assert_eq!(
        store.resolve_span(a.session.session_id, &span).unwrap(),
        "📝"
    );
    assert!(matches!(
        store.resolve_span(b.session.session_id, &span),
        Err(StoreError::ScopeMismatch)
    ));
}

#[test]
fn human_revision_keeps_history_and_invalidates_old_translation_coverage() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    store.register_capture(&f.capture).unwrap();
    store.ingest_final(&f.final_event).unwrap();
    let edit = f.revision(Revision::FIRST, "修正📝术语");
    let receipt = store.revise_transcript(&edit).unwrap();
    let current = store.session_snapshot(f.session.session_id).unwrap();
    assert_eq!(current.transcript[0].payload.revision.get(), 2);
    assert_eq!(current.transcript[0].payload.text, edit.payload.text);
    assert!(
        matches!(&current.transcript[0].origin, RevisionOrigin::Human { operator_id, .. } if operator_id == "local-operator")
    );
    let original = store
        .transcript_revision(
            f.session.session_id,
            f.capture.payload.segment_id,
            Revision::FIRST,
        )
        .unwrap();
    assert_eq!(original.payload.text, f.final_event.payload.text);
    let coverage = store.coverage(f.session.session_id).unwrap();
    assert_eq!(coverage.len(), 2);
    assert_eq!(coverage[0].state, "stale");
    assert_eq!(coverage[1].state, "pending");
    assert_eq!(coverage[1].end_utf8, edit.payload.text.len());
    assert_eq!(
        store.revise_transcript(&edit).unwrap().store_seq,
        receipt.store_seq
    );
    let conflict = f.revision(Revision::FIRST, "另一编辑");
    assert!(matches!(
        store.revise_transcript(&conflict),
        Err(StoreError::RevisionConflict {
            actual: Some(2),
            ..
        })
    ));
    assert_eq!(store.outbox_after(0, 100).unwrap().len(), 3);
    // Late delivery of the original ASR event acknowledges it without replacing the edit.
    assert!(store.ingest_final(&f.final_event).unwrap().duplicate);
    assert_eq!(
        store
            .session_snapshot(f.session.session_id)
            .unwrap()
            .transcript[0]
            .payload
            .text,
        edit.payload.text
    );
}

#[test]
fn empty_and_failed_results_remain_visible_as_gaps_not_normal_transcript() {
    for status in [TranscriptStatus::Empty, TranscriptStatus::Failed] {
        let mut store = Store::in_memory().unwrap();
        let mut f = Fixture::new();
        f.setup(&mut store);
        f.final_event.payload.text.clear();
        f.final_event.payload.status = status;
        f.final_event.payload.reason = Some("ASR_NO_RESULT".into());
        store.register_capture(&f.capture).unwrap();
        store.ingest_final(&f.final_event).unwrap();
        let snapshot = store.session_snapshot(f.session.session_id).unwrap();
        assert!(snapshot.transcript.is_empty());
        assert_eq!(snapshot.incomplete[0].status, Some(status));
        assert!(store.coverage(f.session.session_id).unwrap().is_empty());
        store
            .revise_transcript(&f.revision(Revision::FIRST, "人工补录原文"))
            .unwrap();
        assert_eq!(
            store
                .session_snapshot(f.session.session_id)
                .unwrap()
                .transcript
                .len(),
            1
        );
        assert_eq!(store.coverage(f.session.session_id).unwrap().len(), 1);
    }
}

#[test]
fn empty_success_and_duplicate_targets_are_rejected_before_writing() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    store.register_capture(&f.capture).unwrap();
    let mut invalid = f.final_event.clone();
    invalid.payload.text = " \n".into();
    assert!(matches!(
        store.ingest_final(&invalid),
        Err(StoreError::Validation(_))
    ));
    invalid = f.final_event.clone();
    invalid.payload.target_languages.push("en".into());
    assert!(matches!(
        store.ingest_final(&invalid),
        Err(StoreError::Validation(_))
    ));
    assert_eq!(store.outbox_after(0, 100).unwrap().len(), 1);
}

#[test]
fn real_sql_failure_after_projection_rolls_back_receipt_revision_coverage_and_outbox() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    store.register_capture(&f.capture).unwrap();
    store.connection.execute_batch("CREATE TEMP TRIGGER fail_outbox BEFORE INSERT ON outbox
        WHEN NEW.event_type='transcript.final' BEGIN SELECT RAISE(ABORT,'injected disk/write failure'); END;").unwrap();
    assert!(matches!(
        store.ingest_final(&f.final_event),
        Err(StoreError::Database(_))
    ));
    let snapshot = store.session_snapshot(f.session.session_id).unwrap();
    assert_eq!(snapshot.cursor, 1);
    assert!(snapshot.transcript.is_empty());
    assert_eq!(snapshot.incomplete.len(), 1);
    assert!(store.coverage(f.session.session_id).unwrap().is_empty());
    assert_eq!(store.outbox_after(0, 100).unwrap().len(), 1);
    store
        .connection
        .execute_batch("DROP TRIGGER fail_outbox;")
        .unwrap();
    let receipt = store.ingest_final(&f.final_event).unwrap();
    assert!(!receipt.duplicate);
    assert_eq!(receipt.store_seq, 2);
}

#[test]
fn reopening_recovers_committed_state_and_ack_lost_replay() {
    let database = TempDatabase::new();
    let f = Fixture::new();
    let receipt;
    {
        let mut store = Store::open(database.path()).unwrap();
        f.setup(&mut store);
        store.register_capture(&f.capture).unwrap();
        receipt = store.ingest_final(&f.final_event).unwrap();
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "synchronous", |row| row.get::<_, u32>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .connection
                .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                .unwrap(),
            "wal"
        );
    }
    let mut reopened = Store::open(database.path()).unwrap();
    let duplicate = reopened.ingest_final(&f.final_event).unwrap();
    assert!(duplicate.duplicate);
    assert_eq!(duplicate.store_seq, receipt.store_seq);
    assert_eq!(
        reopened
            .session_snapshot(f.session.session_id)
            .unwrap()
            .transcript
            .len(),
        1
    );
    assert_eq!(reopened.coverage(f.session.session_id).unwrap().len(), 1);
    assert_eq!(reopened.outbox_after(0, 100).unwrap().len(), 3);
}

#[test]
fn future_schema_is_rejected_without_downgrading() {
    let database = TempDatabase::new();
    let connection = Connection::open(database.path()).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);
    assert!(matches!(
        Store::open(database.path()),
        Err(StoreError::UnsupportedDatabase(99))
    ));
    let connection = Connection::open(database.path()).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        99
    );
}

#[test]
fn failed_catalog_scope_creation_is_atomic_and_conflicting_ids_are_rejected() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    f.setup(&mut store);
    let mut conflicting = f.session.clone();
    conflicting.title = "different".into();
    assert!(matches!(
        store.create_session(&conflicting),
        Err(StoreError::EntityConflict)
    ));
    let mut other = Fixture::new();
    other.session.room_id = f.session.room_id;
    assert!(matches!(
        store.create_session(&other.session),
        Err(StoreError::ScopeMismatch)
    ));
    let count: u64 = store
        .connection
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1); // attempted new event was rolled back too
    let mut bad_track = f.track.clone();
    bad_track.sample_rate = 48000;
    assert!(matches!(
        store.create_track(&bad_track),
        Err(StoreError::EntityConflict)
    ));
}

#[test]
fn snapshot_cursor_can_resume_internal_events_and_allows_real_track_overlap() {
    let mut store = Store::in_memory().unwrap();
    let a = Fixture::new();
    a.setup(&mut store);
    store.register_capture(&a.capture).unwrap();
    store.ingest_final(&a.final_event).unwrap();
    let before = store.session_snapshot(a.session.session_id).unwrap();
    let mut b = Fixture::new();
    b.session = a.session.clone();
    b.track.session_id = a.session.session_id;
    b.capture.session_id = a.session.session_id;
    b.capture.event_id = a.session.event_id;
    b.capture.room_id = a.session.room_id;
    b.final_event.session_id = a.session.session_id;
    b.final_event.event_id = a.session.event_id;
    b.final_event.room_id = a.session.room_id;
    b.setup(&mut store);
    store.register_capture(&b.capture).unwrap();
    store.ingest_final(&b.final_event).unwrap();
    let events = store.outbox_after(before.cursor, 100).unwrap();
    assert_eq!(events.len(), 2);
    let after = store.session_snapshot(a.session.session_id).unwrap();
    assert_eq!(after.transcript.len(), 2);
    assert_eq!(after.cursor, events.last().unwrap().store_seq);
    assert_eq!(
        after.transcript[0].payload.audio,
        after.transcript[1].payload.audio
    );
}

#[test]
fn event_payload_type_schema_and_unknown_wire_fields_are_checked() {
    let mut store = Store::in_memory().unwrap();
    let f = Fixture::new();
    f.setup(&mut store);
    let mut invalid = f.capture.clone();
    invalid.schema_version = 2;
    assert!(matches!(
        store.register_capture(&invalid),
        Err(StoreError::Validation(ValidationError::UnsupportedSchema))
    ));
    invalid = f.capture.clone();
    invalid.event_type = EventType::TranscriptFinal;
    assert!(matches!(
        store.register_capture(&invalid),
        Err(StoreError::Validation(ValidationError::EventTypeMismatch))
    ));
    let mut wire = serde_json::to_value(&f.final_event).unwrap();
    wire["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<Event<TranscriptFinal>>(wire).is_err());
    assert!(store.outbox_after(0, 100).unwrap().is_empty());
}

#[path = "reliable_tests.rs"]
mod reliable_tests;

#[path = "analysis_tests.rs"]
mod analysis_tests;
