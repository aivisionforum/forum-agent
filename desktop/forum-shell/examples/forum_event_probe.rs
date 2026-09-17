//! F10 production host → local MLX → core using two independent temporary stores.
//! One source is imported only through its published peer projection; no audio devices.
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

fn publish_minutes(
    core: &CoreHandle,
    client: &analysis::AnalysisClient,
    session: Uuid,
    text: &str,
) -> anyhow::Result<(ArtifactRecord, PublicArtifact)> {
    let job = client
        .create(analysis::JobRequest {
            request_id: Uuid::new_v4(),
            session_ids: vec![session],
            kind: AnalysisKind::Minutes,
            automatic: false,
            public_selections: None,
        })
        .map_err(anyhow::Error::msg)?;
    let done = wait(core, &job)?;
    anyhow::ensure!(
        done.state == AnalysisJobState::Succeeded,
        "minutes failed: {:?}",
        done.error
    );
    let id = done.result.unwrap().artifact_id;
    let artifact = core.call(move |s| s.artifact(id))?;
    anyhow::ensure!(
        artifact.validation == ArtifactValidation::Valid && artifact.coverage_complete,
        "minutes incomplete"
    );
    let rev = artifact.revision;
    core.call(move |s| {
        s.review_artifact(&ArtifactReviewCommand {
            artifact_id: id,
            expected_revision: rev,
            review: ArtifactReview::Approved,
            operator_id: "synthetic-f10-probe".into(),
            reason: "synthetic integration fixture only".into(),
        })
    })?;
    let evidence = artifact
        .content
        .sections
        .iter()
        .flat_map(|v| &v.claims)
        .flat_map(|v| &v.evidence)
        .map(|e| PublicEvidenceInput {
            evidence: e.clone(),
            reviewed_text: match e {
                AnalysisEvidence::Source { span, .. } => span.quote.clone(),
                AnalysisEvidence::Artifact { quote, .. } => quote.clone(),
            },
        })
        .collect();
    let policy = artifact.config.projection_policy_hash.clone();
    let text = text.to_string();
    let public = core.call(move |s| {
        s.publish_artifact(&ArtifactPublishCommand {
            artifact_id: id,
            expected_revision: rev,
            operator_id: "synthetic-f10-probe".into(),
            reason: "synthetic reviewed projection".into(),
            policy_hash: policy,
            reviewed_title: "Synthetic public minutes".into(),
            reviewed_text: text,
            evidence,
        })
    })?;
    Ok((artifact, public))
}

