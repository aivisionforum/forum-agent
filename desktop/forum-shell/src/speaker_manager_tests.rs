use super::*;
use forum_core::{RevisionOrigin, Store};
use forum_runtime::{audio::SegmentMeta, recording::RecordingJournal};
use std::os::unix::fs::{symlink, DirBuilderExt};
struct Fixture {
    root: PathBuf,
    core: CoreHandle,
    candidate: Candidate,
    session: SessionSpec,
    track: TrackSpec,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.core.shutdown();
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    fn new(dual: bool, recording: bool) -> Self {
        let root = std::env::temp_dir().join(format!("speaker-host-{}", Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let session = SessionSpec {
            session_id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            room_id: Uuid::new_v4(),
            owner_device_id: Uuid::new_v4(),
            title: "synthetic speaker host test".into(),
        };
        let track = TrackSpec {
            track_id: Uuid::new_v4(),
            session_id: session.session_id,
            kind: if dual {
                TrackKind::System
            } else {
                TrackKind::Mic
            },
            sample_rate: 16000,
        };
        let audio = AudioRange {
            start_sample: 0,
            end_sample: 48000,
            sample_rate: 16000,
            start_ms: 0,
            end_ms: 3000,
        };
        let segment = Uuid::new_v4();
        let payload = TranscriptFinal {
            track_id: track.track_id,
            segment_id: segment,
            revision: Revision::FIRST,
            audio: audio.clone(),
            text: "合成输入用于验证宿主范围，不代表真人说话人准确率".into(),
            configured_source_language: "zh".into(),
            detected_language: Some("zh".into()),
            target_languages: vec!["en".into()],
            direction_epoch: 1,
            speaker_id: None,
            status: TranscriptStatus::Success,
            reason: None,
            backend: "host-test".into(),
            model_manifest_id: "host-test".into(),
        };
        let mut store = Store::open(root.join("core.sqlite")).unwrap();
        store.create_session(&session).unwrap();
        store.create_track(&track).unwrap();
        let event = |kind, name: &str, data| Event {
            schema_version: 1,
            message_id: Uuid::new_v4(),
            event_type: kind,
            event_id: session.event_id,
            room_id: session.room_id,
            session_id: session.session_id,
            producer: Producer {
                name: name.into(),
                run_id: Uuid::new_v4(),
                seq: 1,
            },
            payload: data,
        };
        // Reliable lifecycle only, no real capture or microphone permissions.
        for next in [
            SessionState::Preparing,
            SessionState::Ready,
            SessionState::Recording,
        ] {
            let state = store.session_status(session.session_id).unwrap();
            store
                .transition_session(&event(
                    EventType::SessionChanged,
                    "host",
                    SessionTransition {
                        expected_state: state.state,
                        next_state: next,
                        reason: "synthetic test".into(),
                    },
                ))
                .unwrap();
        }
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
                segment_id: segment,
                audio: audio.clone(),
                recording_ref: None,
            },
        };
        store.register_capture(&capture).unwrap();
        store
            .ingest_final(&Event {
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
                payload: payload.clone(),
            })
            .unwrap();
        drop(store);
        let core = CoreHandle::open(root.join("core.sqlite"), 32).unwrap();
        let candidate = Candidate {
            record: TranscriptRecord {
                session_id: session.session_id,
                payload,
                origin: RevisionOrigin::Asr,
                created_seq: 2,
            },
            expected_assignment: None,
        };
        let audio_root = root
            .join("sessions")
            .join(session.session_id.to_string())
            .join("audio");
        let directory = if dual {
            audio_root.join("tracks").join(track.track_id.to_string())
        } else {
            audio_root
        };
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let mut journal = RecordingJournal::create(directory, track.clone()).unwrap();
        let meta = SegmentMeta {
            session_id: session.session_id,
            track_id: track.track_id,
            segment_id: segment,
            revision: Revision::FIRST,
            audio: audio.clone(),
            configured_source_language: "zh".into(),
            target_languages: vec!["en".into()],
            direction_epoch: 1,
            partial_seq: 0,
            final_segment: true,
        };
        let samples: Vec<f32> = (0..48000).map(|n| (n as f32 * 0.05).sin() * 0.2).collect();
        journal.append_pcm(&samples).unwrap();
        journal.close_segment(&meta, &samples, recording).unwrap();
        Self {
            root,
            core,
            candidate,
            session,
            track,
        }
    }
    fn directory(&self) -> PathBuf {
        let base = self
            .root
            .join("sessions")
            .join(self.session.session_id.to_string())
            .join("audio");
        let dual = base.join("tracks").join(self.track.track_id.to_string());
        if dual.exists() {
            dual
        } else {
            base
        }
    }
    fn inner(&self) -> Arc<Inner> {
        Arc::new(Inner {
            core: self.core.clone(),
            root: self.root.clone(),
            resources: None,
            budget: ResourceBudget::default(),
            state: Mutex::new(SpeakerStatus {
                enabled: true,
                session_id: Some(self.session.session_id),
                ..SpeakerStatus::default()
            }),
            model_path: Mutex::new(None),
            changed: Condvar::new(),
            generation: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            contained: AtomicBool::new(true),
            pending_commits: AtomicU64::new(0),
        })
    }
    fn model(&self, mode: &str) -> ModelSetup {
        let source = self.root.join("fake-worker");
        fs::create_dir_all(source.join("forum_speaker_worker")).unwrap();
        let code = format!(
            r#"import sys,json,time,pathlib,os
mode={mode:?}
for line in sys.stdin:
 r=json.loads(line);m=r['method'];p=r['params'];i=r['id']
 if m=='initialize':
  root=pathlib.Path(p['job_root']);manifest=p['model_grant']['model_manifest_id'];out={{'protocol_version':1}}
 elif m=='jobs.run':
  (root/'entered').write_text(str(os.getpid()))
  if mode=='hang': time.sleep(60)
  if mode=='delay': time.sleep(.5)
  out={{'schema_version':1,'job_id':p['job_id'],'attempt':p['attempt'],'session_id':p['session_id'],'track_id':p['track_id'],'segment_id':p['segment_id'],'segment_revision':p['segment_revision'],'model_manifest_id':manifest,'pcm_sha256':p['pcm']['sha256'],'status':'embedding','reason':None,'embedding':[1.0]+[0.0]*191,'quality':{{'duration_seconds':3.0,'rms':0.14,'clipping_ratio':0.0}}}}
 elif m=='shutdown': break
 else: out={{}}
 print(json.dumps({{'jsonrpc':'2.0','id':i,'result':out}}),flush=True)
"#
        );
        fs::write(source.join("forum_speaker_worker/__main__.py"), code).unwrap();
        ModelSetup {
            python: PathBuf::from("/usr/bin/python3"),
            source: Some(source),
            model: self.root.clone(),
            manifest: format!("sha256:{}", "a".repeat(64)),
        }
    }
}
fn wait_until(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < deadline, "condition timeout");
        thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn private_pcm_single_and_dual_track_are_exact_and_reject_redirection() {
    for dual in [false, true] {
        let f = Fixture::new(dual, true);
        let bytes = read_pcm(&f.root, &f.candidate).unwrap();
        assert_eq!(bytes.len(), 96000);
        let file = f.directory().join(format!(
            "segment-{}.f32le",
            f.candidate.record.payload.segment_id
        ));
        fs::remove_file(&file).unwrap();
        symlink("/etc/passwd", &file).unwrap();
        assert!(read_pcm(&f.root, &f.candidate).is_err());
    }
}
#[test]
fn disabled_recording_and_mismatched_source_never_spawn_or_infer() {
    let f = Fixture::new(false, false);
    let model = f.model("ok");
    let inner = f.inner();
    assert!(process_candidate(&inner, 1, &model, &f.candidate).is_err());
    assert!(!f.root.join("speaker-jobs").exists());
    let f = Fixture::new(true, true);
    let mut candidate = f.candidate.clone();
    candidate.record.payload.audio.end_sample -= 1;
    assert!(read_pcm(&f.root, &candidate).is_err());
}
#[test]
fn result_requires_exact_scope_real_shape_and_quality() {
    let f = Fixture::new(false, true);
    let model = f.model("ok");
    let job = Uuid::new_v4();
    let mut value = json!({"schema_version":1,"job_id":job,"attempt":1,"session_id":f.session.session_id,"track_id":f.track.track_id,"segment_id":f.candidate.record.payload.segment_id,"segment_revision":1,"model_manifest_id":model.manifest,"pcm_sha256":"f".repeat(64),"status":"embedding","reason":null,"embedding":([1.0].into_iter().chain(std::iter::repeat_n(0.0,191)).collect::<Vec<_>>()),"quality":{"duration_seconds":3.0,"rms":0.14,"clipping_ratio":0.0}});
    assert!(validate_result(
        value.clone(),
        job,
        &f.candidate,
        &model,
        &"f".repeat(64),
        48000
    )
    .is_ok());
    value["quality"]["rms"] = json!(0.0);
    assert!(validate_result(
        value.clone(),
        job,
        &f.candidate,
        &model,
        &"f".repeat(64),
        48000
    )
    .is_err());
    value["status"] = "unknown".into();
    value["embedding"] = Value::Null;
    value["reason"] = "quiet_or_silent".into();
    assert!(validate_result(
        value.clone(),
        job,
        &f.candidate,
        &model,
        &"f".repeat(64),
        48000
    )
    .is_ok());
    value["segment_id"] = json!(Uuid::new_v4());
    assert!(validate_result(value, job, &f.candidate, &model, &"f".repeat(64), 48000).is_err());
}
#[test]
fn host_worker_round_trip_commits_only_after_process_exit_and_deletes_copy() {
    let f = Fixture::new(true, true);
    let inner = f.inner();
    let model = f.model("ok");
    process_candidate(&inner, 1, &model, &f.candidate).unwrap();
    let session = f.session.session_id;
    let segment = f.candidate.record.payload.segment_id;
    let a = f
        .core
        .call(move |s| s.speaker_assignment(session, segment))
        .unwrap()
        .unwrap();
    assert!(matches!(a.label, SpeakerLabel::Anonymous { .. }));
    assert_eq!(inner.pending_commits.load(Ordering::Acquire), 0);
    let pid: libc::pid_t = fs::read_to_string(f.root.join("speaker-jobs/entered"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    for job in fs::read_dir(f.root.join("speaker-jobs")).unwrap().flatten() {
        if job.path().is_dir() {
            assert!(!job.path().join("1/segment.pcm").exists())
        }
    }
}
#[test]
fn disable_cancels_owned_process_with_no_late_assignment() {
    let f = Fixture::new(false, true);
    let inner = f.inner();
    let model = f.model("hang");
    let candidate = f.candidate.clone();
    let worker_inner = inner.clone();
    let w = thread::spawn(move || process_candidate(&worker_inner, 1, &model, &candidate));
    wait_until(|| f.root.join("speaker-jobs/entered").exists());
    let began = Instant::now();
    SpeakerClient(inner.clone()).disable();
    assert!(w.join().unwrap().is_err());
    assert!(began.elapsed() < Duration::from_secs(4));
    assert!(inner.contained.load(Ordering::Acquire));
    let session = f.session.session_id;
    let segment = f.candidate.record.payload.segment_id;
    assert!(f
        .core
        .call(move |s| s.speaker_assignment(session, segment))
        .unwrap()
        .is_none());
}
#[test]
fn queued_commit_obeys_disable_fence_before_core_is_available() {
    let f = Fixture::new(false, true);
    let inner = f.inner();
    let model = f.model("ok");
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = f
        .core
        .try_call(move |_| {
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
        .unwrap();
    let candidate = f.candidate.clone();
    let worker_inner = inner.clone();
    let w = thread::spawn(move || process_candidate(&worker_inner, 1, &model, &candidate));
    wait_until(|| inner.pending_commits.load(Ordering::Acquire) == 1);
    SpeakerClient(inner.clone()).disable();
    release_tx.send(()).unwrap();
    blocker.wait().unwrap();
    assert!(w.join().unwrap().is_err());
    let session = f.session.session_id;
    let segment = f.candidate.record.payload.segment_id;
    assert!(f
        .core
        .call(move |s| s.speaker_assignment(session, segment))
        .unwrap()
        .is_none());
    assert_eq!(inner.pending_commits.load(Ordering::Acquire), 0);
}
#[test]
fn default_manager_never_reads_models_or_creates_jobs() {
    let f = Fixture::new(false, true);
    let manager = SpeakerManager::new(
        f.core.clone(),
        f.root.clone(),
        None,
        ResourceBudget::default(),
    );
    assert!(!manager.client().status().enabled);
    thread::sleep(Duration::from_millis(50));
    assert!(!f.root.join("speaker-jobs").exists());
    manager.shutdown();
    wait_until(|| manager.shutdown_complete());
}
#[test]
#[ignore = "explicit installed ECAPA runtime/model probe; never opens a capture device"]
fn installed_ecapa_manager_scans_recording_and_commits_real_embedding() {
    let model = PathBuf::from(
        std::env::var_os("FORUM_SPEAKER_PROBE_MODEL").expect("explicit probe model path"),
    );
    let report = PathBuf::from(
        std::env::var_os("FORUM_SPEAKER_PROBE_REPORT").expect("explicit report path"),
    );
    assert!(model.is_absolute() && report.is_absolute());
    let f = Fixture::new(true, true);
    let manager = SpeakerManager::new(
        f.core.clone(),
        f.root.clone(),
        None,
        ResourceBudget::default(),
    );
    assert!(!manager.client().status().enabled);
    let checkpoint = model.join("embedding_model.ckpt");
    let before = fs::metadata(&checkpoint).unwrap();
    let begin = Instant::now();
    manager
        .client()
        .enable(f.session.session_id, Some(model.clone()))
        .unwrap();
    let deadline = begin + Duration::from_secs(40);
    let mut assignment = None;
    while Instant::now() < deadline {
        let session = f.session.session_id;
        let segment = f.candidate.record.payload.segment_id;
        assignment = f
            .core
            .call(move |s| s.speaker_assignment(session, segment))
            .unwrap();
        if assignment.is_some() {
            break;
        }
        let status = manager.client().status();
        if status.skipped > 0 || status.state == "unavailable" || status.state == "uncontained" {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let status = manager.client().status();
    manager.client().disable();
    manager.shutdown();
    wait_until(|| manager.shutdown_complete());
    let after = fs::metadata(&checkpoint).unwrap();
    let cache_unchanged =
        before.len() == after.len() && before.modified().unwrap() == after.modified().unwrap();
    let output = json!({"kind":"synthetic_recording_installed_ecapa_host_probe","audio":"3-second generated waveform, dual-track recording directory; no microphone","elapsed_seconds":begin.elapsed().as_secs_f64(),"speaker_status":status,"assignment":assignment,"cache_size_mtime_unchanged":cache_unchanged,"shutdown_complete":manager.shutdown_complete(),"formal_speaker_quality_passed":false});
    fs::create_dir_all(report.parent().unwrap()).unwrap();
    fs::write(&report, serde_json::to_vec_pretty(&output).unwrap()).unwrap();
    assert!(cache_unchanged);
    assert!(
        matches!(
            assignment.as_ref().map(|a| &a.label),
            Some(SpeakerLabel::Anonymous { .. })
        ),
        "{}",
        output
    );
}
#[test]
fn foreground_pressure_cancels_background_without_a_speaker_commit() {
    let f = Fixture::new(false, true);
    let inner = f.inner();
    let model = f.model("hang");
    let candidate = f.candidate.clone();
    let worker_inner = inner.clone();
    let permit = inner.budget.admit(true).unwrap();
    let w = thread::spawn(move || process_candidate(&worker_inner, 1, &model, &candidate));
    wait_until(|| f.root.join("speaker-jobs/entered").exists());
    inner.budget.set_pressure(Some("字幕积压测试".into()));
    let began = Instant::now();
    let error = w.join().unwrap().unwrap_err();
    assert!(error.starts_with("资源等待："));
    assert!(began.elapsed() < Duration::from_secs(4));
    drop(permit);
    inner.budget.set_pressure(None);
    assert!(inner.budget.admit(true).is_ok());
    let session = f.session.session_id;
    let segment = f.candidate.record.payload.segment_id;
    assert!(f
        .core
        .call(move |s| s.speaker_assignment(session, segment))
        .unwrap()
        .is_none());
}
#[test]
fn host_deadline_does_not_depend_on_receiving_a_worker_frame() {
    let f = Fixture::new(false, true);
    let inner = f.inner();
    let mut command = Command::new("/usr/bin/python3");
    command.args(["-c", "import time;time.sleep(60)"]);
    let mut worker = WorkerProcess::spawn(command, &f.root.join("deadline.stderr.log")).unwrap();
    let start = Instant::now();
    let error =
        await_response(&worker, 1, &inner, 1, start + Duration::from_millis(80)).unwrap_err();
    assert!(error.contains("预算"));
    worker.stop(Duration::from_millis(50)).unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
}
