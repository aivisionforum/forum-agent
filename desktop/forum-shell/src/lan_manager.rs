//! Desktop-only LAN coordinator. No server starts and no capability is restored automatically.
use forum_contracts::{PeerSession, PeerSessionState, PublicSearchHit, PublicSessionView, Uuid};
use forum_core::CoreHandle;
use forum_gateway::{
    lan::{
        CertificateInfo, LanGateway, LanIdentity, LanReaders, ParticipantInfo, PeerClient,
        PeerInvite,
    },
    Asset,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::IpAddr,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[derive(Debug, Serialize)]
pub struct LanState {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub certificate: Option<CertificateInfo>,
    pub peers: Vec<PeerSessionState>,
    pub grants: Vec<forum_gateway::lan::LanGrantInfo>,
    pub notice: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicAccessRequest {
    pub session_id: Uuid,
    pub public_title: String,
    pub room_name: String,
    pub ttl_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairPeerRequest {
    pub invite: PeerInvite,
    pub confirmed_fingerprint: String,
}
struct Peer {
    client: Arc<PeerClient>,
}
struct State {
    gateway: Option<LanGateway>,
    certificate: Option<CertificateInfo>,
    peers: HashMap<(Uuid, Uuid), Peer>,
    notice: Option<String>,
    generation: u64,
}
struct Inner {
    core: CoreHandle,
    directory: PathBuf,
    device: Uuid,
    assets: Vec<Asset>,
    state: Mutex<State>,
    shutdown: AtomicBool,
    provisioning: AtomicUsize,
}
struct Provision<'a>(&'a AtomicUsize);
impl Drop for Provision<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct LanManager {
    inner: Arc<Inner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
#[derive(Clone)]
pub struct LanClient {
    inner: Arc<Inner>,
}
impl LanManager {
    pub fn new(
        core: CoreHandle,
        directory: PathBuf,
        identity: [Uuid; 3],
        assets: Vec<Asset>,
    ) -> Result<Self, String> {
        core.call(|s| s.disconnect_all_peers())
            .map_err(|e| e.to_string())?;
        let certificate = load_certificate(&directory);
        let inner = Arc::new(Inner {
            core,
            directory,
            device: identity[0],
            assets,
            state: Mutex::new(State {
                gateway: None,
                certificate,
                peers: HashMap::new(),
                notice: None,
                generation: 0,
            }),
            shutdown: AtomicBool::new(false),
            provisioning: AtomicUsize::new(0),
        });
        let worker_inner = inner.clone();
        let worker = thread::spawn(move || {
            while !worker_inner.shutdown.load(Ordering::Acquire) {
                let peers = worker_inner
                    .state
                    .lock()
                    .map(|s| {
                        s.peers
                            .values()
                            .map(|p| p.client.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for peer in peers {
                    if worker_inner.shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    sync_peer(&worker_inner, &peer);
                }
                for _ in 0..20 {
                    if worker_inner.shutdown.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        });
        Ok(Self {
            inner,
            worker: Mutex::new(Some(worker)),
        })
    }
    pub fn client(&self) -> LanClient {
        LanClient {
            inner: self.inner.clone(),
        }
    }
    pub fn shutdown(&self) {
        self.inner.shutdown.store(true, Ordering::Release);
        let _ = self.client().stop();
    }
    pub fn shutdown_complete(&self) -> bool {
        self.inner.provisioning.load(Ordering::Acquire) == 0
            && self
                .worker
                .lock()
                .is_ok_and(|w| w.as_ref().is_none_or(JoinHandle::is_finished))
    }
}
impl Drop for LanManager {
    fn drop(&mut self) {
        self.shutdown();
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}
impl LanClient {
    fn active(&self) -> Result<(), String> {
        if self.inner.shutdown.load(Ordering::Acquire) {
            Err("应用正在退出".into())
        } else {
            Ok(())
        }
    }
    pub fn state(&self) -> Result<LanState, String> {
        let state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        let peers = self
            .inner
            .core
            .call(|s| s.all_peer_sessions())
            .map_err(|e| e.to_string())?;
        Ok(LanState {
            enabled: state.gateway.is_some(),
            endpoint: state.gateway.as_ref().map(|g| g.endpoint.clone()),
            certificate: state.certificate.clone(),
            peers,
            grants: state
                .gateway
                .as_ref()
                .map(|gateway| gateway.grants())
                .unwrap_or_default(),
            notice: state.notice.clone(),
        })
    }
    pub fn certificate(&self, address: IpAddr) -> Result<CertificateInfo, String> {
        self.active()?;
        self.inner.provisioning.fetch_add(1, Ordering::AcqRel);
        let _provision = Provision(&self.inner.provisioning);
        self.active()?;
        let state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        if state.gateway.is_some() {
            return Err("请先关闭局域网，再更换证书。".into());
        }
        let generation = state.generation;
        drop(state);
        let parent = self.inner.directory.join("lan-certificates");
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        let identity = LanIdentity::generate_cancellable(
            &parent.join(Uuid::new_v4().to_string()),
            address,
            &|| self.inner.shutdown.load(Ordering::Acquire),
        )
        .map_err(|e| e.to_string())?;
        let info = identity.info;
        let mut state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        self.active()?;
        if state.generation != generation || state.gateway.is_some() {
            return Err("共享状态已改变，请重新准备证书。".into());
        }
        save_certificate(&parent, &info)?;
        state.certificate = Some(info.clone());
        state.generation += 1;
        Ok(info)
    }
    pub fn start(&self, address: IpAddr, port: u16) -> Result<LanState, String> {
        self.active()?;
        let mut state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        if state.gateway.is_some() {
            return Err("局域网服务已经启用".into());
        }
        let cert = state
            .certificate
            .as_ref()
            .ok_or("请先为此 IP 生成证书，并完成手机的系统信任安装。")?;
        if cert.address != address {
            return Err("IP 与证书不匹配，请重新生成。".into());
        }
        let identity = LanIdentity::load(&cert.directory, address).map_err(|e| e.to_string())?;
        let core = self.inner.core.clone();
        let change_core = core.clone();
        let readers = LanReaders {
            snapshot: Arc::new(move |id| {
                core.call(move |s| s.public_snapshot(id))
                    .map_err(|e| e.to_string())
            }),
            changes: Arc::new(move |id, after| {
                change_core
                    .call(move |s| {
                        let changes = s.public_changes(id, after, 1000)?;
                        let cursor = if changes.len() == 1000 {
                            changes.last().map(|c| c.publication_seq).unwrap_or(after)
                        } else {
                            s.public_snapshot(id)?.cursor
                        };
                        Ok((cursor, changes))
                    })
                    .map_err(|e| e.to_string())
            }),
        };
        state.gateway = Some(
            LanGateway::start(identity, port, readers, self.inner.assets.clone())
                .map_err(|e| e.to_string())?,
        );
        state.notice = None;
        state.generation += 1;
        drop(state);
        self.state()
    }
    pub fn stop(&self) -> Result<LanState, String> {
        let gateway = {
            let mut state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
            let keys = state.peers.keys().copied().collect::<Vec<_>>();
            state.peers.clear();
            state.generation += 1;
            let gateway = state.gateway.take();
            for (owner, session) in keys {
                self.inner
                    .core
                    .call(move |s| s.mark_peer_disconnected(owner, session))
                    .map_err(|e| e.to_string())?;
            }
            gateway
        };
        drop(gateway);
        self.state()
    }
    fn source(&self, request: &PublicAccessRequest) -> Result<PeerSession, String> {
        if request.public_title.trim().is_empty() || request.room_name.trim().is_empty() {
            return Err("请明确填写允许公开的场次名和会场名。".into());
        }
        let id = request.session_id;
        let spec = self
            .inner
            .core
            .call(move |s| Ok(s.session_snapshot(id)?.session))
            .map_err(|e| e.to_string())?;
        if spec.owner_device_id != self.inner.device {
            return Err("只能为本机拥有的场次创建访问权限。".into());
        }
        Ok(PeerSession {
            owner_device_id: spec.owner_device_id,
            event_id: spec.event_id,
            session_id: spec.session_id,
            title: request.public_title.trim().into(),
            room_name: request.room_name.trim().into(),
        })
    }
    pub fn participant(&self, request: PublicAccessRequest) -> Result<ParticipantInfo, String> {
        self.active()?;
        let source = self.source(&request)?;
        let state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        state
            .gateway
            .as_ref()
            .ok_or("请先显式启用局域网")?
            .participant(source, request.ttl_ms.unwrap_or(60 * 60 * 1000))
            .map_err(|e| e.to_string())
    }
    pub fn invite(&self, request: PublicAccessRequest) -> Result<PeerInvite, String> {
        self.active()?;
        let source = self.source(&request)?;
        let state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        state
            .gateway
            .as_ref()
            .ok_or("请先显式启用局域网")?
            .invite(source)
            .map_err(|e| e.to_string())
    }
    pub fn revoke(&self, id: Uuid) -> Result<(), String> {
        if let Some(gateway) = self
            .inner
            .state
            .lock()
            .map_err(|_| "LAN 状态不可用")?
            .gateway
            .as_ref()
        {
            gateway.revoke(id)
        }
        Ok(())
    }
    pub fn pair(&self, request: PairPeerRequest) -> Result<PeerSessionState, String> {
        self.active()?;
        if request.invite.source.owner_device_id == self.inner.device {
            return Err("不能配对本机作为另一会场。".into());
        }
        let generation = {
            let state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
            if state.peers.len() >= 8 {
                return Err("当前最多连接 8 个会场。".into());
            }
            state.generation
        };
        let client = Arc::new(
            PeerClient::pair(
                &request.invite,
                &request.confirmed_fingerprint,
                self.inner.device,
            )
            .map_err(|e| e.to_string())?,
        );
        let source = client.lease.source.clone();
        let snapshot = client.snapshot().map_err(|e| e.to_string())?;
        let mut state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        self.active()?;
        if state.generation != generation {
            return Err("局域网状态已改变，请重新配对。".into());
        }
        if state.peers.len() >= 8 {
            return Err("当前最多连接 8 个会场。".into());
        }
        let key = (source.owner_device_id, source.session_id);
        let result = self
            .inner
            .core
            .call(move |s| {
                s.register_peer_session(&source)?;
                s.apply_peer_snapshot(&snapshot)
            })
            .map_err(|e| e.to_string());
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // An actor timeout has an unknown commit outcome. Queue a stale
                // fence after the attempted import before abandoning the lease.
                let _ = self
                    .inner
                    .core
                    .call(move |s| s.mark_peer_disconnected(key.0, key.1));
                return Err(error);
            }
        };
        state.peers.insert(key, Peer { client });
        state.notice = None;
        Ok(result)
    }
    pub fn disconnect(&self, owner: Uuid, session: Uuid) -> Result<(), String> {
        let mut state = self.inner.state.lock().map_err(|_| "LAN 状态不可用")?;
        state.peers.remove(&(owner, session));
        state.generation += 1;
        self.inner
            .core
            .call(move |s| s.mark_peer_disconnected(owner, session))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn sessions(&self, event: Uuid, ids: Vec<Uuid>) -> Result<Vec<PublicSessionView>, String> {
        self.inner
            .core
            .call(move |s| s.public_sessions(event, &ids))
            .map_err(|e| e.to_string())
    }
    pub fn search(
        &self,
        event: Uuid,
        ids: Vec<Uuid>,
        query: String,
        limit: u32,
    ) -> Result<Vec<PublicSearchHit>, String> {
        self.inner
            .core
            .call(move |s| s.search_public(event, &ids, &query, limit))
            .map_err(|e| e.to_string())
    }
}
fn load_certificate(directory: &std::path::Path) -> Option<CertificateInfo> {
    let parent = directory.join("lan-certificates");
    let path = parent.join("current.json");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16384 {
        return None;
    }
    let saved: CertificateInfo = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if saved.directory.parent() != Some(parent.as_path())
        || saved
            .directory
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.parse::<Uuid>().ok())
            .is_none()
    {
        return None;
    }
    let loaded = LanIdentity::load(&saved.directory, saved.address)
        .ok()?
        .info;
    (loaded.certificate_sha256 == saved.certificate_sha256 && loaded.ca_sha256 == saved.ca_sha256)
        .then_some(loaded)
}
fn save_certificate(parent: &std::path::Path, info: &CertificateInfo) -> Result<(), String> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let temporary = parent.join(format!(".current-{}.json", Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(info).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, parent.join("current.json")).map_err(|e| e.to_string())?;
    Ok(())
}
fn current(state: &State, peer: &Arc<PeerClient>) -> bool {
    state
        .peers
        .get(&(
            peer.lease.source.owner_device_id,
            peer.lease.source.session_id,
        ))
        .is_some_and(|p| Arc::ptr_eq(&p.client, peer))
}
fn sync_peer(inner: &Arc<Inner>, peer: &Arc<PeerClient>) {
    let owner = peer.lease.source.owner_device_id;
    let session = peer.lease.source.session_id;
    let before = inner
        .core
        .call(move |s| s.peer_public_snapshot(owner, session));
    let Ok(before) = before else { return };
    let result = peer.changes(before.cursor);
    let mut state = match inner.state.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if inner.shutdown.load(Ordering::Acquire) || !current(&state, peer) {
        return;
    }
    let apply = result.and_then(|batch| {
        inner
            .core
            .call(move |s| s.apply_peer_changes(&batch))
            .map_err(anyhow::Error::from)
    });
    if apply.is_ok() {
        state.notice = None;
        return;
    }
    drop(state);
    // Any gap or expired cursor requires an authoritative snapshot. Check that the
    // connection is still current both before requesting and before committing.
    if inner.shutdown.load(Ordering::Acquire)
        || !inner.state.lock().is_ok_and(|s| current(&s, peer))
    {
        return;
    }
    let snapshot = peer.snapshot();
    state = match inner.state.lock() {
        Ok(s) => s,
        Err(_) => return,
    };
    if inner.shutdown.load(Ordering::Acquire) || !current(&state, peer) {
        return;
    }
    let result = snapshot.and_then(|snapshot| {
        inner
            .core
            .call(move |s| s.apply_peer_snapshot(&snapshot))
            .map_err(anyhow::Error::from)
    });
    if result.is_err() {
        let _ = inner
            .core
            .call(move |s| s.mark_peer_disconnected(owner, session));
        state.notice =
            Some("会场连接已中断。保留的公开内容标记为离线；恢复同步后才会更新撤回状态。".into());
    } else {
        state.notice = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forum_contracts::SessionSpec;
    fn temporary() -> PathBuf {
        let path = std::env::temp_dir().join(format!("forum-lan-manager-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn explicit_two_core_pairing_revoke_event_scope_and_restart() {
        let path_a = temporary();
        let path_b = temporary();
        let core_a = CoreHandle::open(path_a.join("store.sqlite"), 32).unwrap();
        let core_b = CoreHandle::open(path_b.join("store.sqlite"), 32).unwrap();
        let owner_a = Uuid::new_v4();
        let owner_b = Uuid::new_v4();
        let old_event = Uuid::new_v4();
        let future_event = Uuid::new_v4();
        let source = SessionSpec {
            session_id: Uuid::new_v4(),
            event_id: future_event,
            room_id: Uuid::new_v4(),
            owner_device_id: owner_a,
            title: "PRIVATE SESSION TITLE — NEVER SHARE".into(),
        };
        let copy = source.clone();
        core_a.call(move |s| s.create_session(&copy)).unwrap();
        let a = LanManager::new(
            core_a.clone(),
            path_a.clone(),
            [owner_a, old_event, source.room_id],
            vec![],
        )
        .unwrap();
        let b = LanManager::new(
            core_b.clone(),
            path_b.clone(),
            [owner_b, Uuid::new_v4(), Uuid::new_v4()],
            vec![],
        )
        .unwrap();
        assert!(!a.client().state().unwrap().enabled);
        assert!(!b.client().state().unwrap().enabled);
        let ip = "127.0.0.1".parse().unwrap();
        let cert = a.client().certificate(ip).unwrap();
        a.client().start(ip, 0).unwrap();
        let access = || PublicAccessRequest {
            session_id: source.session_id,
            public_title: "Reviewed public title".into(),
            room_name: "Room A".into(),
            ttl_ms: Some(60000),
        };
        let invite = a.client().invite(access()).unwrap();
        assert_eq!(invite.source.event_id, future_event);
        assert_eq!(invite.source.title, "Reviewed public title");
        let remote = b
            .client()
            .pair(PairPeerRequest {
                confirmed_fingerprint: invite.certificate_sha256.clone(),
                invite: invite.clone(),
            })
            .unwrap();
        assert!(!remote.stale);
        assert_eq!(remote.session.owner_device_id, owner_a);
        let visible = b
            .client()
            .sessions(future_event, vec![source.session_id])
            .unwrap();
        assert_eq!(visible.len(), 1);
        assert!(!serde_json::to_string(&visible)
            .unwrap()
            .contains("PRIVATE SESSION"));
        assert!(b
            .client()
            .sessions(old_event, vec![source.session_id])
            .is_err());
        a.client().revoke(invite.invite_id).unwrap();
        let peer = b
            .inner
            .state
            .lock()
            .unwrap()
            .peers
            .values()
            .next()
            .unwrap()
            .client
            .clone();
        sync_peer(&b.inner, &peer);
        assert!(b.client().state().unwrap().peers[0].stale);
        let next = a.client().invite(access()).unwrap();
        b.client()
            .pair(PairPeerRequest {
                confirmed_fingerprint: next.certificate_sha256.clone(),
                invite: next,
            })
            .unwrap();
        assert!(!b.client().state().unwrap().peers[0].stale);
        b.client().disconnect(owner_a, source.session_id).unwrap();
        sync_peer(&b.inner, &peer);
        assert!(b.client().state().unwrap().peers[0].stale);
        a.client().stop().unwrap();
        drop(a);
        let restarted = LanManager::new(
            core_a.clone(),
            path_a.clone(),
            [owner_a, old_event, source.room_id],
            vec![],
        )
        .unwrap();
        let state = restarted.client().state().unwrap();
        assert!(!state.enabled);
        assert_eq!(
            state.certificate.unwrap().certificate_sha256,
            cert.certificate_sha256
        );
        drop(restarted);
        drop(b);
        drop(core_a);
        drop(core_b);
        std::fs::remove_dir_all(path_a).unwrap();
        std::fs::remove_dir_all(path_b).unwrap();
    }
}
