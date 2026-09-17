use super::*;
use forum_runtime::RuntimeClient;

fn fixture() -> (PathBuf, MeetingRepository, MeetingHost) {
    let root = PathBuf::from("/tmp").join(format!("forum-host-test-{}", Uuid::new_v4()));
    let repo = MeetingRepository::open(root.clone()).unwrap();
    let host = MeetingHost::create(
        repo.clone(),
        MeetingOptions {
            max_segment_ms: 10_000,            source_language: "auto".into(),
            target_language: "bilingual".into(),
            recording_enabled: true,
            system_audio: false,
        },
        None,
    )
    .unwrap();
    (root, repo, host)
}
fn ready(host: &mut MeetingHost) {
    RuntimeClient::new(host.config.endpoint.clone()).call("ready",json!({"session_id":host.id(),"role":"asr","prior_run_records":[],"outbox_replayed":true})).unwrap();
    assert!(host.activate_if_ready().unwrap());
}
#[test]
fn invalid_readiness_and_foreign_scope_do_not_open_capture_barrier() {
    let (root, repo, mut host) = fixture();
    let client = RuntimeClient::new(host.config.endpoint.clone());
    let error = client
        .call("status", json!({"session_id":Uuid::new_v4()}))
        .unwrap_err();
    assert_eq!(error.code, "SCOPE_MISMATCH");
    assert!(client
        .call(
            "ready",
            json!({"session_id":host.id(),"role":"asr","prior_run_records":"broken"})
        )
        .is_err());
    assert!(!host.activate_if_ready().unwrap());
    assert_eq!(host.status().unwrap().state, SessionState::Preparing);
    // Translator alone never authorizes opening an audio device.
    client
        .call("ready", json!({"session_id":host.id(),"role":"translator"}))
        .unwrap();
    assert!(!host.activate_if_ready().unwrap());
    ready(&mut host);
    assert_eq!(host.status().unwrap().state, SessionState::Recording);
    drop(host);
    repo.core.shutdown().unwrap();
    drop(repo);
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn host_seals_durable_source_with_translation_pending_and_reads_it_after_reopen() {
    let (root, repo, mut host) = fixture();
    ready(&mut host);
    let id = host.id();
    let options = host.setup.options.clone();
    let mut capture = moxin_dora_bridge::CaptureSession::open(host.capture_context()).unwrap();
    let pcm = vec![0.25f32; 1600];
    capture.record(&pcm).unwrap();
    let meta = capture.close_segment(&pcm).unwrap();
    let mut asr = DurableProducer::open(host.config.for_producer("asr").unwrap()).unwrap();
    let source = TranscriptFinal {
        track_id: meta.track_id,
        segment_id: meta.segment_id,
        revision: Revision::FIRST,
        audio: meta.audio,
        text: "不要发布这个草稿📝。".into(),
        configured_source_language: "auto".into(),
        detected_language: Some("zh".into()),
        target_languages: vec!["zh".into(), "en".into()],
        direction_epoch: 1,
        speaker_id: None,
        status: TranscriptStatus::Success,
        reason: None,
        backend: "synthetic-host-test".into(),
        model_manifest_id: "no-model-loaded".into(),
    };
    let pending = asr.append(EventType::TranscriptFinal, &source).unwrap();
    asr.flush_one(pending.message_id).unwrap();
    host.mark_stopping().unwrap();
    capture.seal_after_devices_released().unwrap();
    // Closed audio alone cannot stand in for the producer's final acknowledgement.
    assert!(!host.try_seal().unwrap());
    let seal = ProducerSeal {
        producer_run_id: asr.run_id(),
        final_seq: asr.last_seq(),
        segment_ids: vec![meta.segment_id],
    };
    let pending = asr.append(EventType::ProducerSealed, &seal).unwrap();
    asr.flush_one(pending.message_id).unwrap();
    assert!(host.try_seal().unwrap());
    assert!(host.try_seal().unwrap());
    assert!(!host.status().unwrap().incomplete);
    assert_eq!(host.translation_pending().unwrap(), 2);
    let page = repo.page(id, None, None).unwrap();
    assert_eq!(
        page["items"][0]["transcript"]["payload"]["text"],
        source.text
    );
    let markdown = repo.export_markdown(id).unwrap();
    assert!(markdown.contains(&source.text));
    drop(capture);
    drop(asr);
    drop(host);
    repo.core.shutdown().unwrap();
    drop(repo);
    let repo = MeetingRepository::open(root.clone()).unwrap();
    assert_eq!(
        repo.page(id, None, None).unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let mut recovery = MeetingHost::create(repo.clone(), options, Some(id)).unwrap();
    let client = RuntimeClient::new(recovery.config.endpoint.clone());
    let status = client.call("status", json!({"session_id":id})).unwrap();
    assert_eq!(status["capture_dispatch_complete"], false);
    // Recovery is an explicit file-only operation, even if the original track was a mic.
    assert!(recovery.capture_context().replay_only);
    let receipt = json!({"session_id":id,"segments":[]});
    client
        .call("capture_dispatch_complete", receipt.clone())
        .unwrap();
    client.call("capture_dispatch_complete", receipt).unwrap();
    assert_eq!(
        client.call("status", json!({"session_id":id})).unwrap()["capture_dispatch_complete"],
        true
    );
    assert!(client
        .call(
            "capture_dispatch_complete",
            json!({"session_id":id,"segments":[{"segment_id":meta.segment_id,"revision":1}]})
        )
        .is_err());
    assert!(recovery.try_seal().unwrap());
    drop(recovery);
    repo.core.shutdown().unwrap();
    drop(repo);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sealed_failed_source_recovery_waits_for_new_revision_and_producer_ack() {
    let (root, repo, mut host) = fixture();
    ready(&mut host);
    let id = host.id();
    let options = host.setup.options.clone();
    let mut capture = moxin_dora_bridge::CaptureSession::open(host.capture_context()).unwrap();
    let pcm = vec![0.2; 1600];
    capture.record(&pcm).unwrap();
    let meta = capture.close_segment(&pcm).unwrap();
    let mut asr = DurableProducer::open(host.config.for_producer("asr").unwrap()).unwrap();
    let mut source = TranscriptFinal {
        track_id: meta.track_id,
        segment_id: meta.segment_id,
        revision: Revision::FIRST,
        audio: meta.audio,
        text: String::new(),
        configured_source_language: "auto".into(),
        detected_language: None,
        target_languages: vec!["zh".into(), "en".into()],
        direction_epoch: 1,
        speaker_id: None,
        status: TranscriptStatus::Failed,
        reason: Some("synthetic failure".into()),
        backend: "synthetic-host-test".into(),
        model_manifest_id: "no-model-loaded".into(),
    };
    let pending = asr.append(EventType::TranscriptFinal, &source).unwrap();
    asr.flush_one(pending.message_id).unwrap();
    host.mark_stopping().unwrap();
    capture.seal_after_devices_released().unwrap();
    let seal = ProducerSeal {
        producer_run_id: asr.run_id(),
        final_seq: asr.last_seq(),
        segment_ids: vec![meta.segment_id],
    };
    let pending = asr.append(EventType::ProducerSealed, &seal).unwrap();
    asr.flush_one(pending.message_id).unwrap();
    assert!(host.try_seal().unwrap());
    assert!(host.status().unwrap().incomplete);
    drop(capture);
    drop(asr);
    drop(host);
    let mut host = MeetingHost::create(repo.clone(), options, Some(id)).unwrap();
    ready(&mut host);
    let client = RuntimeClient::new(host.config.endpoint.clone());
    assert!(!host.try_seal().unwrap());
    client
        .call(
            "capture_dispatch_complete",
            json!({"session_id":id,"segments":[{"segment_id":meta.segment_id,"revision":2}]}),
        )
        .unwrap();
    assert!(!host.try_seal().unwrap());
    let mut asr = DurableProducer::open(host.config.for_producer("asr").unwrap()).unwrap();
    source.revision = 2.try_into().unwrap();
    source.reason = None;
    source.status = TranscriptStatus::Success;
    source.text = "恢复后的原文".into();
    source.detected_language = Some("zh".into());
    let pending = asr.append(EventType::TranscriptFinal, &source).unwrap();
    asr.flush_one(pending.message_id).unwrap();
    assert!(!host.try_seal().unwrap());
    let seal = ProducerSeal {
        producer_run_id: asr.run_id(),
        final_seq: asr.last_seq(),
        segment_ids: vec![meta.segment_id],
    };
    let pending = asr.append(EventType::ProducerSealed, &seal).unwrap();
    asr.flush_one(pending.message_id).unwrap();
    assert!(host.try_seal().unwrap());
    assert!(!host.status().unwrap().incomplete);
    drop(asr);
    drop(host);
    repo.core.shutdown().unwrap();
    drop(repo);
    fs::remove_dir_all(root).unwrap();
}
