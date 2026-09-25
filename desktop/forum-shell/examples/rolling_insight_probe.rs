//! Real 30-second scheduler and local model; synthetic transcript, no audio devices.
#![allow(dead_code)]
#[path = "../src/analysis.rs"]
mod analysis;
#[path = "../src/identity.rs"]
mod identity;
#[path = "../src/preferences.rs"]
mod preferences;
use forum_contracts::*;
use forum_core::CoreHandle;
use forum_runtime::resource_budget::ResourceBudget;
use serde_json::json;
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    time::{Duration, Instant},
};

fn event<T>(s: &SessionSpec, kind: EventType, payload: T) -> Event<T> {
    Event {
        schema_version: 1,
        message_id: Uuid::new_v4(),
        event_type: kind,
        event_id: s.event_id,
        room_id: s.room_id,
        session_id: s.session_id,
        producer: Producer {
            name: "synthetic-rolling-probe".into(),
            run_id: Uuid::new_v4(),
            seq: 1,
        },
        payload,
    }
}
fn feed(
    core: &CoreHandle,
    s: &SessionSpec,
    track: Uuid,
    start: u64,
    text: &str,
) -> anyhow::Result<Uuid> {
    let segment = Uuid::new_v4();
    let audio = AudioRange {
        start_sample: start * 16,
        end_sample: (start + 5000) * 16,
        sample_rate: 16000,
        start_ms: start,
        end_ms: start + 5000,
    };
    let capture = event(
        s,
        EventType::AudioSegmentClosed,
        CaptureSegmentClosed {
            track_id: track,
            segment_id: segment,
            audio: audio.clone(),
            recording_ref: None,
        },
    );
    let transcript = event(
        s,
        EventType::TranscriptFinal,
        TranscriptFinal {
            track_id: track,
            segment_id: segment,
            revision: Revision::FIRST,
            audio,
            text: text.into(),
            configured_source_language: "zh".into(),
            detected_language: Some("zh".into()),
            target_languages: vec![],
            direction_epoch: 1,
            speaker_id: None,
            status: TranscriptStatus::Success,
            reason: None,
            backend: "synthetic-text".into(),
            model_manifest_id: "synthetic-text-only".into(),
        },
    );
    core.call(move |store| {
        store.register_capture(&capture)?;
        store.ingest_final(&transcript)?;
        Ok(())
    })?;
    Ok(segment)
}
fn main() -> anyhow::Result<()> {
    let resources = PathBuf::from(std::env::args().nth(1).expect("app resources path"));
    let root = std::env::temp_dir().join(format!("forum-rolling-probe-{}", Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let core = CoreHandle::open(&root.join("core.sqlite"), 64)?;
    let s = SessionSpec {
        session_id: Uuid::new_v4(),
        event_id: Uuid::new_v4(),
        room_id: Uuid::new_v4(),
        owner_device_id: Uuid::new_v4(),
        title: "Synthetic rolling insight verification".into(),
    };
    let id = s.session_id;
    let track = Uuid::new_v4();
    let setup = s.clone();
    core.call(move |store| {
        store.create_session(&setup)?;
        store.create_track(&TrackSpec {
            track_id: track,
            session_id: id,
            kind: TrackKind::RoomMix,
            sample_rate: 16000,
        })?;
        for (expected_state, next_state) in [
            (SessionState::Created, SessionState::Preparing),
            (SessionState::Preparing, SessionState::Ready),
            (SessionState::Ready, SessionState::Recording),
        ] {
            store.transition_session(&event(
                &setup,
                EventType::SessionChanged,
                SessionTransition {
                    expected_state,
                    next_state,
                    reason: "synthetic test".into(),
                },
            ))?;
        }
        Ok(())
    })?;
    let first = feed(
        &core,
        &s,
        track,
        0,
        "先测试字幕延迟和识别准确率，目标是让观众清楚看到发言。",
    )?;
    feed(
        &core,
        &s,
        track,
        20_000,
        "预算还没有批准，公开展示前需要人工审核。",
    )?;
    let budget = ResourceBudget::default();
    let live = budget
        .enter_live(Duration::from_secs(2))
        .map_err(anyhow::Error::msg)?;
    budget.live_ready();
    let manager =
        analysis::AnalysisManager::new(core.clone(), root.clone(), Some(resources), budget);
    let start = Instant::now();
    manager.client().session_started(id);
    let mut second = None;
    let mut timings = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        anyhow::ensure!(
            start.elapsed() < Duration::from_secs(150),
            "rolling probe timeout"
        );
        let jobs = core
            .call(move |st| st.list_analysis_jobs(id, None, 100))?
            .items;
        for job in &jobs {
            if seen.insert(job.job_id) {
                eprintln!(
                    "created {:.1}s {}",
                    start.elapsed().as_secs_f32(),
                    job.job_id
                );
            }
            anyhow::ensure!(
                !matches!(
                    job.state,
                    AnalysisJobState::Failed | AnalysisJobState::Interrupted
                ),
                "job failed: {:?}",
                job.error
            );
        }
        let completed: Vec<_> = jobs
            .iter()
            .filter(|j| {
                matches!(
                    j.state,
                    AnalysisJobState::Succeeded | AnalysisJobState::SucceededPartial
                )
            })
            .collect();
        if completed.len() == 1 && second.is_none() {
            timings.push(start.elapsed().as_secs_f32());
            second = Some(feed(
                &core,
                &s,
                track,
                35_000,
                "决定周五进行会场联调，字幕测试仍按原计划继续。",
            )?);
            eprintln!("first output {:.1}s; added next speech", timings[0]);
        }
        if completed.len() == 2 {
            timings.push(start.elapsed().as_secs_f32());
            anyhow::ensure!(
                jobs.len() == 2 && jobs.iter().all(|j| j.automatic),
                "unexpected duplicate/manual job"
            );
            let newest = completed[0].job_id;
            let snapshot = core.call(move |st| st.analysis_snapshot(newest))?.snapshot;
            anyhow::ensure!(
                !snapshot.segments.iter().any(|x| x.segment_id == first),
                "entire old input replayed"
            );
            anyhow::ensure!(
                snapshot
                    .segments
                    .iter()
                    .any(|x| Some(x.segment_id) == second),
                "new speech missing"
            );
            let history = core
                .call(move |st| st.live_insight_history(id, 0))?
                .1
                .unwrap();
            anyhow::ensure!(
                history.len() == 2
                    && history.iter().all(|a| a
                        .content
                        .sections
                        .iter()
                        .any(|s| !s.claims.is_empty())),
                "empty or missing accumulated output"
            );
            analysis::write_private(&root.join("report.json"),&serde_json::to_vec_pretty(&json!({"scope":"synthetic text; real 30-second scheduler and local 8B; no audio capture","output_seconds":timings,"jobs":jobs,"artifacts":history}))?).map_err(anyhow::Error::msg)?;
            println!("{}", root.join("report.json").display());
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    manager.shutdown();
    drop(live);
    drop(manager);
    Ok(())
}
