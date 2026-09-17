// Included into app.rs. All controls remain Tauri commands, never LAN routes.
#[tauri::command]
fn get_lan_state(state: State<'_, AppState>) -> Result<crate::lan_manager::LanState, String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .state()
}
#[tauri::command]
async fn create_lan_identity(
    state: State<'_, AppState>,
    address: String,
) -> Result<forum_gateway::lan::CertificateInfo, String> {
    let client = state.forum_lan.as_ref().map_err(Clone::clone)?.client();
    let address = address.parse().map_err(|_| "请输入本机局域网 IP 地址")?;
    tauri::async_runtime::spawn_blocking(move || client.certificate(address))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn start_lan(
    state: State<'_, AppState>,
    address: String,
    port: u16,
) -> Result<crate::lan_manager::LanState, String> {
    let client = state.forum_lan.as_ref().map_err(Clone::clone)?.client();
    let address = address.parse().map_err(|_| "请输入本机局域网 IP 地址")?;
    tauri::async_runtime::spawn_blocking(move || client.start(address, port))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn stop_lan(state: State<'_, AppState>) -> Result<crate::lan_manager::LanState, String> {
    let client = state.forum_lan.as_ref().map_err(Clone::clone)?.client();
    tauri::async_runtime::spawn_blocking(move || client.stop())
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn create_participant_access(
    state: State<'_, AppState>,
    request: crate::lan_manager::PublicAccessRequest,
) -> Result<forum_gateway::lan::ParticipantInfo, String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .participant(request)
}
#[tauri::command]
fn create_peer_invite(
    state: State<'_, AppState>,
    request: crate::lan_manager::PublicAccessRequest,
) -> Result<forum_gateway::lan::PeerInvite, String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .invite(request)
}
#[tauri::command]
async fn pair_forum_peer(
    state: State<'_, AppState>,
    request: crate::lan_manager::PairPeerRequest,
) -> Result<forum_contracts::PeerSessionState, String> {
    let client = state.forum_lan.as_ref().map_err(Clone::clone)?.client();
    tauri::async_runtime::spawn_blocking(move || client.pair(request))
        .await
        .map_err(|e| e.to_string())?
}
#[tauri::command]
fn revoke_lan_access(
    state: State<'_, AppState>,
    grant_id: forum_contracts::Uuid,
) -> Result<(), String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .revoke(grant_id)
}
#[tauri::command]
fn disconnect_forum_peer(
    state: State<'_, AppState>,
    owner_device_id: forum_contracts::Uuid,
    session_id: forum_contracts::Uuid,
) -> Result<(), String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .disconnect(owner_device_id, session_id)
}
#[tauri::command]
fn get_public_sessions(
    state: State<'_, AppState>,
    event_id: forum_contracts::Uuid,
    session_ids: Vec<forum_contracts::Uuid>,
) -> Result<Vec<forum_contracts::PublicSessionView>, String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .sessions(event_id, session_ids)
}
#[tauri::command]
fn search_public_content(
    state: State<'_, AppState>,
    event_id: forum_contracts::Uuid,
    session_ids: Vec<forum_contracts::Uuid>,
    query: String,
    limit: u32,
) -> Result<Vec<forum_contracts::PublicSearchHit>, String> {
    state
        .forum_lan
        .as_ref()
        .map_err(Clone::clone)?
        .client()
        .search(event_id, session_ids, query, limit)
}
