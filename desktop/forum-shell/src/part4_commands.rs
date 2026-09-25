// Included into app.rs: local operator commands only.
#[tauri::command]
fn get_speaker_status(
    state: State<'_, AppState>,
) -> Result<crate::speaker_manager::SpeakerStatus, String> {
    Ok(state
        .speakers
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .status())
}
#[tauri::command]
fn get_speaker_assignments(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
) -> Result<Vec<forum_contracts::SpeakerAssignment>, String> {
    state
        .speakers
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .list(session_id)
}
#[tauri::command]
fn enable_session_speakers(
    state: State<'_, AppState>,
    session_id: forum_contracts::Uuid,
    model_path: Option<PathBuf>,
) -> Result<crate::speaker_manager::SpeakerStatus, String> {
    let _activity = state.activity_gate.lock();
    if state.exiting.load(std::sync::atomic::Ordering::Acquire) {
        return Err("应用正在退出".into());
    }
    state
        .speakers
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .enable(session_id, model_path)
}
#[tauri::command]
fn disable_session_speakers(
    state: State<'_, AppState>,
) -> Result<crate::speaker_manager::SpeakerStatus, String> {
    Ok(state
        .speakers
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .disable())
}
#[tauri::command]
fn correct_speaker_assignment(
    state: State<'_, AppState>,
    command: forum_contracts::SpeakerAssignmentCommand,
) -> Result<forum_contracts::SpeakerAssignment, String> {
    state
        .speakers
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .correct(command)
}

#[tauri::command]
fn join_forum_event(
    state: State<'_, AppState>,
    event_id: forum_contracts::Uuid,
) -> Result<(), String> {
    let _activity = state.activity_gate.lock();
    if state.exiting.load(std::sync::atomic::Ordering::Acquire)
        || state.runtime_state.lock().running
    {
        return Err("请在采集完全停止后加入活动；现有场次的活动归属不会改变".into());
    }
    state.runtime.repository()?.join_event(event_id)
}
