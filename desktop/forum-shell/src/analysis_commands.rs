// Included in app.rs so the Tauri adapter shares AppState without exposing it.
include!(concat!(env!("OUT_DIR"), "/display_assets.rs"));

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyImportRequest {
    title: String,
    content: String,
}
#[tauri::command]
async fn import_legacy_transcript(
    state: State<'_, AppState>,
    request: LegacyImportRequest,
) -> Result<forum_core::SessionSummary, String> {
    let repository = state.runtime.repository()?.clone();
    tauri::async_runtime::spawn_blocking(move || {
        repository.import_legacy(request.title, request.content)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn list_meeting_sessions_page(
    state: State<'_, AppState>,
    after: Option<String>,
    limit: u32,
) -> Result<serde_json::Value, String> {
    let after = after
        .map(|value| value.parse::<u64>().map_err(|_| "会议分页游标无效"))
        .transpose()?;
    state
        .runtime
        .repository()?
        .core
        .call(move |store| {
            let (items, next) = store.list_sessions_page(after, limit)?;
            Ok(serde_json::json!({"items":items,"next_after":next.map(|n|n.to_string())}))
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_analysis_state(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
    after_jobs: Option<String>,
    after_artifacts: Option<String>,
    live_cursor: Option<u64>,
) -> Result<serde_json::Value, String> {
    let notice = match &state.analysis {
        Ok(manager) => manager.client().notice(session_id),
        Err(error) => Some(error.clone()),
    };
    let jobs = after_jobs
        .map(|v| serde_json::from_str::<forum_contracts::AnalysisPageKey>(&v))
        .transpose()
        .map_err(|_| "任务分页游标无效")?;
    let artifacts = after_artifacts
        .map(|v| serde_json::from_str::<forum_contracts::AnalysisPageKey>(&v))
        .transpose()
        .map_err(|_| "产物分页游标无效")?;
    state.runtime.repository()?.core.call(move|store|{
        let jobs=store.list_analysis_jobs(session_id,jobs,30)?;
        let artifacts=store.list_artifacts(session_id,artifacts,30)?;
        let (live_cursor,live_artifacts)=match live_cursor {Some(known)=>store.live_insight_history(session_id,known)?,None=>(0,None)};
        Ok(serde_json::json!({"session_id":session_id,"cursor":jobs.cursor.max(artifacts.cursor),"jobs":jobs.items,"artifacts":artifacts.items,"live_cursor":live_cursor,"live_artifacts":live_artifacts,"notice":notice,"next_jobs":jobs.next_after.map(|k|serde_json::to_string(&k).unwrap()),"next_artifacts":artifacts.next_after.map(|k|serde_json::to_string(&k).unwrap())}))
    }).map_err(|e|e.to_string())
}

#[tauri::command]
async fn create_analysis_job(
    state: State<'_, AppState>,
    request: crate::analysis::JobRequest,
) -> Result<forum_contracts::AnalysisJob, String> {
    if state.exiting.load(std::sync::atomic::Ordering::Acquire) {
        return Err("应用正在退出".into());
    }
    let client = state.analysis.as_ref().map_err(Clone::clone)?.client();
    tauri::async_runtime::spawn_blocking(move || client.create(request))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn cancel_analysis_job(
    state: State<'_, AppState>,
    request: crate::analysis::JobAction,
) -> Result<forum_contracts::AnalysisJob, String> {
    state
        .analysis
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .cancel(request)
}
#[tauri::command]
fn retry_analysis_job(
    state: State<'_, AppState>,
    request: crate::analysis::JobAction,
) -> Result<forum_contracts::AnalysisJob, String> {
    state
        .analysis
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .retry(request)
}
#[tauri::command]
fn revise_artifact(
    state: State<'_, AppState>,
    request: forum_contracts::ArtifactEdit,
) -> Result<forum_contracts::ArtifactRecord, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| s.edit_artifact(&request))
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn review_artifact(
    state: State<'_, AppState>,
    request: forum_contracts::ArtifactReviewCommand,
) -> Result<forum_contracts::ArtifactRecord, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| s.review_artifact(&request))
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn publish_artifact(
    state: State<'_, AppState>,
    request: forum_contracts::ArtifactPublishCommand,
) -> Result<forum_contracts::ArtifactRecord, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| {
            s.publish_artifact(&request)?;
            s.artifact(request.artifact_id)
        })
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn hide_artifact(
    state: State<'_, AppState>,
    request: forum_contracts::ArtifactVisibilityCommand,
) -> Result<forum_contracts::ArtifactRecord, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| s.set_artifact_visibility(&request))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_analysis_evidence(
    state: State<'_, AppState>,
    evidence: forum_contracts::AnalysisEvidence,
) -> Result<serde_json::Value, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| {
            let view = s.resolve_analysis_evidence(&evidence)?;
            let mut output = serde_json::to_value(view)?;
            if let forum_contracts::AnalysisEvidence::Source { session_id, span } = &evidence {
                output["session_id"] = serde_json::to_value(session_id)?;
                output["segment_id"] = serde_json::to_value(span.segment_id)?;
                output["segment_revision"] = serde_json::to_value(span.segment_revision)?;
                output["start_ms"] = serde_json::to_value(
                    s.transcript_revision(*session_id, span.segment_id, span.segment_revision)?
                        .payload
                        .audio
                        .start_ms,
                )?;
            }
            Ok(output)
        })
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn get_analysis_artifact(
    state: State<'_, AppState>,
    artifact_id: forum_contracts::Uuid,
    revision: Option<forum_contracts::Revision>,
) -> Result<forum_contracts::ArtifactRecord, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| match revision {
            Some(revision) => s.artifact_revision(artifact_id, revision),
            None => s.artifact(artifact_id),
        })
        .map_err(|e| e.to_string())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactExportRequest {
    artifact_id: forum_contracts::Uuid,
    expected_revision: forum_contracts::Revision,
    format: String,
}
#[tauri::command]
fn export_artifact(
    state: State<'_, AppState>,
    request: ArtifactExportRequest,
) -> Result<serde_json::Value, String> {
    let format = request.format.clone();
    let (extension, mime) = match format.as_str() {
        "markdown" => ("md", "text/markdown"),
        "html" => ("html", "text/html"),
        "json" => ("json", "application/json"),
        _ => return Err("不支持的导出格式".into()),
    };
    let filename = format!(
        "forum-{}-v{}.{}",
        request.artifact_id,
        request.expected_revision.get(),
        extension
    );
    let content = state
        .runtime
        .repository()?
        .core
        .call(move |s| {
            s.export_artifact(
                request.artifact_id,
                request.expected_revision,
                &request.format,
            )
        })
        .map_err(|e| e.to_string())?;
    // Keep the exported artifact on this Mac even if the webview ignores download.
    let directory = preferences::preferences_dir().join("exports");
    use std::os::unix::fs::DirBuilderExt;
    if !directory.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(|e| e.to_string())?;
    }
    let path = directory.join(&filename);
    crate::analysis::write_private(&path, content.as_bytes())?;
    Ok(
        serde_json::json!({"filename":filename,"mime_type":mime,"content":content,"saved_path":path}),
    )
}
#[tauri::command]
fn get_display_info(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
) -> Result<serde_json::Value, String> {
    state
        .runtime
        .repository()?
        .core
        .call(move |s| s.session_status(session_id))
        .map_err(|e| e.to_string())?;
    let info = state
        .display
        .as_ref()
        .map_err(Clone::clone)?
        .grant(session_id)?;
    let expires = chrono::DateTime::from_timestamp_millis(info.expires_at_ms as i64)
        .ok_or("大屏授权时间无效")?
        .to_rfc3339();
    Ok(serde_json::json!({"displayId":info.display_id,"url":info.url,"expiresAt":expires}))
}
#[tauri::command]
fn revoke_display(
    state: State<'_, AppState>,
    display_id: forum_contracts::Uuid,
) -> Result<(), String> {
    state
        .display
        .as_ref()
        .map_err(Clone::clone)?
        .revoke(display_id);
    Ok(())
}

#[tauri::command]
fn get_public_snapshot(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
    after: Option<u64>,
) -> Result<forum_contracts::PublicSnapshot, String> {
    let _ = after; // Full snapshot replacement is atomic and includes withdrawals.
    state
        .runtime
        .repository()?
        .core
        .call(move |s| s.public_snapshot(session_id))
        .map_err(|e| e.to_string())
}
