//! Actual coordinator + local model on an explicitly copied DB. No audio devices.
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
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 4,
        "expected copied-probe-root session-id app-resources"
    );
    let root = PathBuf::from(&args[1]).canonicalize()?;
    anyhow::ensure!(
        root.starts_with("/private/tmp") || root.starts_with("/tmp"),
        "probe accepts temporary DB copies only"
    );
    let session = Uuid::parse_str(&args[2])?;
    let core = CoreHandle::open(&root.join("core.sqlite"), 64)?;
    // Isolate this test from unrelated automatic post-meeting requests in the copy.
    core.call(|s| {
        for (id, marker) in s.pending_analysis_stop_intents(1000)? {
            s.ack_analysis_stop_intent(id, marker)?;
        }
        Ok(())
    })?;
    let budget = ResourceBudget::default();
    let live = budget
        .enter_live(Duration::from_secs(2))
        .map_err(anyhow::Error::msg)?;
    budget.live_ready();
    let manager = analysis::AnalysisManager::new(
        core.clone(),
        root.clone(),
        Some(PathBuf::from(&args[3])),
        budget.clone(),
    );
    let client = manager.client();
    client.session_started(session);
    let job = client
        .create(analysis::JobRequest {
            request_id: Uuid::new_v4(),
            session_ids: vec![session],
            kind: AnalysisKind::Insight,
            automatic: false,
            public_selections: None,
        })
        .map_err(anyhow::Error::msg)?;
    let snapshot = core.call(move |s| s.analysis_snapshot(job.job_id))?;
    anyhow::ensure!(
        snapshot
            .snapshot
            .segments
            .iter()
            .all(|s| s.revision.is_some()),
        "pending tail included in snapshot"
    );
    let started = Instant::now();
    let mut preempted = false;
    let mut resume_at = None;
    let mut soft_step = 0;
    let mut paused_at = Instant::now();
    let mut pause_count = 0;
    let mut states = Vec::new();
    let result = loop {
        let current = core.call(move |s| s.analysis_job(job.job_id))?;
        let summary = format!(
            "{:?}/{}/{}",
            current.state, current.attempt, current.progress.phase
        );
        if states.last() != Some(&summary) {
            eprintln!("{summary}");
            states.push(summary);
        }
        // Reproduce one foreground interruption, then let the same request resume.
        if !preempted
            && current.state == AnalysisJobState::Running
            && current.progress.phase == "analyzing"
        {
            budget.set_pressure(Some("probe: sustained ASR backlog".into()));
            preempted = true;
        }
        if current.attempt > 1 && resume_at.is_none() {
            budget.set_pressure(None);
            resume_at = Some(Instant::now());
        }
        // Sustained ASR pressure must pause twice without discarding this attempt.
        if resume_at.is_some() && current.state == AnalysisJobState::Running {
            if soft_step == 0 && pause_count < 2 && current.progress.phase == "analyzing" {
                budget.set_transient_pressure(
                    "probe: sustained ASR burst".into(),
                    Duration::from_millis(200),
                );
                soft_step = 1;
            } else if soft_step == 1 && current.progress.phase == "paused" {
                pause_count += 1;
                paused_at = Instant::now();
                soft_step = 2;
            } else if soft_step == 2 && paused_at.elapsed() >= Duration::from_secs(1) {
                budget.set_pressure(None);
                paused_at = Instant::now();
                soft_step = 3;
            } else if soft_step == 3 && paused_at.elapsed() >= Duration::from_secs(1) {
                soft_step = 0;
            }
        }
        if matches!(
            current.state,
            AnalysisJobState::Succeeded
                | AnalysisJobState::SucceededPartial
                | AnalysisJobState::Failed
                | AnalysisJobState::Interrupted
                | AnalysisJobState::Cancelled
        ) {
            break current;
        }
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(220),
            "probe timed out"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut artifacts = core.call(move |s| s.list_artifacts(session, None, 30))?;
    artifacts
        .items
        .retain(|artifact| artifact.job_id == job.job_id);
    analysis::write_private(&root.join("report.json"), &serde_json::to_vec_pretty(&json!({
        "scope":"copied local meeting; production coordinator/8B; simulated foreground pressure; no audio capture",
        "elapsed_ms":started.elapsed().as_millis(),"preempted":preempted,"pause_count":pause_count,"states":states,"job":result,"artifacts":artifacts.items
    }))?).map_err(anyhow::Error::msg)?;
    manager.shutdown();
    drop(live);
    drop(manager);
    anyhow::ensure!(
        matches!(
            result.state,
            AnalysisJobState::Succeeded | AnalysisJobState::SucceededPartial
        ),
        "analysis failed: {:?}",
        result.error
    );
    anyhow::ensure!(
        preempted && result.attempt == 2,
        "expected one interruption and successful automatic retry"
    );
    anyhow::ensure!(
        pause_count == 2,
        "expected two cooperative pauses in the same attempt"
    );
    anyhow::ensure!(
        artifacts
            .items
            .iter()
            .any(|a| a.content.sections.iter().any(|s| !s.claims.is_empty())),
        "no visible claims"
    );
    println!("{}", root.join("report.json").display());
    Ok(())
}
