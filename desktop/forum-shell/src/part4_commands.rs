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

#[tauri::command]
fn start_closing_readout(
    state: State<'_, AppState>,
    artifact_id: forum_contracts::Uuid,
    expected_revision: forum_contracts::Revision,
    language: Option<String>,
) -> Result<(), String> {
    let _activity = state.activity_gate.lock();
    if state.exiting.load(std::sync::atomic::Ordering::Acquire)
        || state.runtime_state.lock().running
    {
        return Err("请先停止采集，待音频设备释放后朗读闭幕稿".into());
    }
    let progress = state.runtime.shared_state().capture_progress.read();
    if progress.started && !progress.devices_released {
        return Err("音频采集设备尚未确认释放".into());
    }
    let public = state
        .runtime
        .repository()?
        .core
        .call(move |s| s.public_artifact_for_revision(artifact_id, expected_revision))
        .map_err(|e| e.to_string())?;
    let settings = TranslationSettings::from(&*state.preferences.lock());
    let language = language.unwrap_or_else(|| "zh".into());
    if !matches!(language.as_str(), "zh" | "en") {
        return Err("闭幕朗读当前支持中文和英文，请选择已安装对应音色的语言".into());
    }
    let voice = settings
        .spoken_translation_voice
        .as_deref()
        .unwrap_or("apple-voice-1");
    apple_speech::ensure_voice_available(&language, voice)?;
    state.stop_closing_readout()?;
    stop_preview_process(&state);
    state.closing_speech.configure(
        true,
        &language,
        voice,
        settings.spoken_translation_output_device.as_deref(),
    );
    *state.closing_readout.lock() = Some((artifact_id, expected_revision));
    // The projection is independently reviewed; never read model draft/source text.
    state.closing_speech.speak(public.text);
    Ok(())
}

#[tauri::command]
fn stop_closing_readout(state: State<'_, AppState>) -> Result<(), String> {
    let _activity = state.activity_gate.lock();
    state.stop_closing_readout()
}

impl AppState {
    fn stop_closing_readout(&self) -> Result<(), String> {
        // Keep the identity on a timeout so the capture guard cannot forget an
        // audio output stream whose release has not been acknowledged.
        let mut current = self.closing_readout.lock();
        if current.is_some() {
            self.closing_speech.stop_and_wait()?;
            *current = None;
        }
        Ok(())
    }
    fn check_closing_publication(&self) {
        let _activity = self.activity_gate.lock();
        let current = *self.closing_readout.lock();
        if let Some((id, revision)) = current {
            let valid = self.runtime.repository().and_then(|repo| {
                repo.core
                    .call(move |s| s.public_artifact_for_revision(id, revision))
                    .map_err(|e| e.to_string())
            });
            if valid.is_err() {
                if let Err(error) = self.stop_closing_readout() {
                    log::error!("Cannot stop withdrawn closing readout: {error}");
                }
            }
        }
    }
}
