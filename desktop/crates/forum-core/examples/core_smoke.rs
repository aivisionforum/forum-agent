//! Synthetic persistence demonstration. Does not open an audio device/model.
use forum_contracts::*;
use forum_core::Store;

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("forum-core-smoke-{}.sqlite", Uuid::new_v4()))
        });
    let session = SessionSpec {
        session_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        room_id: Uuid::new_v4(),
        owner_device_id: Uuid::new_v4(),
        title: "合成数据持久化验证".into(),
    };
    let track = TrackSpec {
        track_id: Uuid::new_v4(),
        session_id: session.session_id,
        kind: TrackKind::Replay,
        sample_rate: 16000,
    };
    let audio = AudioRange {
        start_sample: 0,
        end_sample: 48000,
        sample_rate: 16000,
        start_ms: 0,
        end_ms: 3000,
    };
    let segment_id = Uuid::new_v4();
    let capture = Event {
        schema_version: SCHEMA_VERSION,
        message_id: Uuid::new_v4(),
        event_type: EventType::AudioSegmentClosed,
        event_id: session.event_id,
        room_id: session.room_id,
        session_id: session.session_id,
        producer: Producer {
            name: "synthetic-capture".into(),
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
        schema_version: SCHEMA_VERSION,
        message_id: Uuid::new_v4(),
        event_type: EventType::TranscriptFinal,
        event_id: session.event_id,
        room_id: session.room_id,
        session_id: session.session_id,
        producer: Producer {
            name: "synthetic-asr".into(),
            run_id: Uuid::new_v4(),
            seq: 1,
        },
        payload: TranscriptFinal {
            track_id: track.track_id,
            segment_id,
            revision: Revision::FIRST,
            audio,
            text: "本地字幕📝已经保存。".into(),
            configured_source_language: "auto".into(),
            detected_language: Some("zh".into()),
            target_languages: vec!["en".into()],
            direction_epoch: 1,
            speaker_id: None,
            status: TranscriptStatus::Success,
            reason: None,
            backend: "synthetic-example".into(),
            model_manifest_id: "no-model-used".into(),
        },
    };
    let edit = Event {
        schema_version: SCHEMA_VERSION,
        message_id: Uuid::new_v4(),
        event_type: EventType::TranscriptRevised,
        event_id: session.event_id,
        room_id: session.room_id,
        session_id: session.session_id,
        producer: Producer {
            name: "synthetic-operator".into(),
            run_id: Uuid::new_v4(),
            seq: 1,
        },
        payload: TranscriptRevision {
            segment_id,
            expected_revision: Revision::FIRST,
            text: "本地字幕📝已持续保存。".into(),
            reason: "合成演示中的文本修订".into(),
            operator_id: "demo-operator".into(),
        },
    };
    let mut store = Store::open(&path)?;
    store.create_session(&session)?;
    store.create_track(&track)?;
    store.register_capture(&capture)?;
    let final_receipt = store.ingest_final(&final_event)?;
    store.revise_transcript(&edit)?;
    drop(store);

    let mut reopened = Store::open(&path)?;
    let replay = reopened.ingest_final(&final_event)?;
    assert!(replay.duplicate && replay.store_seq == final_receipt.store_seq);
    let snapshot = reopened.session_snapshot(session.session_id)?;
    assert_eq!(snapshot.transcript[0].payload.revision.get(), 2);
    assert_eq!(snapshot.transcript[0].payload.text, edit.payload.text);
    assert_eq!(
        reopened
            .transcript_revision(session.session_id, segment_id, Revision::FIRST)?
            .payload
            .text,
        final_event.payload.text
    );
    let coverage = reopened.coverage(session.session_id)?;
    assert_eq!(coverage.len(), 2);
    assert_eq!(coverage[0].state, "stale");
    assert_eq!(coverage[1].state, "pending");
    eprintln!("Synthetic database: {}", path.display());
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}