fn run(root: &PathBuf, report: &mut Value) -> anyhow::Result<()> {
    let local_root = root.join("local");
    let remote_root = root.join("remote");
    for dir in [&local_root, &remote_root] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let local = CoreHandle::open(local_root.join("forum.sqlite"), 128)?;
    let remote = CoreHandle::open(remote_root.join("forum.sqlite"), 128)?;
    let event = Uuid::new_v4();
    let a = local.call(move |s| {
        synthetic_session(
            s,
            &["会场A：今天没有批准公开发布。试点预算为12万元。负责人和完成日期尚未确定。"],
            event,
        )
    })?;
    let b = remote.call(move |s| {
        synthetic_session(
            s,
            &["会场B：今天同意先比较两个方案。试点预算为15万元。负责人和完成日期尚未确定。"],
            event,
        )
    })?;
    let ma =
        analysis::AnalysisManager::new(local.clone(), local_root, None, ResourceBudget::default());
    let mb = analysis::AnalysisManager::new(
        remote.clone(),
        remote_root,
        None,
        ResourceBudget::default(),
    );
    let (local_minutes, ap) = publish_minutes(
        &local,
        &ma.client(),
        a.session_id,
        "会场A公开结论：试点预算12万元，尚未确定负责人或完成日期。",
    )?;
    let (remote_minutes, bp) = publish_minutes(
        &remote,
        &mb.client(),
        b.session_id,
        "会场B公开结论：先比较两个方案；试点预算15万元，尚未确定负责人或完成日期。",
    )?;
    report["local_minutes"] = json!(local_minutes);
    report["remote_minutes"] = json!(remote_minutes);
    let bid = b.session_id;
    let projection = remote.call(move |s| s.public_snapshot(bid))?;
    let peer = PeerSession {
        owner_device_id: b.owner_device_id,
        event_id: event,
        session_id: bid,
        title: "Explicit public room B".into(),
        room_name: "B".into(),
    };
    let peer_copy = peer.clone();
    let snapshot = PeerPublicationSnapshot {
        owner_device_id: peer.owner_device_id,
        event_id: event,
        session_id: bid,
        cursor: projection.cursor,
        artifacts: projection.artifacts,
    };
    local.call(move |s| {
        s.register_peer_session(&peer_copy)?;
        s.apply_peer_snapshot(&snapshot)
    })?;
    let selections = vec![
        PublicSelection {
            owner_device_id: b.owner_device_id,
            session_id: bid,
            public_id: bp.public_id,
            revision: bp.revision,
        },
        PublicSelection {
            owner_device_id: a.owner_device_id,
            session_id: a.session_id,
            public_id: ap.public_id,
            revision: ap.revision,
        },
    ];
    let mut outputs = vec![];
    for kind in [AnalysisKind::EventReport, AnalysisKind::ClosingBrief] {
        let job = ma
            .client()
            .create(analysis::JobRequest {
                request_id: Uuid::new_v4(),
                session_ids: vec![bid, a.session_id],
                kind,
                automatic: false,
                public_selections: Some(selections.clone()),
            })
            .map_err(anyhow::Error::msg)?;
        let id = job.job_id;
        let frozen = local.call(move |s| s.analysis_snapshot(id))?;
        anyhow::ensure!(
            frozen.snapshot.segments.is_empty() && frozen.snapshot.published_artifacts.len() == 2,
            "expected exactly two reviewed public inputs"
        );
        let provenance = local.call(move |s| s.analysis_peer_provenance(id))?;
        anyhow::ensure!(
            provenance.len() == 1
                && provenance[0].owner_device_id == b.owner_device_id
                && provenance[0].public_id == bp.public_id
                && provenance[0].revision == bp.revision,
            "peer version provenance missing"
        );
        let done = wait(&local, &job)?;
        let key = if kind == AnalysisKind::EventReport {
            "event_report"
        } else {
            "closing_brief"
        };
        report[key] = json!({"job":done,"snapshot":frozen,"peer_provenance":provenance});
        anyhow::ensure!(
            done.state == AnalysisJobState::Succeeded,
            "{key} failed: {:?}",
            done.error
        );
        let aid = done.result.unwrap().artifact_id;
        let artifact = local.call(move |s| s.artifact(aid))?;
        report[key]["artifact"] = json!(artifact);
        report[key]["validation"] = json!(artifact.validation);
        report[key]["unsupported_claims"] = json!(artifact
            .content
            .sections
            .iter()
            .flat_map(|v| &v.claims)
                .filter(|v| v.grounding == GroundingStatus::Unsupported)
            .count());
        anyhow::ensure!(
            artifact.coverage_complete
                && matches!(
                    artifact.validation,
                    ArtifactValidation::Valid | ArtifactValidation::NeedsReview
                )
                && artifact.review == ArtifactReview::Draft
                && artifact.publication == ArtifactPublication::Private,
            "report must be a complete private draft with explicit validation status"
        );
        anyhow::ensure!(
            artifact.coverage.units.len() == 2
                && artifact
                    .coverage
                    .units
                    .iter()
                    .all(|u| matches!(u.target, CoverageTarget::Artifact { .. })),
            "public evidence coverage missing"
        );
        if artifact.validation == ArtifactValidation::NeedsReview {
            let rev = artifact.revision;
            anyhow::ensure!(
                local
                    .call(move |s| s.review_artifact(&ArtifactReviewCommand {
                        artifact_id: aid,
                        expected_revision: rev,
                        review: ArtifactReview::Approved,
                        operator_id: "synthetic-f10-probe".into(),
                        reason: "must reject unvalidated approval".into(),
                    }))
                    .is_err(),
                "NeedsReview artifact was approved"
            );
            let policy = artifact.config.projection_policy_hash.clone();
            let public_evidence = artifact
                .content
                .sections
                .iter()
                .flat_map(|v| &v.claims)
                .flat_map(|v| &v.evidence)
                .map(|e| PublicEvidenceInput {
                    evidence: e.clone(),
                    reviewed_text: match e {
                        AnalysisEvidence::Source { span, .. } => span.quote.clone(),
                        AnalysisEvidence::Artifact { quote, .. } => quote.clone(),
                    },
                })
                .collect();
            anyhow::ensure!(
                local
                    .call(move |s| s.publish_artifact(&ArtifactPublishCommand {
                        artifact_id: aid,
                        expected_revision: rev,
                        operator_id: "synthetic-f10-probe".into(),
                        reason: "must reject unvalidated publication".into(),
                        policy_hash: policy,
                        reviewed_title: "Synthetic forbidden publication".into(),
                        reviewed_text: "合成未核验稿必须拒绝公开。".into(),
                        evidence: public_evidence,
                    }))
                    .is_err(),
                "NeedsReview artifact was published"
            );
            report[key]["unvalidated_approval_and_publication_rejected"] = json!(true);
        }
        outputs.push(aid);
    }
    let source_id = remote_minutes.artifact_id;
    let source_rev = remote_minutes.revision;
    remote.call(move |s| {
        s.set_artifact_visibility(&ArtifactVisibilityCommand {
            artifact_id: source_id,
            expected_revision: source_rev,
            publication: ArtifactPublication::Withdrawn,
            operator_id: "synthetic-f10-probe".into(),
            reason: "verify withdrawal cascade".into(),
        })
    })?;
    let withdrawn = remote.call(move |s| s.public_snapshot(bid))?;
    local.call(move |s| {
        s.apply_peer_snapshot(&PeerPublicationSnapshot {
            owner_device_id: peer.owner_device_id,
            event_id: event,
            session_id: bid,
            cursor: withdrawn.cursor,
            artifacts: withdrawn.artifacts,
        })
    })?;
    for id in outputs {
        anyhow::ensure!(
            local.call(move |s| s.artifact(id))?.validation == ArtifactValidation::Stale,
            "withdrawal did not invalidate dependent result"
        );
    }
    anyhow::ensure!(
        ma.client()
            .create(analysis::JobRequest {
                request_id: Uuid::new_v4(),
                session_ids: vec![bid, a.session_id],
                kind: AnalysisKind::EventReport,
                automatic: false,
                public_selections: Some(selections),
            })
            .is_err(),
        "withdrawn public selection admitted"
    );
    ma.shutdown();
    mb.shutdown();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ma.shutdown_complete() || !mb.shutdown_complete() {
        anyhow::ensure!(Instant::now() < deadline, "worker shutdown incomplete");
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(ma);
    drop(mb);
    local.shutdown()?;
    remote.shutdown()?;
    report["two_independent_stores"] = json!(true);
    report["only_exact_public_inputs"] = json!(true);
    report["withdrawal_invalidated_both"] = json!(true);
    report["withdrawn_selection_rejected"] = json!(true);
    report["shutdown_complete"] = json!(true);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("forum-event-probe-{}", Uuid::new_v4()))
        });
    anyhow::ensure!(!root.exists(), "probe output must be new");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    let mut report = json!({"root":root,"scope":"production Rust host + core + local 8B; two independent temporary stores and peer published projection; synthetic text only, no audio devices", "formal_quality_acceptance":false});
    let start = Instant::now();
    let result = run(&root, &mut report);
    report["elapsed_ms"] = json!(start.elapsed().as_millis());
    report["error"] = json!(result.as_ref().err().map(ToString::to_string));
    fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", root.join("report.json").display());
    result
}
