//! Production coordinator + local 8B, using synthetic text only. No audio devices.
#![allow(dead_code)]
#[path = "../src/analysis.rs"]
mod analysis;
#[path = "../src/identity.rs"]
mod identity;
#[path = "../src/preferences.rs"]
mod preferences;
use forum_contracts::*;
use forum_core::{CoreHandle, Store};
use forum_runtime::resource_budget::ResourceBudget;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    time::{Duration, Instant},
};

fn event<T>(
    session: &SessionSpec,
    kind: EventType,
    name: &str,
    run: Uuid,
    seq: u64,
    payload: T,
) -> Event<T> {
    Event {
        schema_version: 1,
        message_id: Uuid::new_v4(),
        event_type: kind,
        event_id: session.event_id,
        room_id: session.room_id,
        session_id: session.session_id,
        producer: Producer {
            name: name.into(),
            run_id: run,
            seq,
        },
        payload,
    }
}
fn synthetic_session(
    store: &mut Store,
    texts: &[&str],
    event_id: Uuid,
) -> forum_core::Result<SessionSpec> {
    let session = SessionSpec {
        session_id: Uuid::new_v4(),
        event_id,
        room_id: Uuid::new_v4(),
        owner_device_id: Uuid::new_v4(),
        title: "合成文本 · 第三部分生产链路验证".into(),
    };
    let track = TrackSpec {
        track_id: Uuid::new_v4(),
        session_id: session.session_id,
        kind: TrackKind::Replay,
        sample_rate: 16000,
    };
    store.create_session(&session)?;
    store.create_track(&track)?;
    for (expected_state, next_state) in [
        (SessionState::Created, SessionState::Preparing),
        (SessionState::Preparing, SessionState::Ready),
        (SessionState::Ready, SessionState::Recording),
    ] {
        store.transition_session(&event(
            &session,
            EventType::SessionChanged,
            "synthetic-host",
            Uuid::new_v4(),
            1,
            SessionTransition {
                expected_state,
                next_state,
                reason: "synthetic fixture, no audio devices".into(),
            },
        ))?;
    }
    let run = Uuid::new_v4();
    let mut ids = vec![];
    for (i, text) in texts.iter().enumerate() {
        let id = Uuid::new_v4();
        ids.push(id);
        let start = i as u64 * 3000;
        let audio = AudioRange {
            start_sample: start * 16,
            end_sample: (start + 3000) * 16,
            sample_rate: 16000,
            start_ms: start,
            end_ms: start + 3000,
        };
        store.register_capture(&event(
            &session,
            EventType::AudioSegmentClosed,
            "synthetic-capture",
            Uuid::new_v4(),
            1,
            CaptureSegmentClosed {
                track_id: track.track_id,
                segment_id: id,
                audio: audio.clone(),
                recording_ref: None,
            },
        ))?;
        store.ingest_final(&event(
            &session,
            EventType::TranscriptFinal,
            "synthetic-asr",
            run,
            i as u64 + 1,
            TranscriptFinal {
                track_id: track.track_id,
                segment_id: id,
                revision: Revision::FIRST,
                audio,
                text: (*text).into(),
                configured_source_language: "auto".into(),
                detected_language: None,
                target_languages: vec![],
                direction_epoch: 1,
                speaker_id: None,
                status: TranscriptStatus::Success,
                reason: None,
                backend: "synthetic-text-fixture".into(),
                model_manifest_id: "no-asr-model-used".into(),
            },
        ))?;
    }
    store.producer_sealed(&event(
        &session,
        EventType::ProducerSealed,
        "synthetic-asr",
        run,
        texts.len() as u64 + 1,
        ProducerSeal {
            producer_run_id: run,
            final_seq: texts.len() as u64,
            segment_ids: ids.clone(),
        },
    ))?;
    store.transition_session(&event(
        &session,
        EventType::SessionChanged,
        "synthetic-host",
        Uuid::new_v4(),
        1,
        SessionTransition {
            expected_state: SessionState::Recording,
            next_state: SessionState::Stopping,
            reason: "synthetic fixture sealed".into(),
        },
    ))?;
    let tracks = vec![TrackSeal {
        track_id: track.track_id,
        final_sample: texts.len() as u64 * 48000,
        segment_ids: ids,
    }];
    let hash = capture_manifest_sha256(&tracks)?;
    store.capture_stopped(&event(
        &session,
        EventType::CaptureStopped,
        "synthetic-host",
        Uuid::new_v4(),
        1,
        CaptureStopped {
            tracks,
            manifest_sha256: hash.clone(),
        },
    ))?;
    store.seal_transcript(&event(
        &session,
        EventType::TranscriptSealed,
        "synthetic-host",
        Uuid::new_v4(),
        1,
        TranscriptSeal {
            capture_manifest_sha256: hash,
        },
    ))?;
    Ok(session)
}
fn wait(core: &CoreHandle, job: &AnalysisJob) -> anyhow::Result<AnalysisJob> {
    let deadline = Instant::now() + Duration::from_secs(200);
    let id = job.job_id;
    loop {
        let current = core.call(move |s| s.analysis_job(id))?;
        if matches!(
            current.state,
            AnalysisJobState::Succeeded
                | AnalysisJobState::SucceededPartial
                | AnalysisJobState::Failed
                | AnalysisJobState::Cancelled
                | AnalysisJobState::Interrupted
        ) {
            return Ok(current);
        }
        anyhow::ensure!(Instant::now() < deadline, "probe deadline");
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn run(root: &PathBuf, report: &mut Value) -> anyhow::Result<()> {
    let core = CoreHandle::open(root.join("forum.sqlite"), 128)?;
    let event_id = Uuid::new_v4();
    let session=core.call(move|s|synthetic_session(s,&[
        "主持人：今天没有批准公开发布。试点预算不是20万元，而是12万元。先评估两个方案，再作决定。",
        "Participant: We have not approved publication. The pilot budget is 120,000 yuan, not 200,000. We still need to compare two options.",
        "参会者：同意先试点，但目前没有确定负责人，也没有承诺完成日期。"],event_id))?;
    let unrelated = core.call(move |s| {
        synthetic_session(s, &["另一个合成会场，内容不允许进入前一场报告。"], event_id)
    })?;
    report["session_id"] = json!(session.session_id);
    report["unselected_session_id"] = json!(unrelated.session_id);
    let manager =
        analysis::AnalysisManager::new(core.clone(), root.clone(), None, ResourceBudget::default());
    let client = manager.client();
    let sid = session.session_id;
    let requested = client
        .create(analysis::JobRequest {
            request_id: Uuid::new_v4(),
            session_ids: vec![sid],
            kind: AnalysisKind::Minutes,
            automatic: false,
        })
        .map_err(anyhow::Error::msg)?;
    let finished = wait(&core, &requested)?;
    report["minutes_job"] = json!(finished);
    anyhow::ensure!(
        finished.state == AnalysisJobState::Succeeded,
        "minutes did not fully complete: {:?}",
        finished.error
    );
    let id = finished.result.as_ref().unwrap().artifact_id;
    let artifact = core.call(move |s| s.artifact(id))?;
    report["minutes"] = json!(artifact);
    anyhow::ensure!(
        artifact.coverage_complete && artifact.validation == ArtifactValidation::Valid,
        "minutes require further validation"
    );
    anyhow::ensure!(
        artifact.review == ArtifactReview::Draft
            && artifact.publication == ArtifactPublication::Private,
        "model auto-approved"
    );
    let empty = core.call(move |s| s.public_snapshot(sid))?;
    anyhow::ensure!(empty.artifacts.is_empty(), "private draft leaked");
    let aid = id;
    let rev = artifact.revision;
    core.call(move |s| {
        s.review_artifact(&ArtifactReviewCommand {
            artifact_id: aid,
            expected_revision: rev,
            review: ArtifactReview::Approved,
            operator_id: "synthetic-probe".into(),
            reason: "合成联调审核，不代表正式语义验收".into(),
        })
    })?;
    let evidence = artifact
        .content
        .sections
        .iter()
        .flat_map(|s| &s.claims)
        .flat_map(|c| &c.evidence)
        .map(|e| PublicEvidenceInput {
            evidence: e.clone(),
            reviewed_text: match e {
                AnalysisEvidence::Source { span, .. } => span.quote.clone(),
                AnalysisEvidence::Artifact { quote, .. } => quote.clone(),
            },
        })
        .collect();
    let public_text =
        "本合成讨论尚未批准公开发布；试点预算为12万元，需要先比较两个方案。负责人和日期尚未确定。"
            .to_string();
    let published = core.call(move |s| {
        s.publish_artifact(&ArtifactPublishCommand {
            artifact_id: id,
            expected_revision: rev,
            operator_id: "synthetic-probe".into(),
            reason: "仅验证合成数据的审核发布事务".into(),
            policy_hash: artifact.config.projection_policy_hash,
            reviewed_title: "合成联调公开版本".into(),
            reviewed_text: public_text,
            evidence,
        })
    })?;
    report["published"] = json!(published);
    for (format, ext) in [("markdown", "md"), ("html", "html"), ("json", "json")] {
        let content = core.call(move |s| s.export_artifact(id, rev, format))?;
        fs::write(root.join(format!("minutes.{ext}")), content)?;
    }
    let requested = client
        .create(analysis::JobRequest {
            request_id: Uuid::new_v4(),
            session_ids: vec![sid],
            kind: AnalysisKind::EventReport,
            automatic: false,
        })
        .map_err(anyhow::Error::msg)?;
    let finished = wait(&core, &requested)?;
    report["report_job"] = json!(finished);
    anyhow::ensure!(
        finished.state == AnalysisJobState::Succeeded,
        "selected report did not fully complete: {:?}",
        finished.error
    );
    let report_id = finished.result.unwrap().artifact_id;
    let generated = core.call(move |s| s.artifact(report_id))?;
    report["report_artifact"] = json!(generated);
    let other = unrelated.session_id;
    anyhow::ensure!(
        core.call(move |s| s.list_artifacts(other, None, 100))?
            .items
            .is_empty(),
        "cross-session write"
    );
    let source = core.call(move |s| s.analysis_snapshot(requested.job_id))?;
    anyhow::ensure!(
        source.snapshot.segments.is_empty() && source.snapshot.published_artifacts.len() == 1,
        "report used private transcripts"
    );
    core.call(move |s| {
        s.set_artifact_visibility(&ArtifactVisibilityCommand {
            artifact_id: id,
            expected_revision: rev,
            publication: ArtifactPublication::Hidden,
            operator_id: "synthetic-probe".into(),
            reason: "验证上游隐藏与依赖失效".into(),
        })
    })?;
    anyhow::ensure!(
        core.call(move |s| s.public_snapshot(sid))?
            .artifacts
            .is_empty(),
        "hidden content leaked"
    );
    let invalidated = core.call(move |s| s.artifact(report_id))?;
    anyhow::ensure!(
        invalidated.validation == ArtifactValidation::Stale,
        "report dependency not invalidated"
    );
    report["draft_private"] = json!(true);
    report["selected_scope_only"] = json!(true);
    report["hidden_retracted"] = json!(true);
    report["dependent_report_stale"] = json!(true);
    manager.shutdown();
    drop(manager);
    drop(core);
    Ok(())
}
fn main() -> anyhow::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("forum-analysis-probe-{}", Uuid::new_v4()))
        });
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    let mut report = json!({"root":root,"scope":"production Rust analysis coordinator + core + local 8B; synthetic text only; no audio devices","formal_quality_acceptance":false});
    let started = Instant::now();
    let result = run(&root, &mut report);
    report["elapsed_ms"] = json!(started.elapsed().as_millis());
    report["error"] = json!(result.as_ref().err().map(|e| e.to_string()));
    fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", root.join("report.json").display());
    result
}
