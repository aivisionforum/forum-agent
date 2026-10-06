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
        Ok(serde_json::json!({"session_id":session_id,"cursor":jobs.cursor.max(artifacts.cursor),"jobs":jobs.items,"artifacts":artifacts.items,"live_cursor":live_cursor,"live_artifacts":live_artifacts,"notice":notice,"insight_settings":store.insight_settings(session_id)?,"next_jobs":jobs.next_after.map(|k|serde_json::to_string(&k).unwrap()),"next_artifacts":artifacts.next_after.map(|k|serde_json::to_string(&k).unwrap())}))
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
fn prepare_insight_publication(
    state: State<'_, AppState>,
    artifact_id: forum_contracts::Uuid,
    revision: forum_contracts::Revision,
) -> Result<forum_contracts::ArtifactPublishCommand, String> {
    state.runtime.repository()?.core.call(move |s| s.prepare_insight_publication(artifact_id, revision)).map_err(|e| e.to_string())
}

#[tauri::command]
fn approve_and_publish_artifact(
    state: State<'_, AppState>,
    request: forum_contracts::ArtifactPublishCommand,
) -> Result<forum_contracts::ArtifactRecord, String> {
    state.runtime.repository()?.core.call(move |s| s.approve_and_publish_artifact(&request)).map_err(|e| e.to_string())
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
// Public HTTP content never receives the operator IPC capability. These
// navigation intents control only its own window; they cannot read/write a
// meeting, publish an insight, or invoke an arbitrary native command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WallWindowAction { Close, Minimize, Fullscreen, Pin, Drag, State }
fn wall_window_action(url: &tauri::Url) -> Option<WallWindowAction> {
    if url.scheme() != "forum-wall-window" || !url.username().is_empty()
        || url.password().is_some() || url.port().is_some()
        || !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    match url.host_str()? {
        "close" => Some(WallWindowAction::Close),
        "minimize" => Some(WallWindowAction::Minimize),
        "fullscreen" => Some(WallWindowAction::Fullscreen),
        "pin" => Some(WallWindowAction::Pin),
        "drag" => Some(WallWindowAction::Drag),
        "state" => Some(WallWindowAction::State),
        _ => None,
    }
}
fn queue_wall_window_action(app: &tauri::AppHandle, action: WallWindowAction) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window("insight-wall") else { return; };
        let result = match action {
            WallWindowAction::Close => window.hide(),
            WallWindowAction::Minimize => window.minimize(),
            WallWindowAction::Fullscreen => window.is_fullscreen().and_then(|value| window.set_fullscreen(!value)),
            WallWindowAction::Pin => window.is_always_on_top().and_then(|value| window.set_always_on_top(!value)),
            WallWindowAction::Drag => window.start_dragging(),
            WallWindowAction::State => Ok(()),
        };
        let detail = serde_json::json!({
            "pinned":window.is_always_on_top().unwrap_or(false),
            "error":result.err().map(|error|error.to_string()).unwrap_or_default(),
        });
        let _ = window.eval(&format!("window.dispatchEvent(new CustomEvent('audience-window-state', {{detail:{detail}}}));"));
    });
}

#[tauri::command]
async fn show_insight_wall(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
    language: String,
) -> Result<serde_json::Value, String> {
    let info = get_display_info(state, session_id)?;
    let mut url: tauri::Url = info["url"].as_str().ok_or("大屏地址无效")?
        .parse().map_err(|_| "大屏地址无效")?;
    url.query_pairs_mut().append_pair("lang", if language == "en" { "en" } else { "zh" }).append_pair("window", "desktop");
    // The audience window loads the read-only HTTP projection. It is deliberately
    // absent from the operator capability; no private state or command bridge.
    if let Some(window) = app.get_webview_window("insight-wall") {
        window.navigate(url).map_err(|e| e.to_string())?;
        window.show().map_err(|e| e.to_string())?;
        window.unminimize().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
    } else {
        let origin = url.origin();
        let handle = app.clone();
        WebviewWindowBuilder::new(&app, "insight-wall", WebviewUrl::External(url))
            .on_navigation(move |target| {
                if let Some(action) = wall_window_action(target) {
                    queue_wall_window_action(&handle, action);
                    return false;
                }
                target.origin() == origin && target.path() == "/display.html"
            })
            .decorations(false)
            .title(if language == "en" { "Public insights · Screen 2" } else { "公开洞察墙 · 屏幕 2" })
            .inner_size(1180.0, 760.0).min_inner_size(640.0, 480.0)
            .resizable(true).build().map_err(|e| e.to_string())?;
    }
    Ok(info)
}

#[tauri::command]
fn get_insight_settings(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
) -> Result<forum_contracts::InsightSettings, String> {
    state.runtime.repository()?.core.call(move |s| s.insight_settings(session_id)).map_err(|e| e.to_string())
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

#[tauri::command]
fn set_insight_settings(
    state: State<'_, AppState>,
    request: forum_contracts::SetInsightSettings,
) -> Result<forum_contracts::InsightSettings, String> {
    state.runtime.repository()?.core.call(move |s| s.set_insight_settings(&request)).map_err(|e| e.to_string())
}

#[cfg(test)]
mod wall_window_tests {
    use super::*;
    #[test]
    fn public_window_intents_are_exact_and_never_route_operator_commands() {
        for (name, expected) in [
            ("close",WallWindowAction::Close), ("minimize",WallWindowAction::Minimize),
            ("fullscreen",WallWindowAction::Fullscreen), ("pin",WallWindowAction::Pin),
            ("drag",WallWindowAction::Drag), ("state",WallWindowAction::State),
        ] {
            assert_eq!(wall_window_action(&format!("forum-wall-window://{name}").parse().unwrap()), Some(expected));
        }
        for url in [
            "https://pin", "forum-wall-window://publish_artifact", "forum-wall-window://get_overlay_state",
            "forum-wall-window://pin?window=main", "forum-wall-window://pin/main",
            "forum-wall-window://pin#javascript", "forum-wall-window://user@pin",
            "forum-wall-window://pin:1234", "forum-wall-window://pin.example.com",
        ] {
            assert_eq!(wall_window_action(&url.parse().unwrap()), None, "{url}");
        }
    }
}
